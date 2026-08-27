mod sidecar_lock;
use sidecar_lock::SidecarLock;

mod auto_run;

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{hash_map::DefaultHasher, BTreeMap, HashMap},
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{self, Read},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use rayon::prelude::*;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Listener, LogicalPosition, LogicalSize, Manager, Position, Size,
    WebviewWindow,
};
use tauri_plugin_notification::NotificationExt;
use windows::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS};

mod nsfw;
use nsfw::{analyze_image_nsfw, create_session};

mod ocr;
use ocr::{analyze_image_text, extract_image_text};

mod thumbnails;
use thumbnails::{ensure_thumbnail, THUMBNAIL_DIR_NAME};

mod chunker;
use chunker::{build_plan, clean_title, ChunkPlan};

mod vision;
use vision::{build_agent, describe_image, list_models, warm_model, DESCRIBE_PROMPT};

mod model_lease;
use model_lease::{IdleLeaseStatus, ModelLease};

// Holds the nightly vision pass back while somebody else is using the GPU. Only the headless job
// consults it — an interactive Describe run is the user asking for the GPU on purpose.
mod gpu_gate;

mod geo;

mod kinds;

mod review;

// Reads screenshot-tool's capture log — how many screenshots were TAKEN, as opposed to how many
// are still in the library. The two are never reconciled; see the module docs.
mod capture_log;

// Public because `src/bin/icat.rs` — the agent-facing CLI — links this crate as a library and needs
// to reach `text_cli::run`. The rest of the app treats them like any other module.
pub mod redact;
pub mod text_cli;
pub mod text_index;
pub mod topics;

const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "webp", "tiff", "tif", "heic", "heif"];
const SIDECAR_FILE_NAME: &str = ".image-categorizer.json";
const MAX_SCAN_DEPTH: usize = 4;
const HASH_SAMPLE_BYTES: usize = 65536;

// How deep `import_images` walks into a dropped folder, and how many per-file copy failures it
// reports back before it stops collecting them (the count still reflects every failure).
const MAX_IMPORT_DEPTH: usize = 8;
const MAX_IMPORT_ERRORS: usize = 5;

// How many analyzed images an in-flight pass buffers before merging them to disk. Small enough that
// a crash costs seconds of work rather than hours; large enough that rewriting the sidecar (~10MB on
// a 20k library) stays a rounding error next to the OCR/NSFW inference it sits between.
const ANALYSIS_CHECKPOINT_EVERY: usize = 250;

// Extracted OCR text is written here, one `<hash>.txt` per image, so the folder stays stable
// across renames/moves and dedupes identical images — same keying scheme as the thumbnail cache.
const OCR_TEXT_DIR_NAME: &str = ".image-categorizer-ocr-text";

// Video-chunking + vision-description feature (all keyed by content hash, same as the caches above).
// The chunk plan is a standalone, hand-editable file so it can be reviewed or discarded by itself
// without touching the main sidecar. Vision descriptions land one `<hash>.json` (+ `<hash>.txt`) per
// image under the descriptions dir, with an `index.json` mapping relative path -> hash so other apps
// can look a description up by the image file they hold.
const CHUNK_PLAN_FILE_NAME: &str = ".image-categorizer-chunks.json";
const VISION_DESC_DIR_NAME: &str = ".image-categorizer-descriptions";
const VISION_INDEX_FILE_NAME: &str = "index.json";
const VISION_DESC_SCHEMA_VERSION: u32 = 1;
const VISION_PROMPT_VERSION: u32 = 1;

// Fraction of the frame height OCRed for the title bar, and how many frames per confirmed video the
// chunk plan samples for the vision pass. Fixed defaults for this first version.
const TITLE_STRIP_TOP_FRACTION: f32 = 0.06;
const DEFAULT_SAMPLES_PER_GROUP: u32 = 10;

// How many leading images may fail at the vision endpoint with nothing yet described before the
// Describe pass gives up and reports the endpoint's own error (no model loaded / bad token / down).
const VISION_FAIL_FAST_ATTEMPTS: usize = 3;

// How much of a description the scene classifier sees. Enough to cover the actual scene prose after
// the video title is stripped, without bloating a 20-item batch prompt.
const KIND_SCENE_MAX_CHARS: usize = 700;

const DEFAULT_VISION_ENDPOINT: &str = "http://localhost:1234/v1/chat/completions";
const DEFAULT_VISION_MODEL: &str = "local-model";

const DEFAULT_OCR_WORD_THRESHOLD: u32 = 35;
const DEFAULT_OCR_AREA_THRESHOLD: f32 = 0.05;
const LOW_TEXT_CATEGORY: &str = "Low Text";
const HIGH_TEXT_CATEGORY: &str = "High Text";

const DEFAULT_NSFW_THRESHOLD: f32 = 0.45;
const EXPLICIT_CATEGORY: &str = "Explicit";
const ROOT_SOURCE_FOLDER: &str = "Root";
const NUDENET_MODEL_DOWNLOAD_URL: &str =
    "https://files.pythonhosted.org/packages/1c/ee/1aa02d44ba958cc77e16ff1e41a0aac5e721037db7bf62b9c9d124917f87/nudenet-3.4.2-py3-none-any.whl";
const NUDENET_MODEL_DOWNLOAD_FILENAME: &str = "320n.onnx";
const NUDENET_MODEL_FILENAMES: &[&str] = &["320n.onnx", "nudenet-320n.onnx", "nudenet.onnx"];

// Passed on the command line by the Windows Task Scheduler entry that `set_auto_refresh_settings`
// installs/removes. When present, `run()` skips creating any window entirely (see `run_headless_refresh`)
// so the nightly job never flashes UI or fights the GUI's own startup scan for the sidecar file.
const HEADLESS_REFRESH_ARG: &str = "--headless-refresh";
const AUTO_REFRESH_TASK_NAME: &str = "ImageCategorizerAutoRefresh";
const DEFAULT_AUTO_REFRESH_TIME: &str = "04:00";

/// How long the nightly description pass may hold the GPU before stopping itself and leaving the
/// rest for the next night.
///
/// The pass used to run until the backlog was empty, and the scheduled task sets
/// `ExecutionTimeLimit=PT0S`, so nothing bounded it at all. On this machine that is not a
/// theoretical concern: ~60,000 images still needed describing at roughly 12/minute, which is some
/// 80 hours of continuous GPU time — a "nightly" job that would still be saturating the card in the
/// middle of the next working day. A bounded slice per night converges on the same backlog without
/// ever being the reason the machine is busy.
const DEFAULT_VISION_LIMIT_MINUTES: u32 = 30;

/// Upper bound on the configured limit — a day. Past this the setting is indistinguishable from the
/// unlimited option it already has (`0`), and a typo'd four-digit number should not silently mean
/// "most of a week".
const MAX_VISION_LIMIT_MINUTES: u32 = 24 * 60;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppSettings {
    last_root: Option<String>,
    tile_size: Option<u32>,
    dark_mode: Option<bool>,
    #[serde(default)]
    known_roots: Vec<String>,
    #[serde(default)]
    auto_refresh_enabled: bool,
    auto_refresh_time: Option<String>,
    #[serde(default)]
    auto_refresh_roots: Vec<String>,
    auto_refresh_nsfw: Option<bool>,
    auto_refresh_text_analysis: Option<bool>,
    auto_refresh_text_extraction: Option<bool>,
    // The local-LLM description pass. `None` = off: it is the only pass that needs the GPU and a
    // running model server, so unlike the others it is not something to switch on for someone by
    // default. `auto_refresh_gpu_wait` (default on) holds it back while the card is in use.
    auto_refresh_vision: Option<bool>,
    // How long the nightly description pass may hold the GPU, in minutes; `0` = until the backlog
    // is done. See `DEFAULT_VISION_LIMIT_MINUTES` for why this defaults to a limit rather than to
    // the old unbounded behaviour.
    auto_refresh_vision_minutes: Option<u32>,
    auto_refresh_gpu_wait: Option<bool>,
    auto_refresh_low_priority: Option<bool>,
    auto_refresh_toast: Option<bool>,
    last_auto_refresh_at: Option<String>,
    last_auto_refresh_summary: Option<String>,
    // OpenAI-compatible vision endpoint (LM Studio by default) + the model name to send. Global, so
    // one setting drives the description pass across every library. `vision_api_key` is the bearer
    // token LM Studio requires when its API-token auth is enabled; `None` = send no Authorization.
    vision_endpoint: Option<String>,
    vision_model: Option<String>,
    vision_api_key: Option<String>,
    // Idle lease over the model (see `model_lease`): when this app is the one that loaded the
    // model, let it unload after `vision_idle_minutes` with no LM Studio traffic from anyone and no
    // activity in this window. `None` on both = the defaults (on, 5 minutes).
    vision_idle_unload: Option<bool>,
    vision_idle_minutes: Option<u32>,
    // Where the window opens, saved by hand from Settings rather than tracked on every move: the
    // point is a chosen default, so a stray resize before closing must not overwrite it. `None` =
    // no preference, i.e. the size in tauri.conf.json wherever Windows decides to put it.
    window_bounds: Option<WindowBounds>,
    // Saved while the window was maximized. The bounds are kept alongside it (the size to come back
    // to when it is restored down), which is why this is a flag and not a third variant.
    #[serde(default)]
    window_maximized: bool,
}

/// A saved window rect in LOGICAL pixels. Physical pixels are DPI-dependent, so a rect saved on a
/// 150%-scaled monitor would reopen at the wrong visual size on a 100% one.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowBounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageRecord {
    last_known_path: String,
    category: Option<String>,
    classified_by: Option<String>,
    classified_at: Option<String>,
    ocr_word_count: Option<u32>,
    ocr_text_area_ratio: Option<f32>,
    // Number of characters of OCR text extracted to the sidecar text folder. `Some` (including
    // `Some(0)` for images with no text) marks the image as already extracted; `None` means it
    // still needs an extraction pass — mirrors the "already done" gating of the other scans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ocr_text_chars: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nsfw_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nsfw_labels: Option<Vec<String>>,
    // Video-chunking: the title read from this image's top strip. `None` = title strip not scanned
    // yet (so it's pending); `Some("")` = scanned, no video marker found (a normal standalone image);
    // `Some("Driving across…")` = a confirmed video frame with that title. Mirrors the `Some(0)`
    // "done but empty" convention `ocr_text_chars` uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    video_title: Option<String>,
    // Vision pass: character count of the saved description. `Some` (including `Some(0)`) marks the
    // image as already described; the prose itself lives in the descriptions sidecar folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vision_desc_chars: Option<u32>,
    // Size and mtime of the file at `last_known_path` when its hash was last computed. A scan
    // reuses the stored hash whenever both still match, so unchanged files are never re-read —
    // see `hash_index`. Absent on records written before this cache existed; those re-hash once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    modified_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryConfig {
    #[serde(default)]
    version: u32,
    source_pattern_preset: Option<String>,
    source_pattern_regex: Option<String>,
    #[serde(default)]
    manual_source_folders: Vec<String>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    images: HashMap<String, ImageRecord>,
    ocr_word_threshold: Option<u32>,
    ocr_area_threshold: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nsfw_score_threshold: Option<f32>,
    #[serde(default)]
    excluded_analysis_folders: Vec<String>,
    #[serde(default)]
    excluded_analysis_categories: Vec<String>,
    // Which categories the text index covers. `None` means the default (High Text alone) — the pool
    // the extraction pass is actually aimed at. Stored rather than hardcoded so widening it is a
    // config edit, but left unset by default so the honest default is visible in code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_index_categories: Option<Vec<String>>,
    // Source folders kept OUT of the text index. A denylist rather than an allowlist: everything
    // already in the library is indexed, and opting a folder out is the rare case.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    text_index_excluded_folders: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppSettingsView {
    last_root: Option<String>,
    last_root_exists: bool,
    tile_size: u32,
    dark_mode: bool,
    known_roots: Vec<KnownRootView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnownRootView {
    path: String,
    exists: bool,
}

/// Both halves of the Settings row: what is stored, and what the window is doing right now — so the
/// panel can say "this is what you would be saving" without the frontend measuring the window
/// itself (the webview knows its own viewport, not the window's outer rect on the desktop).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowDefaultsView {
    saved: Option<WindowBounds>,
    saved_maximized: bool,
    current: Option<WindowBounds>,
    current_maximized: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AutoRefreshSettingsView {
    enabled: bool,
    time: String,
    roots: Vec<String>,
    run_nsfw: bool,
    run_text_analysis: bool,
    run_text_extraction: bool,
    run_vision: bool,
    vision_minutes: u32,
    gpu_wait: bool,
    low_priority: bool,
    toast: bool,
    task_installed: bool,
    last_run_at: Option<String>,
    last_run_summary: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceFolderView {
    name: String,
    relative_path: String,
    is_manual: bool,
    image_count: usize,
    included_in_analysis: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CategoryView {
    name: String,
    count: usize,
    included_in_analysis: bool,
}

/// What `assign_category` stamped on the record, so the frontend can mirror it without a rescan.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssignResult {
    classified_by: Option<String>,
    classified_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportReport {
    imported: usize,
    skipped: usize,
    target_folder: String,
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageView {
    hash: String,
    path: String,
    thumbnail_path: Option<String>,
    relative_path: String,
    name: String,
    source_folder: String,
    size: u64,
    modified_ms: u64,
    category: Option<String>,
    classified_by: Option<String>,
    classified_at: Option<String>,
    ocr_word_count: Option<u32>,
    ocr_text_area_ratio: Option<f32>,
    ocr_text_chars: Option<u32>,
    nsfw_score: Option<f32>,
    nsfw_labels: Option<Vec<String>>,
    // Non-empty when this frame was identified as belonging to a video of this title.
    video_title: Option<String>,
    vision_desc_chars: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryView {
    root: String,
    source_pattern_preset: Option<String>,
    source_pattern_regex: Option<String>,
    ocr_word_threshold: u32,
    ocr_area_threshold: f32,
    nsfw_score_threshold: f32,
    source_folders: Vec<SourceFolderView>,
    categories: Vec<CategoryView>,
    unclassified_count: usize,
    images: Vec<ImageView>,
    /// How much analysis is outstanding, measured off this very scan. It rides along with the view
    /// because the toolbar has to show it *before* anything is started, and re-deriving it on the
    /// frontend would mean a second copy of every "already done" rule in another language.
    pending: PendingAnalysis,
}

/// Bit per pass in `PendingAnalysis::by_pass_mask`, in queue order.
const PASS_BIT_NSFW: usize = 1;
const PASS_BIT_CHUNK: usize = 2;
const PASS_BIT_TEXT: usize = 4;
const PASS_BIT_OCR: usize = 8;
const PASS_BIT_VISION: usize = 16;
const PASS_MASK_COUNT: usize = 32;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingAnalysis {
    /// How many images each COMBINATION of passes still has work on, indexed by a bitmask of
    /// nsfw=1, chunk=2, text=4, ocr=8, vision=16; index 0 is everything nothing is waiting on.
    ///
    /// A 32-entry table rather than five totals because the question the toolbar answers — "how
    /// many images would the ticked passes touch" — is a *union*, and a union cannot be recovered
    /// from per-pass totals: an image that is new to both Explicit and Text is one image to
    /// analyze, not two. Summing the table over the masks that intersect the selection gives the
    /// exact answer for any of the 31 tick combinations, with no second trip to the backend.
    by_pass_mask: Vec<usize>,
    /// Images inside an included folder AND an included category: the pool the passes draw from.
    eligible_images: usize,
    /// False when the library has source folders and every one of them is switched off — which is
    /// otherwise indistinguishable from a fully analyzed library, since both report zero pending.
    any_folder_included: bool,
    vision_skipped_unscored: usize,
    vision_skipped_explicit: usize,
    vision_skipped_video: usize,
    vision_skipped_category: usize,
}

impl Default for PendingAnalysis {
    fn default() -> Self {
        Self {
            by_pass_mask: vec![0; PASS_MASK_COUNT],
            eligible_images: 0,
            any_folder_included: true,
            vision_skipped_unscored: 0,
            vision_skipped_explicit: 0,
            vision_skipped_video: 0,
            vision_skipped_category: 0,
        }
    }
}

// `Deserialize` because a headless run reads these back off its own event bus to mirror progress
// into the cross-process run state — see `install_progress_mirror`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextAnalysisProgress {
    processed: usize,
    total: usize,
    current_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TextAnalysisFinished {
    status: String,
    message: Option<String>,
}

#[derive(Default)]
struct AnalysisControl {
    running: AtomicBool,
    cancel: AtomicBool,
}

#[derive(Default)]
struct NsfwControl {
    running: AtomicBool,
    cancel: AtomicBool,
}

#[derive(Default)]
struct OcrTextControl {
    running: AtomicBool,
    cancel: AtomicBool,
}

#[derive(Default)]
struct ChunkControl {
    running: AtomicBool,
    cancel: AtomicBool,
}

#[derive(Default)]
struct VisionControl {
    running: AtomicBool,
    cancel: AtomicBool,
    /// Set by the headless supervisor when it stopped the pass at its time limit rather than
    /// because somebody cancelled. It stops the run *through* `cancel` — reusing the one flag the
    /// loop already checks every image, instead of adding a second check to the same hot path — so
    /// this is what tells the two apart afterwards. Never set by the GUI's Describe button, which
    /// is deliberately unlimited.
    hit_time_limit: AtomicBool,
}

#[derive(Default)]
struct TopicControl {
    running: AtomicBool,
    cancel: AtomicBool,
}

#[derive(Default)]
struct KindControl {
    running: AtomicBool,
    cancel: AtomicBool,
}

const DEFAULT_TILE_SIZE: u32 = 168;
const MIN_TILE_SIZE: u32 = 96;
const MAX_TILE_SIZE: u32 = 280;

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Failed to resolve app data directory: {error}"))?
        .join("settings.json"))
}

fn load_app_settings(app: &AppHandle) -> AppSettings {
    settings_path(app)
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|data| serde_json::from_str::<AppSettings>(&data).ok())
        .unwrap_or_default()
}

fn save_app_settings(app: &AppHandle, settings: &AppSettings) -> Result<(), String> {
    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create settings directory: {error}"))?;
    }
    let data = serde_json::to_string_pretty(settings)
        .map_err(|error| format!("Failed to serialize settings: {error}"))?;
    fs::write(path, data).map_err(|error| format!("Failed to save settings: {error}"))
}

fn clamp_tile_size(size: u32) -> u32 {
    size.clamp(MIN_TILE_SIZE, MAX_TILE_SIZE)
}

fn app_settings_view(app: &AppHandle) -> AppSettingsView {
    let settings = load_app_settings(app);
    let last_root_exists = settings
        .last_root
        .as_ref()
        .map(|root| Path::new(root).is_dir())
        .unwrap_or(false);
    let known_roots = settings
        .known_roots
        .iter()
        .map(|path| KnownRootView {
            path: path.clone(),
            exists: Path::new(path).is_dir(),
        })
        .collect();
    AppSettingsView {
        last_root: settings.last_root,
        last_root_exists,
        tile_size: clamp_tile_size(settings.tile_size.unwrap_or(DEFAULT_TILE_SIZE)),
        dark_mode: settings.dark_mode.unwrap_or(true),
        known_roots,
    }
}

// ==============================
// Saved window geometry
//
// Deliberate, not automatic: "Save Current Position & Size" in Settings stamps the window's current
// rect as the one every launch opens at. Nothing writes it on close, so nudging the window around
// during a session — or closing it maximized — can never quietly replace the default that was
// chosen on purpose.
// ==============================

fn window_bounds_of(window: &WebviewWindow) -> Result<WindowBounds, String> {
    let scale = window
        .scale_factor()
        .map_err(|error| format!("Failed to read the window scale factor: {error}"))?;
    let position = window
        .outer_position()
        .map_err(|error| format!("Failed to read the window position: {error}"))?;
    let size = window
        .inner_size()
        .map_err(|error| format!("Failed to read the window size: {error}"))?;
    Ok(WindowBounds {
        x: (f64::from(position.x) / scale).round() as i32,
        y: (f64::from(position.y) / scale).round() as i32,
        width: (f64::from(size.width) / scale).round() as u32,
        height: (f64::from(size.height) / scale).round() as u32,
    })
}

/// Position → size → position. The first move lands the window on its target monitor so the size is
/// resolved at *that* monitor's scale factor; applying the size can then nudge the window, so the
/// position is re-applied afterwards.
fn apply_window_bounds(window: &WebviewWindow, bounds: &WindowBounds) -> Result<(), String> {
    if bounds.width == 0 || bounds.height == 0 {
        return Ok(());
    }
    let position = Position::Logical(LogicalPosition {
        x: f64::from(bounds.x),
        y: f64::from(bounds.y),
    });
    window
        .set_position(position.clone())
        .map_err(|error| format!("Failed to move the window: {error}"))?;
    window
        .set_size(Size::Logical(LogicalSize {
            width: f64::from(bounds.width),
            height: f64::from(bounds.height),
        }))
        .map_err(|error| format!("Failed to size the window: {error}"))?;
    window
        .set_position(position)
        .map_err(|error| format!("Failed to move the window: {error}"))
}

/// A saved rect can point at a monitor that is no longer attached — and this window is unhidden by
/// the frontend, so an off-screen restore would look exactly like a hung app. Measured in PHYSICAL
/// pixels because that is the one coordinate space every monitor shares; a "logical virtual desktop"
/// does not exist on a mixed-DPI desk.
fn window_is_reachable(window: &WebviewWindow) -> bool {
    const MIN_VISIBLE_WIDTH: i32 = 120;
    const MIN_VISIBLE_HEIGHT: i32 = 32;

    let (Ok(position), Ok(size), Ok(monitors)) = (
        window.outer_position(),
        window.outer_size(),
        window.available_monitors(),
    ) else {
        return true; // Can't tell — leave the window where the user asked for it.
    };
    let (left, top) = (position.x, position.y);
    let (right, bottom) = (left + size.width as i32, top + size.height as i32);
    monitors.iter().any(|monitor| {
        let origin = monitor.position();
        let extent = monitor.size();
        let overlap_x = right.min(origin.x + extent.width as i32) - left.max(origin.x);
        let overlap_y = bottom.min(origin.y + extent.height as i32) - top.max(origin.y);
        overlap_x >= MIN_VISIBLE_WIDTH && overlap_y >= MIN_VISIBLE_HEIGHT
    })
}

fn restore_saved_window_bounds(window: &WebviewWindow, settings: &AppSettings) {
    let Some(bounds) = settings.window_bounds else {
        return;
    };
    if apply_window_bounds(window, &bounds).is_err() {
        return;
    }
    if !window_is_reachable(window) {
        let _ = window.center();
    }
    if settings.window_maximized {
        let _ = window.maximize();
    }
}

fn window_defaults_view(app: &AppHandle, window: Option<&WebviewWindow>) -> WindowDefaultsView {
    let settings = load_app_settings(app);
    let current = window.and_then(|window| window_bounds_of(window).ok());
    WindowDefaultsView {
        saved: settings.window_bounds,
        saved_maximized: settings.window_maximized,
        current,
        current_maximized: window
            .and_then(|window| window.is_maximized().ok())
            .unwrap_or(false),
    }
}

#[tauri::command]
fn get_window_defaults(app: AppHandle, window: WebviewWindow) -> WindowDefaultsView {
    window_defaults_view(&app, Some(&window))
}

#[tauri::command]
fn save_window_defaults(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<WindowDefaultsView, String> {
    let maximized = window.is_maximized().unwrap_or(false);
    let mut settings = load_app_settings(&app);
    // A minimized window reports a junk rect, and a maximized one reports the whole screen — neither
    // is the size to come back to, so in both cases the previously saved rect is kept and only the
    // maximized flag moves. With nothing saved yet, the current rect is still better than nothing.
    let usable = !maximized && !window.is_minimized().unwrap_or(false);
    if usable || settings.window_bounds.is_none() {
        let bounds = window_bounds_of(&window)?;
        if bounds.width > 0 && bounds.height > 0 {
            settings.window_bounds = Some(bounds);
        }
    }
    settings.window_maximized = maximized;
    save_app_settings(&app, &settings)?;
    Ok(window_defaults_view(&app, Some(&window)))
}

#[tauri::command]
fn clear_window_defaults(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<WindowDefaultsView, String> {
    let mut settings = load_app_settings(&app);
    settings.window_bounds = None;
    settings.window_maximized = false;
    save_app_settings(&app, &settings)?;
    Ok(window_defaults_view(&app, Some(&window)))
}

fn remember_known_root(settings: &mut AppSettings, root: &str) {
    settings.known_roots.retain(|item| item != root);
    settings.known_roots.insert(0, root.to_string());
}

fn sidecar_path(root: &Path) -> PathBuf {
    root.join(SIDECAR_FILE_NAME)
}

fn load_library_config(root: &Path) -> LibraryConfig {
    fs::read_to_string(sidecar_path(root))
        .ok()
        .and_then(|data| serde_json::from_str::<LibraryConfig>(&data).ok())
        .unwrap_or_default()
}

fn save_library_config(root: &Path, config: &LibraryConfig) -> Result<(), String> {
    let data = serde_json::to_string_pretty(config)
        .map_err(|error| format!("Failed to serialize library data: {error}"))?;
    fs::write(sidecar_path(root), data)
        .map_err(|error| format!("Failed to save library data: {error}"))
}

fn now_iso() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();
    let days = secs / 86400;
    let (y, m, d) = civil_from_days(days as i64);
    let rem = secs % 86400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// Howard Hinnant's days-from-civil algorithm (inverse), public-domain.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn root_path(root: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(root);
    if !path.is_dir() {
        return Err("Root folder does not exist.".to_string());
    }
    Ok(path)
}

fn path_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled")
        .to_string()
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| IMAGE_EXTS.iter().any(|candidate| candidate.eq_ignore_ascii_case(ext)))
        .unwrap_or(false)
}

fn system_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn has_forbidden_name_char(value: &str) -> bool {
    value
        .chars()
        .any(|ch| matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || ch.is_control())
}

fn validate_child_name(value: &str, kind: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{kind} name cannot be empty."));
    }
    if trimmed == "." || trimmed == ".." || trimmed.ends_with('.') || has_forbidden_name_char(trimmed) {
        return Err(format!("{kind} name contains characters Windows cannot use in filenames."));
    }
    Ok(trimmed.to_string())
}

fn preset_regex(preset: &str) -> Option<&'static str> {
    match preset {
        "YYYY-MM" => Some(r"^\d{4}-\d{2}$"),
        "YYYY_MM" => Some(r"^\d{4}_\d{2}$"),
        "MM-YYYY" => Some(r"^\d{2}-\d{4}$"),
        "Month YYYY" => Some(
            r"^(?i)(January|February|March|April|May|June|July|August|September|October|November|December) \d{4}$",
        ),
        _ => None,
    }
}

fn effective_pattern(config: &LibraryConfig) -> Option<String> {
    if let Some(custom) = config.source_pattern_regex.as_ref().filter(|value| !value.trim().is_empty()) {
        return Some(custom.clone());
    }
    config
        .source_pattern_preset
        .as_deref()
        .and_then(preset_regex)
        .map(|pattern| pattern.to_string())
}

fn detect_source_folders(root: &Path, config: &LibraryConfig) -> Result<Vec<(String, bool)>, String> {
    let pattern = effective_pattern(config);
    let regex = match pattern {
        Some(pattern) => Some(Regex::new(&pattern).map_err(|error| format!("Invalid source pattern: {error}"))?),
        None => None,
    };

    let mut folders: Vec<(String, bool)> = Vec::new();
    let entries = fs::read_dir(root).map_err(|error| format!("Failed to read root folder: {error}"))?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path_name(&path);
        if name.starts_with('.') {
            continue;
        }
        let matches_pattern = regex.as_ref().map(|re| re.is_match(&name)).unwrap_or(false);
        let is_manual = config.manual_source_folders.iter().any(|folder| folder == &name);
        if matches_pattern || is_manual {
            folders.push((name, is_manual));
        }
    }

    folders.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    Ok(folders)
}

/// The source folder a record belongs to, read back out of its stored relative path. Paths are
/// stored with '/' separators (see `scanned_image`); a path with no separator is a file sitting
/// directly in the root, which `ROOT_SOURCE_FOLDER` represents.
fn record_source_folder(last_known_path: &str) -> &str {
    match last_known_path.split_once('/') {
        Some((folder, _)) => folder,
        None => ROOT_SOURCE_FOLDER,
    }
}

struct ScannedImage {
    relative_path: String,
    absolute_path: PathBuf,
    name: String,
    source_folder: String,
    size: u64,
    modified_ms: u64,
    hash: String,
}

fn hash_file(path: &Path, size: u64) -> Result<String, String> {
    let file = File::open(path).map_err(|error| format!("Failed to open {}: {error}", path.display()))?;

    // `Read::read` is allowed to return fewer bytes than asked for without being at EOF, which a
    // single call would silently treat as the whole sample — yielding a different hash for a file
    // that never changed, orphaning its record and losing that image's category. `read_to_end` on a
    // capped reader keeps reading until the cap or real EOF. It hashes the identical bytes a full
    // single read would have, so hashes already stored in sidecars stay valid.
    let mut buffer = Vec::with_capacity(HASH_SAMPLE_BYTES.min(size as usize));
    file.take(HASH_SAMPLE_BYTES as u64)
        .read_to_end(&mut buffer)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;

    let mut hasher = DefaultHasher::new();
    size.hash(&mut hasher);
    buffer[..].hash(&mut hasher);
    Ok(format!("{:016x}", hasher.finish()))
}

/// Maps a known relative path to the hash last computed for it, plus the size/mtime it had at
/// that moment. Built from the sidecar's existing records — no extra file to keep in sync.
type HashIndex = HashMap<String, (String, u64, u64)>;

fn build_hash_index(config: &LibraryConfig) -> HashIndex {
    config
        .images
        .iter()
        .filter_map(|(hash, record)| match (record.size, record.modified_ms) {
            (Some(size), Some(modified_ms)) => Some((
                record.last_known_path.clone(),
                (hash.clone(), size, modified_ms),
            )),
            _ => None,
        })
        .collect()
}

/// Builds the scan entry for one image file, reusing the cached hash when the file is byte-for-byte
/// the file we hashed last time (same path, size and mtime). Hashing reads 64KB off disk per image,
/// so on a large library skipping it is the difference between a refresh costing a gigabyte of
/// reads and costing a directory listing.
fn scanned_image(
    root: &Path,
    source_folder: &str,
    path: PathBuf,
    metadata: &fs::Metadata,
    hash_index: &HashIndex,
) -> Result<ScannedImage, String> {
    let name = path_name(&path);
    let size = metadata.len();
    let modified_ms = metadata.modified().map(system_time_ms).unwrap_or_default();
    let relative_path = path
        .strip_prefix(root)
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| name.clone());

    let cached = hash_index
        .get(&relative_path)
        .filter(|(_, cached_size, cached_modified)| *cached_size == size && *cached_modified == modified_ms)
        .map(|(hash, _, _)| hash.clone());
    let hash = match cached {
        Some(hash) => hash,
        None => hash_file(&path, size)?,
    };

    Ok(ScannedImage {
        relative_path,
        absolute_path: path,
        name,
        source_folder: source_folder.to_string(),
        size,
        modified_ms,
        hash,
    })
}

fn collect_images_in_folder(
    root: &Path,
    source_folder: &str,
    folder: &Path,
    depth: usize,
    hash_index: &HashIndex,
    images: &mut Vec<ScannedImage>,
) -> Result<(), String> {
    let entries = fs::read_dir(folder).map_err(|error| format!("Failed to read folder {}: {error}", folder.display()))?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = path_name(&path);
        if name.starts_with('.') {
            continue;
        }
        if path.is_file() && is_image_path(&path) {
            let metadata = fs::metadata(&path).map_err(|error| format!("Failed to read metadata: {error}"))?;
            images.push(scanned_image(root, source_folder, path, &metadata, hash_index)?);
        } else if path.is_dir() && depth < MAX_SCAN_DEPTH {
            collect_images_in_folder(root, source_folder, &path, depth + 1, hash_index, images)?;
        }
    }
    Ok(())
}

fn collect_direct_images_in_folder(
    root: &Path,
    source_folder: &str,
    folder: &Path,
    hash_index: &HashIndex,
    images: &mut Vec<ScannedImage>,
) -> Result<(), String> {
    let entries = fs::read_dir(folder).map_err(|error| format!("Failed to read folder {}: {error}", folder.display()))?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = path_name(&path);
        if name.starts_with('.') || !path.is_file() || !is_image_path(&path) {
            continue;
        }
        let metadata = fs::metadata(&path).map_err(|error| format!("Failed to read metadata: {error}"))?;
        images.push(scanned_image(root, source_folder, path, &metadata, hash_index)?);
    }
    Ok(())
}

/// Merges freshly computed analysis results into whatever is on disk *now*, rather than writing back
/// the snapshot the pass started from.
///
/// A pass over ~20k images runs for hours. The old code loaded the config once at the start, mutated
/// that copy throughout, and saved it at the end — so any manual category, import, threshold or
/// folder change the user made during those hours was silently overwritten by the stale copy. The
/// nightly job is a *separate process*, so the in-process `AtomicBool` guards could never fix this.
/// Re-reading immediately before writing narrows the clobber window from hours to microseconds.
fn commit_analysis<F>(root: &Path, apply: F) -> Result<(), String>
where
    F: FnOnce(&mut LibraryConfig),
{
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(root);
    let mut config = load_library_config(root);
    apply(&mut config);
    reclassify_nsfw_categories(&mut config);
    reclassify_text_categories(&mut config);
    save_library_config(root, &config)
}

fn commit_text_results(root: &Path, results: &mut Vec<(String, u32, f32)>) -> Result<(), String> {
    if results.is_empty() {
        return Ok(());
    }
    commit_analysis(root, |config| {
        for (hash, word_count, area_ratio) in results.iter() {
            if let Some(record) = config.images.get_mut(hash) {
                record.ocr_word_count = Some(*word_count);
                record.ocr_text_area_ratio = Some(*area_ratio);
            }
        }
    })?;
    results.clear();
    Ok(())
}

fn commit_nsfw_results(root: &Path, results: &mut Vec<(String, f32, Vec<String>)>) -> Result<(), String> {
    if results.is_empty() {
        return Ok(());
    }
    commit_analysis(root, |config| {
        for (hash, score, labels) in results.iter() {
            if let Some(record) = config.images.get_mut(hash) {
                record.nsfw_score = Some(*score);
                record.nsfw_labels = Some(labels.clone());
            }
        }
    })?;
    results.clear();
    Ok(())
}

fn commit_extraction_results(root: &Path, results: &mut Vec<(String, u32)>) -> Result<(), String> {
    if results.is_empty() {
        return Ok(());
    }
    commit_analysis(root, |config| {
        for (hash, chars) in results.iter() {
            if let Some(record) = config.images.get_mut(hash) {
                record.ocr_text_chars = Some(*chars);
            }
        }
    })?;
    results.clear();
    Ok(())
}

fn commit_chunk_results(root: &Path, results: &mut Vec<(String, String)>) -> Result<(), String> {
    if results.is_empty() {
        return Ok(());
    }
    commit_analysis(root, |config| {
        for (hash, title) in results.iter() {
            if let Some(record) = config.images.get_mut(hash) {
                record.video_title = Some(title.clone());
            }
        }
    })?;
    results.clear();
    Ok(())
}

fn commit_vision_results(root: &Path, results: &mut Vec<(String, u32)>) -> Result<(), String> {
    if results.is_empty() {
        return Ok(());
    }
    commit_analysis(root, |config| {
        for (hash, chars) in results.iter() {
            if let Some(record) = config.images.get_mut(hash) {
                record.vision_desc_chars = Some(*chars);
            }
        }
    })?;
    results.clear();
    Ok(())
}

fn ocr_thresholds(config: &LibraryConfig) -> (u32, f32) {
    (
        config.ocr_word_threshold.unwrap_or(DEFAULT_OCR_WORD_THRESHOLD),
        config.ocr_area_threshold.unwrap_or(DEFAULT_OCR_AREA_THRESHOLD),
    )
}

fn ensure_category(config: &mut LibraryConfig, name: &str) {
    if !config.categories.iter().any(|item| item == name) {
        config.categories.push(name.to_string());
    }
}

fn ensure_analysis_categories(config: &mut LibraryConfig) {
    if config.images.values().any(|record| record.nsfw_score.is_some()) {
        ensure_category(config, EXPLICIT_CATEGORY);
    }
    if config.images.values().any(|record| record.ocr_word_count.is_some()) {
        ensure_category(config, LOW_TEXT_CATEGORY);
        ensure_category(config, HIGH_TEXT_CATEGORY);
    }
}

fn reclassify_text_categories(config: &mut LibraryConfig) {
    let any_analyzed = config.images.values().any(|record| record.ocr_word_count.is_some());
    if !any_analyzed {
        return;
    }

    let nsfw_min = nsfw_threshold(config);
    let (word_threshold, area_threshold) = ocr_thresholds(config);
    ensure_category(config, LOW_TEXT_CATEGORY);
    ensure_category(config, HIGH_TEXT_CATEGORY);

    for record in config.images.values_mut() {
        if record.classified_by.as_deref() == Some("manual") {
            continue;
        }
        if record.nsfw_score.map_or(false, |s| s >= nsfw_min) {
            if record.category.as_deref() != Some(EXPLICIT_CATEGORY) {
                record.category = Some(EXPLICIT_CATEGORY.to_string());
                record.classified_by = Some("auto-nsfw".to_string());
                record.classified_at = Some(now_iso());
            }
            // Explicit wins over the text categories, and the `continue` below is what enforces that.
            // This used to also null `ocr_word_count`/`ocr_text_area_ratio` here, which threw away
            // real OCR results on every single scan: the card then read "Text: not analyzed" forever,
            // and raising the NSFW threshold later released the image with no text data to classify
            // it by, forcing an expensive re-OCR. Keeping the data costs nothing and changes nothing
            // about which category wins.
            continue;
        }
        let (Some(word_count), Some(area_ratio)) = (record.ocr_word_count, record.ocr_text_area_ratio) else {
            continue;
        };
        let is_low_text = word_count <= word_threshold && area_ratio <= area_threshold;
        let category = if is_low_text { LOW_TEXT_CATEGORY } else { HIGH_TEXT_CATEGORY };
        if record.category.as_deref() != Some(category) {
            record.category = Some(category.to_string());
            record.classified_by = Some("auto".to_string());
            record.classified_at = Some(now_iso());
        }
    }
}

fn scan_and_reconcile(root: &Path) -> Result<LibraryView, String> {
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(root);
    let mut config = load_library_config(root);
    let source_folders = detect_source_folders(root, &config)?;

    let hash_index = build_hash_index(&config);
    let mut all_images: Vec<ScannedImage> = Vec::new();
    collect_direct_images_in_folder(root, ROOT_SOURCE_FOLDER, root, &hash_index, &mut all_images)?;
    for (folder_name, _) in &source_folders {
        collect_images_in_folder(root, folder_name, &root.join(folder_name), 0, &hash_index, &mut all_images)?;
    }

    let thumb_dir = root.join(THUMBNAIL_DIR_NAME);
    let _ = fs::create_dir_all(&thumb_dir);
    let thumbnail_paths: HashMap<String, String> = all_images
        .par_iter()
        .filter_map(|image| {
            ensure_thumbnail(&thumb_dir, &image.hash, &image.absolute_path)
                .map(|path| (image.hash.clone(), path.to_string_lossy().to_string()))
        })
        .collect();

    let mut seen_hashes = std::collections::HashSet::new();
    for image in &all_images {
        seen_hashes.insert(image.hash.clone());
        let record = config.images.entry(image.hash.clone()).or_default();
        record.last_known_path = image.relative_path.clone();
        record.size = Some(image.size);
        record.modified_ms = Some(image.modified_ms);
    }
    // Only forget an image when we actually looked in the folder it lives in and it wasn't there.
    //
    // This used to be `retain(|hash, _| seen_hashes.contains(hash))`, which could not tell "the file
    // was deleted" apart from "that folder wasn't scanned this time". `detect_source_folders` only
    // walks folders matching the source pattern or listed as manual, so mistyping the pattern in
    // Settings — or dropping a manual folder, or a month folder being temporarily renamed/offline —
    // made every record in the de-matched folders vanish, taking every manual category, NSFW score
    // and OCR result with it. Restoring the pattern brought the files back as blank records; the
    // classifications were gone for good.
    //
    // The cost of this is that records for a folder you delete outright linger in the sidecar,
    // because a folder that isn't there is also a folder we didn't scan. That is the intended trade:
    // a few stale KB beats silently shredding hand-made classifications, and it means re-adding a
    // folder restores its categories.
    let scanned_folders: std::collections::HashSet<&str> = std::iter::once(ROOT_SOURCE_FOLDER)
        .chain(source_folders.iter().map(|(name, _)| name.as_str()))
        .collect();
    config.images.retain(|hash, record| {
        if seen_hashes.contains(hash) {
            return true;
        }
        if record.last_known_path.is_empty() {
            return false; // No path at all: not a real image, nothing to protect.
        }
        !scanned_folders.contains(record_source_folder(&record.last_known_path))
    });

    reclassify_nsfw_categories(&mut config);
    reclassify_text_categories(&mut config);
    ensure_analysis_categories(&mut config);
    let valid_categories: std::collections::HashSet<String> = config.categories.iter().cloned().collect();
    for record in config.images.values_mut() {
        if let Some(category) = record.category.clone() {
            if !valid_categories.contains(&category) {
                record.category = None;
                record.classified_by = None;
            }
        }
    }

    save_library_config(root, &config)?;

    let mut category_counts: HashMap<String, usize> = config.categories.iter().map(|name| (name.clone(), 0)).collect();
    let mut unclassified_count = 0usize;
    let mut image_views = Vec::with_capacity(all_images.len());

    for image in &all_images {
        let record = config.images.get(&image.hash).cloned().unwrap_or_default();
        if let Some(category) = &record.category {
            *category_counts.entry(category.clone()).or_insert(0) += 1;
        } else {
            unclassified_count += 1;
        }
        image_views.push(ImageView {
            hash: image.hash.clone(),
            path: image.absolute_path.to_string_lossy().to_string(),
            thumbnail_path: thumbnail_paths.get(&image.hash).cloned(),
            relative_path: image.relative_path.clone(),
            name: image.name.clone(),
            source_folder: image.source_folder.clone(),
            size: image.size,
            modified_ms: image.modified_ms,
            category: record.category,
            classified_by: record.classified_by,
            classified_at: record.classified_at,
            ocr_word_count: record.ocr_word_count,
            ocr_text_area_ratio: record.ocr_text_area_ratio,
            ocr_text_chars: record.ocr_text_chars,
            nsfw_score: record.nsfw_score,
            nsfw_labels: record.nsfw_labels,
            // Surface the title only when it's a real video (non-empty); `Some("")` just means
            // "title strip scanned, not a video" and should read as blank in the UI.
            video_title: record.video_title.filter(|title| !title.is_empty()),
            vision_desc_chars: record.vision_desc_chars,
        });
    }

    image_views.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms).then_with(|| a.name.cmp(&b.name)));

    let mut folder_counts: HashMap<String, usize> = HashMap::new();
    for image in &all_images {
        *folder_counts.entry(image.source_folder.clone()).or_insert(0) += 1;
    }

    let mut source_folder_views: Vec<SourceFolderView> = Vec::new();
    if folder_counts.get(ROOT_SOURCE_FOLDER).copied().unwrap_or(0) > 0 {
        source_folder_views.push(SourceFolderView {
            relative_path: ".".to_string(),
            image_count: folder_counts.get(ROOT_SOURCE_FOLDER).copied().unwrap_or(0),
            included_in_analysis: !config
                .excluded_analysis_folders
                .iter()
                .any(|excluded| excluded == ROOT_SOURCE_FOLDER),
            name: ROOT_SOURCE_FOLDER.to_string(),
            is_manual: false,
        });
    }
    source_folder_views.extend(source_folders.into_iter().map(|(name, is_manual)| SourceFolderView {
            relative_path: name.clone(),
            image_count: folder_counts.get(&name).copied().unwrap_or(0),
            included_in_analysis: !config.excluded_analysis_folders.iter().any(|excluded| excluded == &name),
            name,
            is_manual,
        }));

    let mut categories: Vec<CategoryView> = config
        .categories
        .iter()
        .map(|name| CategoryView {
            name: name.clone(),
            count: category_counts.get(name).copied().unwrap_or(0),
            included_in_analysis: !config.excluded_analysis_categories.iter().any(|excluded| excluded == name),
        })
        .collect();
    categories.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let (ocr_word_threshold, ocr_area_threshold) = ocr_thresholds(&config);
    let nsfw_score_threshold = nsfw_threshold(&config);

    let mut view = LibraryView {
        root: root.to_string_lossy().to_string(),
        source_pattern_preset: config.source_pattern_preset.clone(),
        source_pattern_regex: config.source_pattern_regex.clone(),
        ocr_word_threshold,
        ocr_area_threshold,
        nsfw_score_threshold,
        source_folders: source_folder_views,
        categories,
        unclassified_count,
        images: image_views,
        pending: PendingAnalysis::default(),
    };
    // Measured off the scan that just happened rather than by a check of its own: every path that
    // refreshes the library — first load, Rescan, a threshold or exclusion change, the end of a run
    // — therefore refreshes the outstanding counts too, and they can never be from a different
    // moment than the grid beside them.
    view.pending = pending_analysis(&view, &config, load_chunk_plan(root).as_ref());
    Ok(view)
}

// The Windows directory, for naming a system binary absolutely. `CreateProcess` resolves a bare
// `explorer.exe` against the current directory before `System32`, so a stray executable dropped
// beside a library folder could stand in for it; an absolute path removes that possibility.
#[cfg(target_os = "windows")]
fn windows_dir() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
}

fn launch_path(path: &Path) -> Result<(), String> {
    // Windows: `explorer.exe <path>` reaches the same default handler `start` would — for a file
    // and for a folder alike — but explorer is not a shell, so it never re-reads the path looking
    // for operators.
    //
    // This must not go back through `cmd /C start`. Rust quotes an argument only when it holds a
    // space or a tab, so any path without one arrived at cmd bare, and cmd read `&` in it as a
    // command separator: opening an image named `cat&whoami&x.jpg` out of a space-free folder ran
    // `whoami`. Filenames carrying `&` are legal and survive any download or archive extraction,
    // which is precisely how images reach this app.
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new(windows_dir().join("explorer.exe"));
        command.arg(path);
        command
    };

    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };

    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Failed to open {}: {error}", path.display()))
}

#[tauri::command]
fn get_app_settings(app: AppHandle) -> AppSettingsView {
    app_settings_view(&app)
}

#[tauri::command]
fn set_tile_size(app: AppHandle, tile_size: u32) -> Result<AppSettingsView, String> {
    let mut settings = load_app_settings(&app);
    settings.tile_size = Some(clamp_tile_size(tile_size));
    save_app_settings(&app, &settings)?;
    Ok(app_settings_view(&app))
}

#[tauri::command]
fn set_dark_mode(app: AppHandle, dark_mode: bool) -> Result<AppSettingsView, String> {
    let mut settings = load_app_settings(&app);
    settings.dark_mode = Some(dark_mode);
    save_app_settings(&app, &settings)?;
    Ok(app_settings_view(&app))
}

#[tauri::command]
fn choose_root_folder(app: AppHandle, folder_path: String) -> Result<LibraryView, String> {
    let root = root_path(&folder_path)?;
    let root_str = root.to_string_lossy().to_string();
    let mut settings = load_app_settings(&app);
    settings.last_root = Some(root_str.clone());
    remember_known_root(&mut settings, &root_str);
    save_app_settings(&app, &settings)?;
    scan_and_reconcile(&root)
}

#[tauri::command]
fn select_root_folder(app: AppHandle, root: String) -> Result<LibraryView, String> {
    let root_buf = root_path(&root)?;
    let root_str = root_buf.to_string_lossy().to_string();
    let mut settings = load_app_settings(&app);
    settings.last_root = Some(root_str.clone());
    remember_known_root(&mut settings, &root_str);
    save_app_settings(&app, &settings)?;
    scan_and_reconcile(&root_buf)
}

#[tauri::command]
fn scan_library(root: String) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    scan_and_reconcile(&root)
}

#[tauri::command]
fn set_source_pattern(
    root: String,
    preset: Option<String>,
    regex: Option<String>,
) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);
    config.source_pattern_preset = preset;
    config.source_pattern_regex = regex.filter(|value| !value.trim().is_empty());
    save_library_config(&root, &config)?;
    scan_and_reconcile(&root)
}

// ==============================
// Analysis selection — what "new" means
// ==============================
//
// `Analyze New` counts the images it is about to work through before it starts one, so the count
// and the pass that follows it MUST select the same images: a rule tightened in one place and not
// the other turns the number on screen into a lie about a run that can take hours. Every pass and
// the pre-count go through the `pending_*` functions below, and nothing else decides eligibility.

/// The folder and category switches, which apply to every pass identically. Built once per call so
/// a pass doesn't rebuild the excluded-name sets per image.
struct AnalysisScope {
    excluded_folders: std::collections::HashSet<String>,
    excluded_categories: std::collections::HashSet<String>,
}

impl AnalysisScope {
    fn new(config: &LibraryConfig) -> Self {
        Self {
            excluded_folders: config.excluded_analysis_folders.iter().cloned().collect(),
            excluded_categories: excluded_analysis_categories(config),
        }
    }

    fn folder_included(&self, image: &ImageView) -> bool {
        !self.excluded_folders.contains(&image.source_folder)
    }

    fn category_included(&self, config: &LibraryConfig, image: &ImageView) -> bool {
        !category_is_excluded(config, &image.hash, &self.excluded_categories)
    }

    fn includes(&self, config: &LibraryConfig, image: &ImageView) -> bool {
        self.folder_included(image) && self.category_included(config, image)
    }
}

/// False only when the library has source folders and every one of them is switched off. Worth
/// saying out loud, because otherwise a fully excluded library is indistinguishable from a fully
/// analyzed one: both report zero pending.
fn analysis_has_included_folder(view: &LibraryView, config: &LibraryConfig) -> bool {
    let excluded: std::collections::HashSet<&str> = config
        .excluded_analysis_folders
        .iter()
        .map(String::as_str)
        .collect();
    view.source_folders.is_empty()
        || view
            .source_folders
            .iter()
            .any(|folder| !excluded.contains(folder.name.as_str()))
}

/// Images the OCR word-count pass still has to read. Explicit images are left alone: their score is
/// already the classification, and running OCR over them buys nothing.
fn pending_text<'a>(view: &'a LibraryView, config: &LibraryConfig, force: bool) -> Vec<&'a ImageView> {
    let scope = AnalysisScope::new(config);
    let threshold = nsfw_threshold(config);
    view.images
        .iter()
        .filter(|image| scope.includes(config, image))
        .filter(|image| {
            config
                .images
                .get(&image.hash)
                .and_then(|record| record.nsfw_score)
                .map(|score| score < threshold)
                .unwrap_or(true)
        })
        .filter(|image| {
            force
                || config
                    .images
                    .get(&image.hash)
                    .map(|record| record.ocr_word_count.is_none())
                    .unwrap_or(true)
        })
        .collect()
}

/// Images whose recognized text has not been written to the OCR text folder yet.
fn pending_text_extraction<'a>(
    view: &'a LibraryView,
    config: &LibraryConfig,
    force: bool,
) -> Vec<&'a ImageView> {
    let scope = AnalysisScope::new(config);
    view.images
        .iter()
        .filter(|image| scope.includes(config, image))
        .filter(|image| {
            force
                || config
                    .images
                    .get(&image.hash)
                    .map(|record| record.ocr_text_chars.is_none())
                    .unwrap_or(true)
        })
        .collect()
}

/// Images with no explicit-content score yet.
fn pending_nsfw<'a>(view: &'a LibraryView, config: &LibraryConfig, force: bool) -> Vec<&'a ImageView> {
    let scope = AnalysisScope::new(config);
    view.images
        .iter()
        .filter(|image| scope.includes(config, image))
        .filter(|image| {
            force
                || config
                    .images
                    .get(&image.hash)
                    .map(|record| record.nsfw_score.is_none())
                    .unwrap_or(true)
        })
        .collect()
}

/// Images whose title strip has not been OCR'd yet. A frame that was scanned and turned out not to
/// be a video carries `Some("")`, so it is not pending either — only a missing field is.
fn pending_chunk<'a>(view: &'a LibraryView, config: &LibraryConfig, force: bool) -> Vec<&'a ImageView> {
    let scope = AnalysisScope::new(config);
    view.images
        .iter()
        .filter(|image| scope.includes(config, image))
        .filter(|image| {
            force
                || config
                    .images
                    .get(&image.hash)
                    .map(|record| record.video_title.is_none())
                    .unwrap_or(true)
        })
        .collect()
}

/// Why Describe's queue is so much shorter than the others — reported rather than inferred, because
/// "3 images to describe" out of 13,000 reads as a bug until you can see where the rest went.
#[derive(Debug, Clone, Copy, Default)]
struct VisionSkips {
    /// Video frames the chunk plan did not sample.
    video: usize,
    explicit: usize,
    /// Not yet Explicit-analyzed. Describe refuses to look at an unscored image, so these are work
    /// the Explicit pass unlocks rather than work that is done.
    unscored: usize,
    category: usize,
}

/// Images the vision model still has to describe, plus the tally of what was skipped and why.
fn pending_vision<'a>(
    view: &'a LibraryView,
    config: &LibraryConfig,
    plan: Option<&ChunkPlan>,
    force: bool,
) -> (Vec<&'a ImageView>, VisionSkips) {
    let scope = AnalysisScope::new(config);
    let threshold = nsfw_threshold(config);

    // The chunk plan decides which video frames are allowed (only the sampled ones) and which
    // hashes are video members at all (the rest are non-video and always eligible).
    let mut selected: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut video_members: std::collections::HashSet<&str> = std::collections::HashSet::new();
    if let Some(plan) = plan {
        for group in &plan.groups {
            for hash in &group.member_hashes {
                video_members.insert(hash.as_str());
            }
            for hash in &group.selected_hashes {
                selected.insert(hash.as_str());
            }
        }
    }

    let mut skips = VisionSkips::default();
    let pending = view
        .images
        .iter()
        .filter(|image| scope.folder_included(image))
        .filter(|image| {
            // Omitted categories (e.g. "High Text", already covered by OCR) never reach the
            // vision model — this is where the token savings the user asked for come from.
            if !scope.category_included(config, image) {
                skips.category += 1;
                return false;
            }
            true
        })
        .filter(|image| {
            if video_members.contains(image.hash.as_str()) && !selected.contains(image.hash.as_str()) {
                skips.video += 1;
                return false;
            }
            true
        })
        .filter(|image| match config.images.get(&image.hash).and_then(|r| r.nsfw_score) {
            Some(score) if score >= threshold => {
                skips.explicit += 1;
                false
            }
            Some(_) => true,
            None => {
                skips.unscored += 1;
                false
            }
        })
        .filter(|image| {
            force
                || config
                    .images
                    .get(&image.hash)
                    .map(|record| record.vision_desc_chars.is_none())
                    .unwrap_or(true)
        })
        .collect();

    (pending, skips)
}

/// Tallies every pass's outstanding work in one walk of the scan that just finished, through the
/// very same selectors the passes themselves use.
///
/// Each image is reduced to the bitmask of passes still waiting on it, which is what makes the
/// toolbar's readout free: the frontend re-answers "how many for THIS tick combination" by summing
/// the table, never by asking again.
fn pending_analysis(view: &LibraryView, config: &LibraryConfig, plan: Option<&ChunkPlan>) -> PendingAnalysis {
    let scope = AnalysisScope::new(config);
    let (vision, skips) = pending_vision(view, config, plan, false);

    // Keyed by relative path, which is one file: duplicates share a hash and each copy is analyzed,
    // so a hash here would quietly merge two images' worth of work into one.
    let sets: [(usize, std::collections::HashSet<&str>); 5] = [
        (PASS_BIT_NSFW, pending_nsfw(view, config, false).iter().map(|i| i.relative_path.as_str()).collect()),
        (PASS_BIT_CHUNK, pending_chunk(view, config, false).iter().map(|i| i.relative_path.as_str()).collect()),
        (PASS_BIT_TEXT, pending_text(view, config, false).iter().map(|i| i.relative_path.as_str()).collect()),
        (PASS_BIT_OCR, pending_text_extraction(view, config, false).iter().map(|i| i.relative_path.as_str()).collect()),
        (PASS_BIT_VISION, vision.iter().map(|i| i.relative_path.as_str()).collect()),
    ];

    let mut by_pass_mask = vec![0usize; PASS_MASK_COUNT];
    for image in &view.images {
        let mut mask = 0usize;
        for (bit, set) in &sets {
            if set.contains(image.relative_path.as_str()) {
                mask |= bit;
            }
        }
        by_pass_mask[mask] += 1;
    }

    PendingAnalysis {
        by_pass_mask,
        eligible_images: view.images.iter().filter(|image| scope.includes(config, image)).count(),
        any_folder_included: analysis_has_included_folder(view, config),
        vision_skipped_unscored: skips.unscored,
        vision_skipped_explicit: skips.explicit,
        vision_skipped_video: skips.video,
        vision_skipped_category: skips.category,
    }
}

#[tauri::command]
fn analyze_text(app: AppHandle, control: tauri::State<'_, AnalysisControl>, root: String, force: bool) -> Result<(), String> {
    if control.running.swap(true, Ordering::SeqCst) {
        return Err("Text analysis is already running.".to_string());
    }

    let root_buf = match root_path(&root) {
        Ok(path) => path,
        Err(error) => {
            control.running.store(false, Ordering::SeqCst);
            return Err(error);
        }
    };
    control.cancel.store(false, Ordering::SeqCst);

    let app_handle = app.clone();
    std::thread::spawn(move || {
        run_text_analysis(&app_handle, &root_buf, force);
    });

    Ok(())
}

#[tauri::command]
fn cancel_text_analysis(control: tauri::State<'_, AnalysisControl>) -> Result<(), String> {
    if !control.running.load(Ordering::SeqCst) {
        return Err("No text analysis is running.".to_string());
    }
    control.cancel.store(true, Ordering::SeqCst);
    Ok(())
}

// Runs on a detached background thread so `analyze_text` returns immediately and the UI stays
// responsive. Only the images present at scan time are ever touched: anything added to the
// library mid-run is picked up by the next scan, never by this one.
fn run_text_analysis(app: &AppHandle, root_buf: &Path, force: bool) {
    let control = app.state::<AnalysisControl>();

    let result = (|| -> Result<(&'static str, Option<String>), String> {
        let view = scan_and_reconcile(root_buf)?;
        let config = load_library_config(root_buf);

        if !analysis_has_included_folder(&view, &config) {
            return Ok(("completed", Some("No source folders are included in analysis.".to_string())));
        }

        let pending: Vec<(String, String, String)> = pending_text(&view, &config, force)
            .into_iter()
            .map(|image| (image.hash.clone(), image.path.clone(), image.name.clone()))
            .collect();
        drop(config);

        let total = pending.len();
        let mut cancelled = false;
        let mut results: Vec<(String, u32, f32)> = Vec::new();

        for (index, (hash, path, name)) in pending.iter().enumerate() {
            if control.cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }

            match analyze_image_text(Path::new(path)) {
                Ok(stats) => results.push((hash.clone(), stats.word_count, stats.text_area_ratio)),
                Err(error) => {
                    eprintln!("OCR failed for {path}: {error}");
                }
            }

            // Checkpoint periodically so a crash, a reboot or a cancel part-way through a multi-hour
            // pass keeps the work done so far instead of throwing all of it away.
            if results.len() >= ANALYSIS_CHECKPOINT_EVERY {
                commit_text_results(root_buf, &mut results)?;
            }

            let _ = app.emit(
                "text-analysis-progress",
                TextAnalysisProgress {
                    processed: index + 1,
                    total,
                    current_name: name.clone(),
                },
            );
        }

        commit_text_results(root_buf, &mut results)?;

        let message = if total == 0 { Some("No images needed analysis.".to_string()) } else { None };
        Ok((if cancelled { "cancelled" } else { "completed" }, message))
    })();

    control.running.store(false, Ordering::SeqCst);

    let (status, message) = match result {
        Ok((status, message)) => (status.to_string(), message),
        Err(error) => ("error".to_string(), Some(error)),
    };
    let _ = app.emit("text-analysis-finished", TextAnalysisFinished { status, message });
}

#[tauri::command]
fn set_text_thresholds(root: String, word_threshold: u32, area_threshold: f32) -> Result<LibraryView, String> {
    let root_buf = root_path(&root)?;
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root_buf);
    let mut config = load_library_config(&root_buf);
    config.ocr_word_threshold = Some(word_threshold);
    config.ocr_area_threshold = Some(area_threshold.clamp(0.0, 1.0));
    reclassify_text_categories(&mut config);
    save_library_config(&root_buf, &config)?;
    scan_and_reconcile(&root_buf)
}

#[tauri::command]
fn extract_text(
    app: AppHandle,
    control: tauri::State<'_, OcrTextControl>,
    root: String,
    force: bool,
    indexed_only: bool,
) -> Result<(), String> {
    if control.running.swap(true, Ordering::SeqCst) {
        return Err("Text extraction is already running.".to_string());
    }

    let root_buf = match root_path(&root) {
        Ok(path) => path,
        Err(error) => {
            control.running.store(false, Ordering::SeqCst);
            return Err(error);
        }
    };
    control.cancel.store(false, Ordering::SeqCst);

    let app_handle = app.clone();
    std::thread::spawn(move || {
        run_text_extraction(&app_handle, &root_buf, force, indexed_only);
    });

    Ok(())
}

#[tauri::command]
fn cancel_text_extraction(control: tauri::State<'_, OcrTextControl>) -> Result<(), String> {
    if !control.running.load(Ordering::SeqCst) {
        return Err("No text extraction is running.".to_string());
    }
    control.cancel.store(true, Ordering::SeqCst);
    Ok(())
}

// Runs on a detached background thread, mirroring `run_text_analysis`/`run_nsfw_analysis`: it only
// touches the images present at scan time, skips already-extracted images unless `force`, honours
// excluded folders, and reports progress through the `text-extraction-*` events. Each image's
// recognized text is written to `<root>/.image-categorizer-ocr-text/<hash>.txt`.
//
// `indexed_only` narrows the run to the categories the text index covers. The Analyze row passes
// false — it means "extract everything outstanding" and always has. The Extracted Text panel passes
// true, because the number it offers to act on is a High Text number: running the other 783 images
// as well would do work the panel never mentioned and finish reporting a count nobody asked about.
fn run_text_extraction(app: &AppHandle, root_buf: &Path, force: bool, indexed_only: bool) {
    let control = app.state::<OcrTextControl>();

    let result = (|| -> Result<(&'static str, Option<String>), String> {
        let view = scan_and_reconcile(root_buf)?;
        let config = load_library_config(root_buf);

        let text_dir = root_buf.join(OCR_TEXT_DIR_NAME);
        fs::create_dir_all(&text_dir)
            .map_err(|error| format!("Failed to create text folder: {error}"))?;

        if !analysis_has_included_folder(&view, &config) {
            return Ok(("completed", Some("No source folders are included in extraction.".to_string())));
        }

        let categories = indexed_categories(&config);
        let pending: Vec<(String, String, String)> = pending_text_extraction(&view, &config, force)
            .into_iter()
            .filter(|image| {
                !indexed_only
                    || image
                        .category
                        .as_ref()
                        .map(|category| categories.contains(category))
                        .unwrap_or(false)
            })
            .map(|image| (image.hash.clone(), image.path.clone(), image.name.clone()))
            .collect();
        drop(config);

        let total = pending.len();
        let mut cancelled = false;
        let mut results: Vec<(String, u32)> = Vec::new();

        for (index, (hash, path, name)) in pending.iter().enumerate() {
            if control.cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }

            match extract_image_text(Path::new(path)) {
                Ok(text) => {
                    let text_path = text_dir.join(format!("{hash}.txt"));
                    match fs::write(&text_path, &text) {
                        Ok(()) => results.push((hash.clone(), text.chars().count() as u32)),
                        Err(error) => eprintln!("Failed to save OCR text for {path}: {error}"),
                    }
                }
                Err(error) => {
                    eprintln!("Text extraction failed for {path}: {error}");
                }
            }

            if results.len() >= ANALYSIS_CHECKPOINT_EVERY {
                commit_extraction_results(root_buf, &mut results)?;
            }

            let _ = app.emit(
                "text-extraction-progress",
                TextAnalysisProgress {
                    processed: index + 1,
                    total,
                    current_name: name.clone(),
                },
            );
        }

        commit_extraction_results(root_buf, &mut results)?;

        let message = if total == 0 { Some("No images needed text extraction.".to_string()) } else { None };
        Ok((if cancelled { "cancelled" } else { "completed" }, message))
    })();

    control.running.store(false, Ordering::SeqCst);

    let (status, message) = match result {
        Ok((status, message)) => (status.to_string(), message),
        Err(error) => ("error".to_string(), Some(error)),
    };
    let _ = app.emit("text-extraction-finished", TextAnalysisFinished { status, message });
}

#[tauri::command]
fn add_manual_source_folder(root: String, folder_path: String) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    let folder = PathBuf::from(&folder_path);
    if !folder.is_dir() {
        return Err("Selected folder does not exist.".to_string());
    }
    let canonical_root = root.canonicalize().map_err(|error| format!("Failed to resolve root: {error}"))?;
    let canonical_folder = folder.canonicalize().map_err(|error| format!("Failed to resolve folder: {error}"))?;
    let relative = canonical_folder
        .strip_prefix(&canonical_root)
        .map_err(|_| "Folder must be a direct subfolder of the root folder.".to_string())?;
    if relative.components().count() != 1 {
        return Err("Folder must be a direct subfolder of the root folder.".to_string());
    }
    let name = relative.to_string_lossy().to_string();

    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);
    if !config.manual_source_folders.iter().any(|item| item == &name) {
        config.manual_source_folders.push(name);
        save_library_config(&root, &config)?;
    }
    scan_and_reconcile(&root)
}

// ---------------------------------------------------------------------------------------------
// Extracted text: index scope, freshness, and the panel's commands.
//
// The scope rules live here rather than in `text_cli` so the panel and the CLI cannot drift apart
// about what "in the index" means — there is one answer to that question and both callers read it.
// ---------------------------------------------------------------------------------------------

fn text_dir_path(root: &Path) -> PathBuf {
    root.join(OCR_TEXT_DIR_NAME)
}

fn indexed_categories(config: &LibraryConfig) -> Vec<String> {
    config
        .text_index_categories
        .clone()
        .filter(|list| !list.is_empty())
        .unwrap_or_else(|| vec![text_index::DEFAULT_TEXT_CATEGORY.to_string()])
}

/// The index's source list, read straight off the stored records rather than a filesystem scan. A
/// search must not pay for a rescan: the records already hold the path, category and mtime the
/// index needs, and a file that moved since the last scan still has its text under the same hash.
fn text_index_sources(config: &LibraryConfig) -> Vec<text_index::SourceDoc> {
    let excluded: Vec<&str> = config
        .text_index_excluded_folders
        .iter()
        .map(String::as_str)
        .collect();

    config
        .images
        .iter()
        .filter_map(|(hash, record)| {
            let relative = record.last_known_path.replace('\\', "/");
            let (folder, name) = match relative.rsplit_once('/') {
                Some((parent, name)) => (
                    parent.split('/').next().unwrap_or(ROOT_SOURCE_FOLDER).to_string(),
                    name.to_string(),
                ),
                None => (ROOT_SOURCE_FOLDER.to_string(), relative.clone()),
            };
            if excluded.contains(&folder.as_str()) {
                return None;
            }
            Some(text_index::SourceDoc {
                hash: hash.clone(),
                relative_path: relative,
                name,
                folder,
                category: record.category.clone().unwrap_or_default(),
                modified_ms: record.modified_ms.unwrap_or_default(),
            })
        })
        .collect()
}

fn file_modified_ms(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

/// Why the index is out of date, or `None` when it is current. Answered from write ORDER — file
/// mtimes — rather than by re-reading the 2.2 MB records file, so it is cheap enough to run on
/// every query and on window focus. Same technique as `geo::status`, and the same trap: the
/// comparison must be strictly greater-than, because a build writes the index moments after
/// reading the sidecar and `>=` would make every build immediately accuse itself of being stale.
fn text_index_staleness(root: &Path, index: &text_index::TextIndex) -> Option<String> {
    if file_modified_ms(&root.join(SIDECAR_FILE_NAME))
        .map(|ms| ms > index.built_at_ms)
        .unwrap_or(false)
    {
        return Some("the library sidecar changed since the index was built".to_string());
    }
    if file_modified_ms(&text_dir_path(root))
        .map(|ms| ms > index.built_at_ms)
        .unwrap_or(false)
    {
        return Some("new text was extracted since the index was built".to_string());
    }
    None
}

fn build_text_index_for(root: &Path, config: &LibraryConfig) -> text_index::TextIndex {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();
    text_index::build(
        &text_index_sources(config),
        &text_dir_path(root),
        &indexed_categories(config),
        now_ms,
        &now_iso(),
    )
}

/// The parsed index, kept in memory between commands. Reparsing a multi-megabyte postings table on
/// every keystroke in the search box would make the panel feel broken; the cache is keyed by root
/// so switching libraries cannot serve the wrong one.
#[derive(Default)]
struct TextIndexCache {
    inner: std::sync::Mutex<Option<(PathBuf, text_index::TextIndex)>>,
}

/// Loads the index, building it when missing or stale. Building is allowed unprompted — it is
/// derived wholly from files already on disk. Running OCR is NOT, and never happens here: an
/// implicit extraction is thousands of model-free but minutes-long OCR calls nobody asked for.
fn text_index_for(root: &Path, cache: &TextIndexCache) -> Result<text_index::TextIndex, String> {
    if let Ok(guard) = cache.inner.lock() {
        if let Some((cached_root, index)) = guard.as_ref() {
            if cached_root == root && text_index_staleness(root, index).is_none() {
                return Ok(index.clone());
            }
        }
    }

    let existing = text_index::load(root);
    let fresh = match &existing {
        Some(index) if text_index_staleness(root, index).is_none() => existing.clone(),
        _ => None,
    };

    let index = match fresh {
        Some(index) => index,
        None => {
            let config = load_library_config(root);
            let built = build_text_index_for(root, &config);
            text_index::save(root, &built)?;
            built
        }
    };

    if let Ok(mut guard) = cache.inner.lock() {
        *guard = Some((root.to_path_buf(), index.clone()));
    }
    Ok(index)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TextStatusView {
    /// Images in an indexed category, and how many of those have had their text extracted. This is
    /// shown quietly: a fragmented screenshot corpus is *meant* to have holes, so coverage is a
    /// fact to have available, not a target to chase.
    in_scope: usize,
    extracted: usize,
    pending: usize,
    /// How many of `pending` an extraction run could actually reach. The analysis scope
    /// (`excludedAnalysisCategories` / `excludedAnalysisFolders`) is a separate axis from what the
    /// index covers, and on a real library they disagree: this one excludes **High Text** from
    /// analysis, which is precisely the category the index is built from. Counting coverage one way
    /// and acting another is how a button offers 5,265 images and then quietly does nothing — so
    /// both numbers are reported and the panel says which is which.
    reachable_pending: usize,
    /// Indexed categories switched off for analysis, and excluded folders holding pending images.
    /// Named rather than counted, because the fix is to name one and turn it back on.
    blocked_categories: Vec<String>,
    blocked_folders: Vec<String>,
    categories: Vec<String>,
    excluded_folders: Vec<String>,
    docs: usize,
    terms: usize,
    groups: usize,
    exact_dupes: usize,
    near_dupes: usize,
    total_chars: u64,
    built_at: Option<String>,
    stale_reason: Option<String>,
    span_from: Option<String>,
    span_to: Option<String>,
}

#[tauri::command]
fn get_text_status(root: String, cache: tauri::State<'_, TextIndexCache>) -> Result<TextStatusView, String> {
    let root = root_path(&root)?;
    let config = load_library_config(&root);
    let categories = indexed_categories(&config);

    // Mirrors `AnalysisScope` deliberately, off the stored records rather than a scan — the panel
    // must not pay for a rescan to answer "how much of this could I actually run".
    let excluded_categories = excluded_analysis_categories(&config);
    let excluded_folders: std::collections::HashSet<&str> = config
        .excluded_analysis_folders
        .iter()
        .map(String::as_str)
        .collect();

    let mut in_scope = 0usize;
    let mut extracted = 0usize;
    let mut reachable_pending = 0usize;
    let mut blocked_folders: std::collections::BTreeSet<String> = Default::default();

    for record in config.images.values() {
        let category = record.category.clone().unwrap_or_default();
        if !categories.contains(&category) {
            continue;
        }
        in_scope += 1;
        if record.ocr_text_chars.is_some() {
            extracted += 1;
            continue;
        }

        let relative = record.last_known_path.replace('\\', "/");
        let folder = match relative.rsplit_once('/') {
            Some((parent, _)) => parent.split('/').next().unwrap_or(ROOT_SOURCE_FOLDER).to_string(),
            None => ROOT_SOURCE_FOLDER.to_string(),
        };
        if excluded_categories.contains(category.as_str()) {
            continue;
        }
        if excluded_folders.contains(folder.as_str()) {
            blocked_folders.insert(folder);
            continue;
        }
        reachable_pending += 1;
    }

    let blocked_categories: Vec<String> = categories
        .iter()
        .filter(|category| excluded_categories.contains(category.as_str()))
        .cloned()
        .collect();

    // Deliberately does NOT build: opening the panel should never kick off work. The panel shows
    // what exists and offers the button.
    let index = {
        let cached = cache
            .inner
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().filter(|(path, _)| path == &root).map(|(_, index)| index.clone()));
        cached.or_else(|| text_index::load(&root))
    };
    let stale_reason = index.as_ref().and_then(|index| text_index_staleness(&root, index));
    let span = index.as_ref().and_then(|index| index.span());

    Ok(TextStatusView {
        in_scope,
        extracted,
        pending: in_scope.saturating_sub(extracted),
        reachable_pending,
        blocked_categories,
        blocked_folders: blocked_folders.into_iter().collect(),
        categories,
        excluded_folders: config.text_index_excluded_folders.clone(),
        docs: index.as_ref().map(|index| index.report.docs).unwrap_or(0),
        terms: index.as_ref().map(|index| index.report.terms).unwrap_or(0),
        groups: index.as_ref().map(|index| index.report.groups).unwrap_or(0),
        exact_dupes: index.as_ref().map(|index| index.report.exact_dupes).unwrap_or(0),
        near_dupes: index.as_ref().map(|index| index.report.near_dupes).unwrap_or(0),
        total_chars: index.as_ref().map(|index| index.total_chars).unwrap_or(0),
        built_at: index.as_ref().map(|index| index.built_at.clone()),
        stale_reason,
        span_from: span.map(|(first, _)| text_index::format_date(first)),
        span_to: span.map(|(_, last)| text_index::format_date(last)),
    })
}

#[tauri::command]
fn build_text_index(
    root: String,
    cache: tauri::State<'_, TextIndexCache>,
) -> Result<text_index::BuildReport, String> {
    let root = root_path(&root)?;
    let config = load_library_config(&root);
    let index = build_text_index_for(&root, &config);
    text_index::save(&root, &index)?;
    let report = index.report.clone();
    if let Ok(mut guard) = cache.inner.lock() {
        *guard = Some((root, index));
    }
    Ok(report)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TextHitView {
    hash: String,
    path: String,
    name: String,
    at: String,
    ts: i64,
    score: f32,
    chars: u32,
    rank: u8,
    terms: Vec<String>,
    exact_dupes: usize,
    near_dupes: usize,
    snippet: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TextSearchView {
    hits: Vec<TextHitView>,
    matched: usize,
    unknown_terms: Vec<String>,
    phrase_capped: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextQueryArgs {
    query: String,
    #[serde(default)]
    from: Option<i64>,
    #[serde(default)]
    to: Option<i64>,
    #[serde(default)]
    folders: Vec<String>,
    #[serde(default)]
    min_chars: u32,
    #[serde(default)]
    include_dupes: bool,
    #[serde(default)]
    require_all: bool,
    #[serde(default)]
    limit: usize,
}

impl TextQueryArgs {
    fn into_options(self) -> text_index::QueryOptions {
        text_index::QueryOptions {
            query: self.query,
            from: self.from,
            to: self.to,
            folders: self.folders,
            categories: Vec::new(),
            min_chars: self.min_chars,
            include_dupes: self.include_dupes,
            require_all: self.require_all,
            limit: self.limit,
        }
    }
}

#[tauri::command]
fn search_text(
    root: String,
    args: TextQueryArgs,
    snippet_width: usize,
    cache: tauri::State<'_, TextIndexCache>,
) -> Result<TextSearchView, String> {
    let root = root_path(&root)?;
    let index = text_index_for(&root, &cache)?;
    let texts = text_dir_path(&root);
    let terms = text_index::split_query(&args.query).0;
    let outcome = text_index::search(&index, &args.into_options(), Some(&texts));

    let width = snippet_width.clamp(80, 4000);
    let hits = outcome
        .hits
        .iter()
        .map(|hit| {
            let text = fs::read_to_string(text_index::text_file_path(&texts, &hit.hash)).unwrap_or_default();
            TextHitView {
                hash: hit.hash.clone(),
                path: hit.path.clone(),
                name: hit.name.clone(),
                at: text_index::format_datetime(hit.ts),
                ts: hit.ts,
                score: hit.score,
                chars: hit.chars,
                rank: hit.rank,
                terms: hit.terms.clone(),
                exact_dupes: hit.exact_dupes,
                near_dupes: hit.near_dupes,
                // The panel is the user reading their own screenshots, which is not an egress
                // event — so no redaction here. Every path that leaves the machine (the CLI, and
                // anything an agent reads) goes through `redact` instead.
                snippet: text_index::snippet(&text, &terms, width),
            }
        })
        .collect();

    Ok(TextSearchView {
        hits,
        matched: outcome.matched,
        unknown_terms: outcome.unknown_terms,
        phrase_capped: outcome.phrase_capped,
    })
}

#[tauri::command]
fn get_text_timeline(
    root: String,
    args: TextQueryArgs,
    bucket_hours: u32,
    cache: tauri::State<'_, TextIndexCache>,
) -> Result<Vec<text_index::Bucket>, String> {
    let root = root_path(&root)?;
    let index = text_index_for(&root, &cache)?;
    let width = bucket_hours.max(1);
    let mut buckets =
        topics::timeline_with_topics(&index, &args.into_options(), width, &topics::load(&root));
    // The hashes were only needed to compare each bucket's membership against the fingerprint its
    // topics were written under. Sending 8,000 of them to the panel afterwards is pure weight.
    for bucket in buckets.iter_mut() {
        bucket.hashes.clear();
    }
    Ok(buckets)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TopicRunProgress {
    processed: usize,
    total: usize,
    current_bucket: String,
    topics: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TopicStatusView {
    generated_at: Option<String>,
    model: Option<String>,
    buckets_with_topics: usize,
    buckets_total: usize,
    buckets_stale: usize,
    top_topics: Vec<(String, usize)>,
    top_notable: Vec<(String, usize)>,
}

#[tauri::command]
fn get_topic_status(
    root: String,
    bucket_hours: u32,
    cache: tauri::State<'_, TextIndexCache>,
) -> Result<TopicStatusView, String> {
    let root = root_path(&root)?;
    let width = bucket_hours.max(1);
    let file = topics::load(&root);
    let (top_topics, top_notable) = topics::vocabulary(&file, width);

    // Counted against the CURRENT buckets, not against what the file happens to hold: a stored
    // bucket for a span that no longer has images is not coverage.
    let index = text_index_for(&root, &cache)?;
    let mut buckets = text_index::timeline(&index, &text_index::QueryOptions::default(), width, true);
    topics::apply(&mut buckets, &file, width);

    Ok(TopicStatusView {
        generated_at: (!file.generated_at.is_empty()).then(|| file.generated_at.clone()),
        model: (!file.model.is_empty()).then(|| file.model.clone()),
        buckets_with_topics: buckets.iter().filter(|bucket| !bucket.topics.is_empty()).count(),
        buckets_total: buckets.len(),
        buckets_stale: buckets.iter().filter(|bucket| bucket.topics_stale).count(),
        top_topics: top_topics.into_iter().take(24).collect(),
        top_notable: top_notable.into_iter().take(24).collect(),
    })
}

#[tauri::command]
fn generate_topics(
    app: AppHandle,
    root: String,
    bucket_hours: u32,
    force: bool,
    control: tauri::State<'_, TopicControl>,
) -> Result<(), String> {
    let root_buf = root_path(&root)?;
    if control.running.swap(true, Ordering::SeqCst) {
        return Err("Topics are already being generated.".to_string());
    }
    control.cancel.store(false, Ordering::SeqCst);

    let app_handle = app.clone();
    std::thread::spawn(move || {
        run_topic_generation(&app_handle, &root_buf, bucket_hours.max(1), force);
    });
    Ok(())
}

#[tauri::command]
fn cancel_topics(control: tauri::State<'_, TopicControl>) -> Result<(), String> {
    if !control.running.load(Ordering::SeqCst) {
        return Err("No topic run is in progress.".to_string());
    }
    control.cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// Names what each time bucket was about, one model call per bucket. Mirrors `run_kind_classification`:
/// detached thread, progress events, cancellable, and it saves what it has on the way out so a
/// stopped run keeps every bucket it already paid for.
fn run_topic_generation(app: &AppHandle, root: &Path, width_hours: u32, force: bool) {
    let control = app.state::<TopicControl>();

    let result = (|| -> Result<(&'static str, Option<String>), String> {
        let settings = load_app_settings(app);
        let endpoint = vision_endpoint(&settings);
        let model = vision_model(&settings);
        let api_key = vision_api_key(&settings);

        let cache = app.state::<TextIndexCache>();
        let index = text_index_for(root, &cache)?;
        if index.docs.is_empty() {
            return Ok(("completed", Some("No extracted text is indexed yet.".to_string())));
        }

        let texts = text_dir_path(root);
        let buckets = text_index::timeline(&index, &text_index::QueryOptions::default(), width_hours, true);
        let mut file = topics::load(root);
        let pending: Vec<text_index::Bucket> =
            topics::pending(&buckets, &file, width_hours, force).into_iter().cloned().collect();

        if pending.is_empty() {
            return Ok((
                "completed",
                Some("Every bucket already has topics for this width.".to_string()),
            ));
        }

        let agent = vision::build_agent();
        let total = pending.len();
        let mut processed = 0usize;
        let mut failures = 0usize;

        for bucket in &pending {
            if control.cancel.load(Ordering::SeqCst) {
                topics::save(root, &file)?;
                return Ok(("cancelled", Some(format!("Stopped after {processed} of {total}."))));
            }

            let (prompt, sampled) = topics::prepare(
                bucket,
                |hash| fs::read_to_string(text_index::text_file_path(&texts, hash)).ok(),
                |hash| index.doc_by_hash(hash).map(|(_, doc)| doc.terms.clone()).unwrap_or_default(),
                |hash| {
                    index
                        .doc_by_hash(hash)
                        .map(|(_, doc)| text_index::format_datetime(doc.ts))
                        .unwrap_or_default()
                },
                topics::DEFAULT_PROMPT_BUDGET,
            );

            if sampled == 0 {
                failures += 1;
                processed += 1;
                continue;
            }

            // Claims the model only when this app is the one loading it, exactly as Describe and
            // Classify do — a run must never shorten the idle life of a load somebody else owns.
            let reply = model_lease::with_claim(app, &model, || {
                topics::ask(&agent, &endpoint, &model, api_key.as_deref(), &prompt)
            });

            match reply {
                Ok((topic_list, notable)) => {
                    let _ = app.emit(
                        "topics-progress",
                        TopicRunProgress {
                            processed: processed + 1,
                            total,
                            current_bucket: bucket.id.clone(),
                            topics: topic_list.clone(),
                        },
                    );
                    topics::record(
                        &mut file, width_hours, bucket, topic_list, notable, sampled, &model, &now_iso(),
                    );
                }
                Err(error) => {
                    failures += 1;
                    eprintln!("Topic generation failed for {}: {error}", bucket.id);
                    let _ = app.emit(
                        "topics-progress",
                        TopicRunProgress {
                            processed: processed + 1,
                            total,
                            current_bucket: bucket.id.clone(),
                            topics: Vec::new(),
                        },
                    );
                }
            }

            processed += 1;
            // Checkpointed per bucket rather than at the end. A bucket costs a model call; losing
            // forty of them to a crash on the forty-first is not a trade worth making for one write
            // of a file this small.
            topics::save(root, &file)?;
        }

        let message = if failures > 0 {
            Some(format!("Named {} of {total} buckets; {failures} failed.", total - failures))
        } else {
            Some(format!("Named {total} buckets."))
        };
        Ok(("completed", message))
    })();

    control.running.store(false, Ordering::SeqCst);

    let (status, message) = match result {
        Ok((status, message)) => (status.to_string(), message),
        Err(error) => ("error".to_string(), Some(error)),
    };
    let _ = app.emit("topics-finished", TextAnalysisFinished { status, message });
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TextGroupMemberView {
    hash: String,
    path: String,
    at: String,
    rank: u8,
    novel_lines: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TextDocumentView {
    hash: String,
    path: String,
    name: String,
    at: String,
    category: String,
    folder: String,
    chars: u32,
    rank: u8,
    terms: Vec<String>,
    text: String,
    members: Vec<TextGroupMemberView>,
}

#[tauri::command]
fn get_image_text(
    root: String,
    hash: String,
    cache: tauri::State<'_, TextIndexCache>,
) -> Result<TextDocumentView, String> {
    let root = root_path(&root)?;
    let index = text_index_for(&root, &cache)?;
    let texts = text_dir_path(&root);
    let (doc_index, doc) = index
        .doc_by_hash(&hash)
        .ok_or_else(|| "That image is not in the text index.".to_string())?;

    let text = fs::read_to_string(text_index::text_file_path(&texts, &doc.hash))
        .map_err(|error| format!("No extracted text for this image: {error}"))?;

    let member_indexes = index.group_members(doc_index);
    let representative = fs::read_to_string(text_index::text_file_path(
        &texts,
        &index.docs[member_indexes[0]].hash,
    ))
    .unwrap_or_default();

    // Only the differences are carried back. A near-duplicate is usually the same screen scrolled
    // a little, and those added lines are the entire reason it was kept — showing the whole thing
    // again would bury them.
    let members = member_indexes
        .iter()
        .skip(1)
        .map(|member| {
            let member_doc = &index.docs[*member];
            let member_text =
                fs::read_to_string(text_index::text_file_path(&texts, &member_doc.hash)).unwrap_or_default();
            TextGroupMemberView {
                hash: member_doc.hash.clone(),
                path: member_doc.path.clone(),
                at: text_index::format_datetime(member_doc.ts),
                rank: member_doc.rank,
                novel_lines: text_index::novel_lines(&representative, &member_text),
            }
        })
        .collect();

    Ok(TextDocumentView {
        hash: doc.hash.clone(),
        path: doc.path.clone(),
        name: doc.name.clone(),
        at: text_index::format_datetime(doc.ts),
        category: doc.category.clone(),
        folder: doc.folder.clone(),
        chars: doc.chars,
        rank: doc.rank,
        terms: doc.terms.clone(),
        text,
        members,
    })
}

/// Assigns a category to every hash given. Separate from `assign_category` because the panel acts
/// on a whole result set, and doing that one IPC call at a time would rewrite the sidecar per image.
#[tauri::command]
fn categorize_images(root: String, hashes: Vec<String>, category: String) -> Result<usize, String> {
    let root = root_path(&root)?;
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does a second instance of
    // this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);
    if !config.categories.iter().any(|item| item == &category) {
        return Err("Category does not exist.".to_string());
    }

    let stamp = now_iso();
    let mut changed = 0usize;
    for hash in &hashes {
        if let Some(record) = config.images.get_mut(hash) {
            if record.category.as_deref() == Some(category.as_str()) {
                continue;
            }
            record.category = Some(category.clone());
            record.classified_by = Some("manual".to_string());
            record.classified_at = Some(stamp.clone());
            changed += 1;
        }
    }
    save_library_config(&root, &config)?;
    Ok(changed)
}

#[tauri::command]
fn set_text_index_folder_included(root: String, folder_name: String, included: bool) -> Result<(), String> {
    let root = root_path(&root)?;
    let _sidecar_lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);
    if included {
        config.text_index_excluded_folders.retain(|item| item != &folder_name);
    } else if !config.text_index_excluded_folders.iter().any(|item| item == &folder_name) {
        config.text_index_excluded_folders.push(folder_name);
    }
    save_library_config(&root, &config)
}

#[tauri::command]
fn remove_manual_source_folder(root: String, folder_name: String) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);
    config.manual_source_folders.retain(|item| item != &folder_name);
    save_library_config(&root, &config)?;
    scan_and_reconcile(&root)
}

#[tauri::command]
fn set_folder_analysis_included(root: String, folder_name: String, included: bool) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);
    config.excluded_analysis_folders.retain(|item| item != &folder_name);
    if !included {
        config.excluded_analysis_folders.push(folder_name);
    }
    save_library_config(&root, &config)?;
    scan_and_reconcile(&root)
}

#[tauri::command]
fn set_category_analysis_included(root: String, category_name: String, included: bool) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);
    config.excluded_analysis_categories.retain(|item| item != &category_name);
    if !included {
        config.excluded_analysis_categories.push(category_name);
    }
    save_library_config(&root, &config)?;
    scan_and_reconcile(&root)
}

/// The set of categories switched OFF for analysis. Images whose current category is in this set are
/// skipped by every analysis pass — the category analog of `excluded_analysis_folders`. Lets the
/// user stop spending analysis (chiefly vision tokens) on images already well-handled, e.g. "High
/// Text", which OCR already captured, so Describe can focus on low-text images and deduped frames.
fn excluded_analysis_categories(config: &LibraryConfig) -> std::collections::HashSet<String> {
    config.excluded_analysis_categories.iter().cloned().collect()
}

/// True when `hash`'s current category is one the user excluded from analysis.
fn category_is_excluded(
    config: &LibraryConfig,
    hash: &str,
    excluded: &std::collections::HashSet<String>,
) -> bool {
    !excluded.is_empty()
        && config
            .images
            .get(hash)
            .and_then(|record| record.category.as_deref())
            .map(|category| excluded.contains(category))
            .unwrap_or(false)
}

#[tauri::command]
fn create_category(root: String, name: String) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    let name = validate_child_name(&name, "Category")?;
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);
    if config.categories.iter().any(|item| item.eq_ignore_ascii_case(&name)) {
        return Err("A category with that name already exists.".to_string());
    }
    config.categories.push(name);
    save_library_config(&root, &config)?;
    scan_and_reconcile(&root)
}

#[tauri::command]
fn rename_category(root: String, old_name: String, new_name: String) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    let new_name = validate_child_name(&new_name, "Category")?;
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);

    if !config.categories.iter().any(|item| item == &old_name) {
        return Err("Category does not exist.".to_string());
    }
    if old_name != new_name && config.categories.iter().any(|item| item.eq_ignore_ascii_case(&new_name)) {
        return Err("A category with that name already exists.".to_string());
    }

    for item in config.categories.iter_mut() {
        if item == &old_name {
            *item = new_name.clone();
        }
    }
    for record in config.images.values_mut() {
        if record.category.as_deref() == Some(old_name.as_str()) {
            record.category = Some(new_name.clone());
        }
    }
    save_library_config(&root, &config)?;
    scan_and_reconcile(&root)
}

#[tauri::command]
fn delete_category(root: String, name: String) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);
    config.categories.retain(|item| item != &name);
    for record in config.images.values_mut() {
        if record.category.as_deref() == Some(name.as_str()) {
            record.category = None;
            record.classified_by = None;
        }
    }
    save_library_config(&root, &config)?;
    scan_and_reconcile(&root)
}

/// Records one manual classification and nothing else. Deliberately does NOT return a `LibraryView`:
/// re-scanning to answer a single click meant re-reading every image in the library and shipping the
/// whole thing back over IPC. The caller already knows which image changed and patches its own copy,
/// so this only persists the edit and reports the timestamp it stamped.
#[tauri::command]
fn assign_category(root: String, hash: String, category: Option<String>) -> Result<AssignResult, String> {
    let root = root_path(&root)?;
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);

    if let Some(category) = &category {
        if !config.categories.iter().any(|item| item == category) {
            return Err("Category does not exist.".to_string());
        }
    }

    let assigned = category.is_some();
    let classified_at = assigned.then(now_iso);
    let record = config.images.entry(hash).or_default();
    if assigned {
        record.category = category;
        record.classified_by = Some("manual".to_string());
        record.classified_at = classified_at.clone();
    } else {
        record.category = None;
        record.classified_by = None;
        record.classified_at = None;
    }

    save_library_config(&root, &config)?;
    Ok(AssignResult {
        classified_by: assigned.then(|| "manual".to_string()),
        classified_at,
    })
}

/// Picks a free filename in `target_dir` for `file_name`, suffixing " (2)", " (3)", … on collision.
fn unique_destination(target_dir: &Path, file_name: &str) -> PathBuf {
    let mut destination = target_dir.join(file_name);
    if !destination.exists() {
        return destination;
    }
    let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("image").to_string();
    let ext = Path::new(file_name).extension().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let mut counter = 2;
    while destination.exists() {
        let candidate_name = if ext.is_empty() {
            format!("{stem} ({counter})")
        } else {
            format!("{stem} ({counter}).{ext}")
        };
        destination = target_dir.join(candidate_name);
        counter += 1;
    }
    destination
}

/// Flattens whatever was dropped or picked into a list of image files: plain files pass through,
/// folders are walked. Unreadable entries are skipped rather than failing the whole import.
fn collect_import_sources(path: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if is_image_path(path) {
            out.push(path.to_path_buf());
        }
        return;
    }
    if !path.is_dir() || depth >= MAX_IMPORT_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let child = entry.path();
        if path_name(&child).starts_with('.') {
            continue;
        }
        collect_import_sources(&child, depth + 1, out);
    }
}

/// Copies dropped or picked images — and any images inside dropped folders — into `target_folder`
/// under the root, then registers that folder as a source so the imports are visible even when its
/// name doesn't match the library's source pattern.
///
/// Copies rather than moves: the sources belong to something else (a download folder, a phone dump,
/// another tool's output) and emptying them out from under their owner isn't this app's call.
#[tauri::command]
fn import_images(root: String, target_folder: String, paths: Vec<String>) -> Result<ImportReport, String> {
    let root_buf = root_path(&root)?;
    let target_name = validate_child_name(&target_folder, "Folder")?;

    // Work out what there is to copy before creating anything, so a drop that turns out to hold no
    // images doesn't leave an empty folder behind as a souvenir.
    let mut sources: Vec<PathBuf> = Vec::new();
    for path in &paths {
        collect_import_sources(Path::new(path), 0, &mut sources);
    }
    if sources.is_empty() {
        return Err("Nothing to import — no image files were found.".to_string());
    }

    // Anything already under the root is in the library; copying it back in would just duplicate it.
    let (inside, to_copy): (Vec<PathBuf>, Vec<PathBuf>) =
        sources.into_iter().partition(|source| source.starts_with(&root_buf));
    let mut skipped = inside.len();
    if to_copy.is_empty() {
        return Err("Everything you dropped is already in this library.".to_string());
    }

    let target_dir = root_buf.join(&target_name);
    let target_existed = target_dir.is_dir();
    fs::create_dir_all(&target_dir).map_err(|error| format!("Failed to create import folder: {error}"))?;

    let mut imported = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for source in &to_copy {
        let file_name = path_name(source);
        let destination = unique_destination(&target_dir, &file_name);
        match fs::copy(source, &destination) {
            Ok(_) => imported += 1,
            Err(error) => {
                if errors.len() < MAX_IMPORT_ERRORS {
                    errors.push(format!("{file_name}: {error}"));
                }
                skipped += 1;
            }
        }
    }

    // Every copy failed, so the folder we just made is empty and was never wanted. Only clear up
    // after ourselves — `remove_dir` refuses a non-empty directory, but a folder the user already
    // had is not ours to remove even when it happens to be empty.
    if imported == 0 && !target_existed {
        let _ = fs::remove_dir(&target_dir);
    }

    if imported > 0 {
        // Cross-process: image-viewer-tauri writes this same sidecar, and so does
        // a second instance of this app. Held for the whole read-modify-write.
        let _sidecar_lock = SidecarLock::acquire(&root_buf);
        let mut config = load_library_config(&root_buf);
        if !config.manual_source_folders.iter().any(|item| item == &target_name) {
            config.manual_source_folders.push(target_name.clone());
            // The files are already copied. If registering the folder fails, say so but still
            // report the import — propagating here would discard the count and leave the caller
            // thinking nothing happened, when in fact the images are on disk.
            if let Err(error) = save_library_config(&root_buf, &config) {
                errors.push(format!("Copied the images, but failed to register {target_name} as a source folder: {error}"));
            }
        }
    }

    Ok(ImportReport {
        imported,
        skipped,
        target_folder: target_name,
        errors,
    })
}

/// Moves one image file into `target_folder`.
///
/// `relative_path` identifies *which file* to move. It can't be derived from the hash: records are
/// keyed by hash, so duplicate files share one record whose `last_known_path` points at whichever
/// copy the last scan happened to visit. Resolving the file from the record therefore moved the
/// wrong copy — click Move on one duplicate and a different one silently moved instead.
#[tauri::command]
fn move_image(
    root: String,
    hash: String,
    relative_path: String,
    target_folder: String,
) -> Result<LibraryView, String> {
    let root_buf = root_path(&root)?;

    // The path comes from the frontend, so confine it to the library before touching the disk.
    let source = root_buf.join(relative_path.replace('/', "\\"));
    let canonical_root = root_buf
        .canonicalize()
        .map_err(|error| format!("Failed to resolve root: {error}"))?;
    let canonical_source = source
        .canonicalize()
        .map_err(|_| "Source file no longer exists at the known path.".to_string())?;
    if !canonical_source.starts_with(&canonical_root) {
        return Err("That image is not inside the library root.".to_string());
    }
    if !canonical_source.is_file() {
        return Err("Source file no longer exists at the known path.".to_string());
    }
    let source = canonical_source;

    let target_name = validate_child_name(&target_folder, "Folder")?;
    let target_dir = root_buf.join(&target_name);
    fs::create_dir_all(&target_dir).map_err(|error| format!("Failed to create target folder: {error}"))?;

    let file_name = path_name(&source);
    // `source` is canonicalized (`\\?\D:\...` on Windows) so it can never compare equal to a plain
    // `target_dir.join(name)`. Compare canonical parent to canonical target instead, or a file
    // already sitting in the destination would be "moved" onto itself as a spurious " (2)" copy.
    let canonical_target = target_dir
        .canonicalize()
        .map_err(|error| format!("Failed to resolve target folder: {error}"))?;
    let destination = if source.parent() == Some(canonical_target.as_path()) {
        source.clone()
    } else {
        let candidate = unique_destination(&target_dir, &file_name);
        fs::rename(&source, &candidate).map_err(|error| format!("Failed to move file: {error}"))?;
        candidate
    };

    let new_relative = destination
        .strip_prefix(&root_buf)
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| format!("{target_name}/{}", path_name(&destination)));

    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root_buf);
    let mut config = load_library_config(&root_buf);
    if let Some(record) = config.images.get_mut(&hash) {
        // Only re-point the record if it was tracking the file we actually moved; with duplicates it
        // may be tracking a different copy, which is still exactly where it was.
        if record.last_known_path == relative_path {
            record.last_known_path = new_relative;
        }
    }
    save_library_config(&root_buf, &config)?;
    scan_and_reconcile(&root_buf)
}

#[tauri::command]
fn open_image(file_path: String) -> Result<(), String> {
    let path = PathBuf::from(file_path);
    if !path.is_file() {
        return Err("File does not exist.".to_string());
    }
    launch_path(&path)
}

#[tauri::command]
fn reveal_image(file_path: String) -> Result<(), String> {
    let path = PathBuf::from(file_path);
    if !path.exists() {
        return Err("File does not exist.".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        let path = path.canonicalize().map_err(|error| format!("Failed to resolve file location: {error}"))?;
        // Absolute for the same reason as `launch_path`. Explorer is not a shell, so `&` in the
        // path is inert here and only the binary's identity was ever in question.
        Command::new(windows_dir().join("explorer.exe"))
            .arg(format!("/select,\"{}\"", path.to_string_lossy()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("Failed to reveal file: {error}"))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        launch_path(parent)
    }
}

#[tauri::command]
fn open_root_folder(root: String) -> Result<(), String> {
    let path = root_path(&root)?;
    launch_path(&path)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NsfwModelInfo {
    path: String,
    exists: bool,
    candidates: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NsfwModelDownloadReport {
    info: NsfwModelInfo,
    source_url: String,
    downloaded_bytes: u64,
    model_bytes: u64,
    report: Vec<String>,
}

#[tauri::command]
fn get_nsfw_model_info(app: AppHandle) -> Result<NsfwModelInfo, String> {
    nsfw_model_info(&app)
}

#[tauri::command]
fn download_nsfw_model(app: AppHandle) -> Result<NsfwModelDownloadReport, String> {
    if nsfw_model_path(&app).is_some() {
        return Ok(NsfwModelDownloadReport {
            info: nsfw_model_info(&app)?,
            source_url: NUDENET_MODEL_DOWNLOAD_URL.to_string(),
            downloaded_bytes: 0,
            model_bytes: 0,
            report: vec!["Model already exists; no download needed.".to_string()],
        });
    }

    let (downloaded_bytes, model_bytes, mut report) = download_nsfw_model_file(&app)?;
    report.push("Model installed and ready for explicit analysis.".to_string());
    Ok(NsfwModelDownloadReport {
        info: nsfw_model_info(&app)?,
        source_url: NUDENET_MODEL_DOWNLOAD_URL.to_string(),
        downloaded_bytes,
        model_bytes,
        report,
    })
}

fn download_nsfw_model_file(app: &AppHandle) -> Result<(u64, u64, Vec<String>), String> {
    let target = nsfw_model_download_path(app)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create model directory: {error}"))?;
    }

    let wheel_path = target.with_extension("whl.download");
    let model_temp_path = target.with_extension("onnx.download");
    let mut report = vec![
        "Source: NudeNet 3.4.2 PyPI wheel.".to_string(),
        format!("Destination: {}", target.display()),
    ];

    let response = ureq::get(NUDENET_MODEL_DOWNLOAD_URL)
        .set("User-Agent", "Image-Categorizer/1.0")
        .call()
        .map_err(|error| format!("Failed to download NudeNet package: {error}"))?;
    let status = response.status();
    let content_type = response
        .header("content-type")
        .unwrap_or("unknown")
        .to_string();
    report.push(format!("HTTP status: {status}; content-type: {content_type}"));

    let mut reader = response.into_reader();
    let mut file = File::create(&wheel_path)
        .map_err(|error| format!("Failed to create temporary package file: {error}"))?;
    let bytes = io::copy(&mut reader, &mut file)
        .map_err(|error| format!("Failed to save NudeNet package: {error}"))?;
    drop(file);
    report.push(format!("Downloaded package: {bytes} bytes"));

    if bytes < 1_000_000 {
        let preview = fs::read(&wheel_path)
            .ok()
            .map(|data| String::from_utf8_lossy(&data[..data.len().min(240)]).to_string())
            .unwrap_or_default();
        let _ = fs::remove_file(&wheel_path);
        return Err(format!(
            "Downloaded NudeNet package was unexpectedly small ({bytes} bytes). HTTP status: {status}; content-type: {content_type}. Response preview: {preview}"
        ));
    }

    let wheel_file = File::open(&wheel_path)
        .map_err(|error| format!("Failed to open downloaded NudeNet package: {error}"))?;
    let mut archive = zip::ZipArchive::new(wheel_file)
        .map_err(|error| format!("Downloaded NudeNet package is not a valid wheel archive: {error}"))?;
    let mut model_entry = archive
        .by_name("nudenet/320n.onnx")
        .map_err(|error| format!("NudeNet package did not contain nudenet/320n.onnx: {error}"))?;
    let mut model_file = File::create(&model_temp_path)
        .map_err(|error| format!("Failed to create temporary model file: {error}"))?;
    let model_bytes = io::copy(&mut model_entry, &mut model_file)
        .map_err(|error| format!("Failed to extract NudeNet model: {error}"))?;
    drop(model_file);
    drop(model_entry);
    drop(archive);
    report.push(format!("Extracted model: {model_bytes} bytes"));

    if model_bytes < 1_000_000 {
        let _ = fs::remove_file(&model_temp_path);
        let _ = fs::remove_file(&wheel_path);
        return Err(format!("Extracted NudeNet model was unexpectedly small ({model_bytes} bytes)."));
    }

    fs::rename(&model_temp_path, &target)
        .map_err(|error| format!("Failed to install NudeNet model: {error}"))?;
    let _ = fs::remove_file(&wheel_path);
    Ok((bytes, model_bytes, report))
}

fn nsfw_model_info(app: &AppHandle) -> Result<NsfwModelInfo, String> {
    let candidates = nsfw_model_candidates(app)?;
    let path = nsfw_model_path(app).unwrap_or_else(|| candidates[0].clone());
    Ok(NsfwModelInfo {
        exists: path.is_file(),
        path: path.to_string_lossy().to_string(),
        candidates: candidates
            .into_iter()
            .map(|candidate| candidate.to_string_lossy().to_string())
            .collect(),
    })
}

fn nsfw_model_download_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data dir: {e}"))?
        .join(NUDENET_MODEL_DOWNLOAD_FILENAME))
}

fn nsfw_model_candidates(app: &AppHandle) -> Result<Vec<PathBuf>, String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data dir: {e}"))?;

    let mut dirs = vec![app_data_dir];
    if let Ok(resource_dir) = app.path().resource_dir() {
        dirs.push(resource_dir);
    }
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            dirs.push(exe_dir.to_path_buf());
        }
    }

    let mut candidates = Vec::new();
    for dir in dirs {
        for filename in NUDENET_MODEL_FILENAMES {
            candidates.push(dir.join(filename));
        }
    }
    Ok(candidates)
}

fn nsfw_model_path(app: &AppHandle) -> Option<PathBuf> {
    nsfw_model_candidates(app)
        .ok()?
        .into_iter()
        .find(|path| path.is_file())
}

fn nsfw_threshold(config: &LibraryConfig) -> f32 {
    config.nsfw_score_threshold.unwrap_or(DEFAULT_NSFW_THRESHOLD)
}

fn reclassify_nsfw_categories(config: &mut LibraryConfig) {
    let any_analyzed = config.images.values().any(|r| r.nsfw_score.is_some());
    if !any_analyzed {
        return;
    }
    let threshold = nsfw_threshold(config);
    ensure_category(config, EXPLICIT_CATEGORY);

    for record in config.images.values_mut() {
        if record.classified_by.as_deref() == Some("manual") {
            continue;
        }
        let Some(score) = record.nsfw_score else {
            continue;
        };
        if score >= threshold {
            if record.category.as_deref() != Some(EXPLICIT_CATEGORY) {
                record.category = Some(EXPLICIT_CATEGORY.to_string());
                record.classified_by = Some("auto-nsfw".to_string());
                record.classified_at = Some(now_iso());
            }
        } else if record.classified_by.as_deref() == Some("auto-nsfw") {
            // Threshold was raised and image is now below it — release back to auto pipeline
            record.category = None;
            record.classified_by = None;
            record.classified_at = None;
        }
    }
}

fn run_nsfw_analysis(app: &AppHandle, root_buf: &Path, force: bool) {
    let control = app.state::<NsfwControl>();

    let result = (|| -> Result<(&'static str, Option<String>), String> {
        let Some(model_path) = nsfw_model_path(app) else {
            let target = nsfw_model_download_path(app)?;
            return Ok((
                "error",
                Some(format!(
                    "NudeNet model is not installed. Open Settings, press Download Model, then run explicit analysis again. Target path: {}",
                    target.display()
                )),
            ));
        };

        let mut session = create_session(&model_path)?;
        let view = scan_and_reconcile(root_buf)?;
        let config = load_library_config(root_buf);

        let pending: Vec<(String, String, String)> = pending_nsfw(&view, &config, force)
            .into_iter()
            .map(|img| (img.hash.clone(), img.path.clone(), img.name.clone()))
            .collect();
        drop(config);

        let total = pending.len();
        let mut cancelled = false;
        let mut results: Vec<(String, f32, Vec<String>)> = Vec::new();

        for (index, (hash, path, name)) in pending.iter().enumerate() {
            if control.cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            match analyze_image_nsfw(&mut session, Path::new(path)) {
                Ok(stats) => results.push((hash.clone(), stats.score, stats.labels)),
                Err(e) => {
                    results.push((hash.clone(), 0.0, vec![format!("NSFW analysis error: {e}")]));
                    eprintln!("NSFW analysis failed for {path}: {e}");
                }
            }

            if results.len() >= ANALYSIS_CHECKPOINT_EVERY {
                commit_nsfw_results(root_buf, &mut results)?;
            }

            let _ = app.emit(
                "nsfw-analysis-progress",
                TextAnalysisProgress {
                    processed: index + 1,
                    total,
                    current_name: name.clone(),
                },
            );
        }

        commit_nsfw_results(root_buf, &mut results)?;

        let message = if total == 0 { Some("No images needed NSFW analysis.".to_string()) } else { None };
        Ok((if cancelled { "cancelled" } else { "completed" }, message))
    })();

    control.running.store(false, Ordering::SeqCst);

    let (status, message) = match result {
        Ok((s, m)) => (s.to_string(), m),
        Err(e) => ("error".to_string(), Some(e)),
    };
    let _ = app.emit("nsfw-analysis-finished", TextAnalysisFinished { status, message });
}

#[tauri::command]
fn analyze_nsfw(app: AppHandle, control: tauri::State<'_, NsfwControl>, root: String, force: bool) -> Result<(), String> {
    if control.running.swap(true, Ordering::SeqCst) {
        return Err("NSFW analysis is already running.".to_string());
    }
    let root_buf = match root_path(&root) {
        Ok(p) => p,
        Err(e) => {
            control.running.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    control.cancel.store(false, Ordering::SeqCst);
    let app_handle = app.clone();
    std::thread::spawn(move || run_nsfw_analysis(&app_handle, &root_buf, force));
    Ok(())
}

#[tauri::command]
fn cancel_nsfw_analysis(control: tauri::State<'_, NsfwControl>) -> Result<(), String> {
    if !control.running.load(Ordering::SeqCst) {
        return Err("No NSFW analysis is running.".to_string());
    }
    control.cancel.store(true, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
fn set_nsfw_threshold(root: String, threshold: f32) -> Result<LibraryView, String> {
    let root_buf = root_path(&root)?;
    // Cross-process: image-viewer-tauri writes this same sidecar, and so does
    // a second instance of this app. Held for the whole read-modify-write.
    let _sidecar_lock = SidecarLock::acquire(&root_buf);
    let mut config = load_library_config(&root_buf);
    config.nsfw_score_threshold = Some(threshold.clamp(0.0, 1.0));
    reclassify_nsfw_categories(&mut config);
    reclassify_text_categories(&mut config);
    save_library_config(&root_buf, &config)?;
    scan_and_reconcile(&root_buf)
}

fn validate_time_of_day(value: &str) -> Result<String, String> {
    let parts: Vec<&str> = value.split(':').collect();
    let [hour_str, minute_str] = parts[..] else {
        return Err("Time must be in HH:MM format.".to_string());
    };
    let hour: u32 = hour_str.parse().map_err(|_| "Invalid hour.".to_string())?;
    let minute: u32 = minute_str.parse().map_err(|_| "Invalid minute.".to_string())?;
    if hour > 23 || minute > 59 {
        return Err("Time must be between 00:00 and 23:59.".to_string());
    }
    Ok(format!("{hour:02}:{minute:02}"))
}

// Installs, updates, or removes the daily Windows Task Scheduler entry that reinvokes this same
// exe with `--headless-refresh`. The task is authoritative only for *when* the job fires — the
// job itself re-reads `auto_refresh_enabled` on every run and no-ops if it's off, so disabling the
// feature in Settings is always the final word even if the scheduled task somehow survives.
// The task is registered from XML so it can state outright that it must not wake a sleeping
// machine — see `auto_refresh_task_xml` for why that cannot be expressed any other way.
fn configure_scheduled_task(enabled: bool, time: &str) -> Result<(), String> {
    if !enabled {
        let _ = Command::new("schtasks")
            .args(["/Delete", "/F", "/TN", AUTO_REFRESH_TASK_NAME])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        return Ok(());
    }

    let exe = std::env::current_exe().map_err(|error| format!("Failed to resolve executable path: {error}"))?;
    let xml_path = std::env::temp_dir().join(format!("{AUTO_REFRESH_TASK_NAME}-{}.xml", std::process::id()));
    // schtasks /XML reads UTF-16LE with a BOM — the encoding `Export-ScheduledTask` emits. A
    // UTF-8 file is rejected outright as malformed.
    write_utf16le_bom(&xml_path, &auto_refresh_task_xml(&exe, time))
        .map_err(|error| format!("Failed to stage the scheduled task definition: {error}"))?;

    let status = Command::new("schtasks")
        .arg("/Create")
        .arg("/F")
        .arg("/TN")
        .arg(AUTO_REFRESH_TASK_NAME)
        .arg("/XML")
        .arg(&xml_path)
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    let _ = fs::remove_file(&xml_path);

    let status = status.map_err(|error| format!("Failed to run schtasks: {error}"))?;
    if !status.success() {
        return Err("schtasks failed to create the scheduled task.".to_string());
    }
    Ok(())
}

// Escapes the five XML metacharacters. The task definition embeds a filesystem path and a
// `DOMAIN\user`, neither of which is guaranteed free of `&`.
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// Writes UTF-16LE with a byte-order mark, the only encoding `schtasks /XML` accepts.
fn write_utf16le_bom(path: &Path, contents: &str) -> io::Result<()> {
    let mut bytes = Vec::with_capacity(contents.len() * 2 + 2);
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    for unit in contents.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(path, bytes)
}

// The full task definition, registered through `/XML` rather than schtasks' flag form for one
// reason: the flag form has no way to express `<WakeToRun>`, so it can only ever *inherit* the
// default. That default is False (measured), but nothing in the code said so and nothing stopped
// it drifting. A nightly job is precisely the shape that tempts someone into arming a wake timer,
// so both power-relevant settings are asserted explicitly here instead of left implicit:
//
//   WakeToRun=false          — never bring the machine out of sleep to run this. The refresh is
//                              a convenience pass over a local image library; it is never worth
//                              spinning a sleeping desktop up at 04:00 for.
//   StartWhenAvailable=true  — the necessary other half. Because the task will not wake the
//                              machine, a desktop asleep at the scheduled time would otherwise
//                              skip the pass entirely and silently, every single night. This
//                              runs the missed pass once the user wakes the machine themselves.
//
// The trigger's StartBoundary is a fixed past date because a daily trigger only reads the
// time-of-day from it and rolls forward; registering it does not fire an immediate catch-up run.
fn auto_refresh_task_xml(exe: &Path, time: &str) -> String {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(name)) => format!("{domain}\\{name}"),
        (Err(_), Ok(name)) => name,
        _ => String::new(),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.3" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Image Categorizer nightly refresh. Never wakes the machine.</Description>
  </RegistrationInfo>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <UseUnifiedSchedulingEngine>true</UseUnifiedSchedulingEngine>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
  </Settings>
  <Triggers>
    <CalendarTrigger>
      <StartBoundary>2020-01-01T{time}:00</StartBoundary>
      <Enabled>true</Enabled>
      <ScheduleByDay>
        <DaysInterval>1</DaysInterval>
      </ScheduleByDay>
    </CalendarTrigger>
  </Triggers>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        user = xml_escape(&user),
        time = time,
        command = xml_escape(&exe.to_string_lossy()),
        arguments = xml_escape(HEADLESS_REFRESH_ARG),
    )
}

fn scheduled_task_installed() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", AUTO_REFRESH_TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn auto_refresh_settings_view(app: &AppHandle) -> AutoRefreshSettingsView {
    let settings = load_app_settings(app);
    // Read before the struct literal moves `auto_refresh_roots` out of `settings`.
    let vision_minutes = vision_limit_minutes(&settings);
    AutoRefreshSettingsView {
        enabled: settings.auto_refresh_enabled,
        time: settings.auto_refresh_time.unwrap_or_else(|| DEFAULT_AUTO_REFRESH_TIME.to_string()),
        roots: settings.auto_refresh_roots,
        run_nsfw: settings.auto_refresh_nsfw.unwrap_or(true),
        run_text_analysis: settings.auto_refresh_text_analysis.unwrap_or(true),
        run_text_extraction: settings.auto_refresh_text_extraction.unwrap_or(false),
        run_vision: settings.auto_refresh_vision.unwrap_or(false),
        vision_minutes,
        gpu_wait: settings.auto_refresh_gpu_wait.unwrap_or(true),
        low_priority: settings.auto_refresh_low_priority.unwrap_or(true),
        toast: settings.auto_refresh_toast.unwrap_or(true),
        task_installed: scheduled_task_installed(),
        last_run_at: settings.last_auto_refresh_at,
        last_run_summary: settings.last_auto_refresh_summary,
    }
}

#[tauri::command]
fn get_auto_refresh_settings(app: AppHandle) -> AutoRefreshSettingsView {
    auto_refresh_settings_view(&app)
}

/// The nightly description pass's GPU budget, in minutes. `0` means "run the backlog down".
///
/// Unset reads as [`DEFAULT_VISION_LIMIT_MINUTES`] rather than as unlimited: an existing install
/// has no value here, and the whole reason this exists is that unlimited was the wrong default.
fn vision_limit_minutes(settings: &AppSettings) -> u32 {
    settings
        .auto_refresh_vision_minutes
        .unwrap_or(DEFAULT_VISION_LIMIT_MINUTES)
        .min(MAX_VISION_LIMIT_MINUTES)
}

/// What a nightly refresh running *in another process* is currently doing, or `None` when none is.
///
/// The GUI polls this — see `auto_run` for why a separate process cannot be observed any other way.
#[tauri::command]
fn get_auto_refresh_run(app: AppHandle) -> Option<auto_run::RunState> {
    auto_run::read_live(&app)
}

/// Asks a running nightly refresh to stop. Returns as soon as the request is recorded; the run
/// itself stops at its next per-image check, which the banner reflects as "Stopping…".
#[tauri::command]
fn cancel_auto_refresh_run(app: AppHandle) -> Result<(), String> {
    if auto_run::read_live(&app).is_none() {
        return Err("No automatic refresh is running.".to_string());
    }
    auto_run::request_cancel(&app)
}

#[tauri::command]
fn set_auto_refresh_settings(
    app: AppHandle,
    enabled: bool,
    time: String,
    roots: Vec<String>,
    run_nsfw: bool,
    run_text_analysis: bool,
    run_text_extraction: bool,
    run_vision: bool,
    vision_minutes: u32,
    gpu_wait: bool,
    low_priority: bool,
    toast: bool,
) -> Result<AutoRefreshSettingsView, String> {
    let time = validate_time_of_day(&time)?;
    if vision_minutes > MAX_VISION_LIMIT_MINUTES {
        return Err(format!(
            "The description time limit must be {MAX_VISION_LIMIT_MINUTES} minutes or less (0 = no limit)."
        ));
    }
    let mut settings = load_app_settings(&app);
    settings.auto_refresh_enabled = enabled;
    settings.auto_refresh_time = Some(time.clone());
    settings.auto_refresh_roots = roots;
    settings.auto_refresh_nsfw = Some(run_nsfw);
    settings.auto_refresh_text_analysis = Some(run_text_analysis);
    settings.auto_refresh_text_extraction = Some(run_text_extraction);
    settings.auto_refresh_vision = Some(run_vision);
    settings.auto_refresh_vision_minutes = Some(vision_minutes);
    settings.auto_refresh_gpu_wait = Some(gpu_wait);
    settings.auto_refresh_low_priority = Some(low_priority);
    settings.auto_refresh_toast = Some(toast);
    save_app_settings(&app, &settings)?;
    configure_scheduled_task(enabled, &time)?;
    Ok(auto_refresh_settings_view(&app))
}

// Lowers the whole process to below-normal OS scheduling priority so a nightly backlog yields
// CPU to anything running in the foreground (a game, encoding, etc.) instead of competing for it.
fn lower_process_priority() {
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }
}

// Caps the rayon global pool at half the logical cores (rather than the all-cores default) so the
// thumbnail pass can't fully saturate the machine during a background refresh.
fn capped_thread_count() -> usize {
    let available = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    (available / 2).max(1)
}

// Entry point for `--headless-refresh`, run from `setup()` on its own thread while the (windowless)
// event loop runs on the main thread to keep the tray icon's Cancel menu responsive. Sequentially
// reconciles + analyzes every opted-in root, honouring the same per-pass toggles as the GUI's
// "Analyze New" controls, then persists a summary and exits the process.
/// Sets every pass's cancel flag. The passes each poll their own control between items, so this is
/// all a stop consists of — whether it came from the tray menu or from the GUI's Stop button.
fn cancel_all_passes(app: &AppHandle) {
    app.state::<NsfwControl>().cancel.store(true, Ordering::SeqCst);
    app.state::<AnalysisControl>().cancel.store(true, Ordering::SeqCst);
    app.state::<OcrTextControl>().cancel.store(true, Ordering::SeqCst);
    app.state::<ChunkControl>().cancel.store(true, Ordering::SeqCst);
    app.state::<VisionControl>().cancel.store(true, Ordering::SeqCst);
}

/// Publishes the run's state once a second, and is the one place that acts on the two things that
/// can stop it from outside the passes themselves: a stop requested by the GUI, and the vision
/// pass's time limit.
///
/// Both live here rather than inside the pass loops for the same reason — the loops already check
/// `control.cancel` between items, so driving them through that flag needs no new checks in any hot
/// path and works identically for every pass. It also means the heartbeat keeps beating during a
/// long `scan_and_reconcile` (minutes on a 90k-image root) that reports no progress of its own,
/// which is what stops the GUI from calling a working run stale.
fn spawn_run_supervisor(app: &AppHandle, shared: &Arc<Mutex<auto_run::RunState>>) {
    let app = app.clone();
    let shared = Arc::clone(shared);
    std::thread::spawn(move || loop {
        {
            // Held across the whole body, including the publish: `run_headless_refresh` sets
            // `finished` under this same lock, so once it has, no further publish can start and the
            // file it then deletes stays deleted.
            let Ok(mut state) = shared.lock() else { return };
            if state.finished {
                return;
            }
            state.heartbeat_ms = auto_run::now_ms();

            if !state.cancel_requested && auto_run::cancel_requested(&app) {
                state.cancel_requested = true;
                state.label = "Stopping…".to_string();
                cancel_all_passes(&app);
            }

            // The GPU budget. `vision_deadline_ms` is only set once descriptions are actually
            // flowing, so the hours the GPU gate may spend waiting for a free card are not charged
            // against it.
            let deadline = state.vision_deadline_ms;
            if deadline > 0 && !state.hit_time_limit && auto_run::now_ms() >= deadline {
                state.hit_time_limit = true;
                state.label = "Stopping — reached the time limit".to_string();
                let control = app.state::<VisionControl>();
                control.hit_time_limit.store(true, Ordering::SeqCst);
                control.cancel.store(true, Ordering::SeqCst);
            }

            auto_run::publish(&app, &state);
        }
        std::thread::sleep(Duration::from_secs(1));
    });
}

/// Mirrors each pass's progress events into the shared run state.
///
/// The passes already emit exactly this, once per item, for the GUI's status line. In a headless
/// run there is no window to receive it — but `app.emit` also reaches Rust listeners in the same
/// process, so subscribing here picks up every pass's progress without touching a single pass.
fn install_progress_mirror(app: &AppHandle, shared: &Arc<Mutex<auto_run::RunState>>) {
    for event_name in [
        "nsfw-analysis-progress",
        "text-analysis-progress",
        "text-extraction-progress",
        "chunk-scan-progress",
        "vision-analysis-progress",
    ] {
        let shared = Arc::clone(shared);
        app.listen(event_name, move |event| {
            let Ok(progress) = serde_json::from_str::<TextAnalysisProgress>(event.payload()) else {
                return;
            };
            let Ok(mut state) = shared.lock() else { return };
            state.processed = progress.processed;
            state.total = progress.total;
            state.current_name = Some(progress.current_name);
        });
    }
}

/// Records which pass is running now, so the banner names it rather than showing a bare count.
fn set_run_phase(shared: &Arc<Mutex<auto_run::RunState>>, phase: &str, label: &str) {
    let Ok(mut state) = shared.lock() else { return };
    state.phase = phase.to_string();
    state.label = label.to_string();
    // Stale counts from the pass that just ended would otherwise show against the new pass's name
    // until its first progress event lands.
    state.processed = 0;
    state.total = 0;
    state.current_name = None;
}

fn set_run_root(shared: &Arc<Mutex<auto_run::RunState>>, root: &str, index: usize, total: usize) {
    let Ok(mut state) = shared.lock() else { return };
    state.root = Some(root.to_string());
    state.root_index = index;
    state.root_total = total;
}

fn run_headless_refresh(app: &AppHandle) {
    let settings = load_app_settings(app);

    if !settings.auto_refresh_enabled {
        eprintln!("Auto-refresh is disabled in settings; exiting.");
        app.exit(0);
        return;
    }

    let roots: Vec<String> = settings
        .auto_refresh_roots
        .iter()
        .filter(|root| Path::new(root).is_dir())
        .cloned()
        .collect();
    if roots.is_empty() {
        eprintln!("No auto-refresh folders configured; exiting.");
        app.exit(0);
        return;
    }

    if settings.auto_refresh_low_priority.unwrap_or(true) {
        lower_process_priority();
    }
    let _ = rayon::ThreadPoolBuilder::new().num_threads(capped_thread_count()).build_global();

    // A stop asked for while nothing was running — or one written just as the previous run exited —
    // must not cancel tonight's job before it starts.
    auto_run::clear_cancel(app);
    let shared = Arc::new(Mutex::new(auto_run::RunState {
        pid: std::process::id(),
        started_at: now_iso(),
        started_ms: auto_run::now_ms(),
        heartbeat_ms: auto_run::now_ms(),
        phase: "starting".to_string(),
        label: "Starting the nightly refresh".to_string(),
        root_total: roots.len(),
        vision_limit_minutes: vision_limit_minutes(&settings),
        ..auto_run::RunState::default()
    }));
    install_progress_mirror(app, &shared);
    spawn_run_supervisor(app, &shared);

    let show_toast = settings.auto_refresh_toast.unwrap_or(true);
    if show_toast {
        let _ = app
            .notification()
            .builder()
            .title("Image Categorizer")
            .body(format!(
                "Nightly refresh starting for {} folder{}. Right-click the tray icon to cancel.",
                roots.len(),
                if roots.len() == 1 { "" } else { "s" }
            ))
            .show();
    }

    let run_nsfw = settings.auto_refresh_nsfw.unwrap_or(true);
    let run_text_analysis_pass = settings.auto_refresh_text_analysis.unwrap_or(true);
    let run_text_extraction_pass = settings.auto_refresh_text_extraction.unwrap_or(false);
    let run_vision_pass = settings.auto_refresh_vision.unwrap_or(false);

    let total_roots = roots.len();
    let mut folders_done = 0usize;
    let mut cancelled = false;

    for (root_index, root) in roots.iter().enumerate() {
        let root_buf = PathBuf::from(root);
        set_run_root(&shared, root, root_index + 1, total_roots);
        set_run_phase(&shared, "scanning", "Scanning for new images");
        if scan_and_reconcile(&root_buf).is_err() {
            continue;
        }

        if run_nsfw && !cancelled {
            set_run_phase(&shared, "explicit", "Explicit content analysis");
            let control = app.state::<NsfwControl>();
            control.running.store(true, Ordering::SeqCst);
            control.cancel.store(false, Ordering::SeqCst);
            run_nsfw_analysis(app, &root_buf, false);
            if app.state::<NsfwControl>().cancel.load(Ordering::SeqCst) {
                cancelled = true;
            }
        }
        if run_text_analysis_pass && !cancelled {
            set_run_phase(&shared, "text", "Text analysis");
            let control = app.state::<AnalysisControl>();
            control.running.store(true, Ordering::SeqCst);
            control.cancel.store(false, Ordering::SeqCst);
            run_text_analysis(app, &root_buf, false);
            if app.state::<AnalysisControl>().cancel.load(Ordering::SeqCst) {
                cancelled = true;
            }
        }
        if run_text_extraction_pass && !cancelled {
            set_run_phase(&shared, "ocr", "Extracting OCR text");
            let control = app.state::<OcrTextControl>();
            control.running.store(true, Ordering::SeqCst);
            control.cancel.store(false, Ordering::SeqCst);
            // Unscoped, as the nightly job always has been: it is the "keep everything current"
            // pass, not the search index's own top-up.
            run_text_extraction(app, &root_buf, false, false);
            if app.state::<OcrTextControl>().cancel.load(Ordering::SeqCst) {
                cancelled = true;
            }
        }

        folders_done += 1;
        if cancelled {
            break;
        }
    }

    // The vision pass is deliberately a second phase over the same roots rather than another step
    // inside the loop above, for two reasons. The GPU gate must be consulted exactly once — after
    // the first batch of descriptions starts, *this* process is what is pinning the card, so a
    // re-check per root would see a busy GPU and stall a run that is working (see `gpu_gate`). And
    // descriptions depend on NSFW scores: `pending_vision` skips anything not yet scored, so
    // finishing the scoring for every root first is what stops the last root's images being passed
    // over as "not yet Explicit-analyzed".
    let vision_note = if run_vision_pass && !cancelled {
        match run_headless_vision(app, &settings, &roots, &shared) {
            HeadlessVisionOutcome::Ran => Some("Descriptions run.".to_string()),
            HeadlessVisionOutcome::Cancelled => {
                cancelled = true;
                Some("Descriptions cancelled.".to_string())
            }
            HeadlessVisionOutcome::HitTimeLimit => Some(format!(
                "Descriptions stopped at the {}-minute limit; the rest is left for the next run.",
                vision_limit_minutes(&settings)
            )),
            HeadlessVisionOutcome::SkippedGpuBusy => {
                Some("Descriptions skipped — the GPU stayed busy.".to_string())
            }
        }
    } else {
        None
    };

    let mut summary = if cancelled {
        format!(
            "Cancelled after {folders_done} of {total_roots} folder{}.",
            if total_roots == 1 { "" } else { "s" }
        )
    } else {
        format!("Completed {folders_done} folder{}.", if folders_done == 1 { "" } else { "s" })
    };
    if let Some(note) = vision_note {
        summary.push(' ');
        summary.push_str(&note);
    }

    let mut settings = load_app_settings(app);
    settings.last_auto_refresh_at = Some(now_iso());
    settings.last_auto_refresh_summary = Some(summary.clone());
    let _ = save_app_settings(app, &settings);

    // Stop the supervisor before removing the file, or its next tick would republish a run that has
    // ended and leave the GUI showing a banner for it until the staleness window expired. Setting
    // this under the lock is what makes the ordering hold — see `spawn_run_supervisor`.
    if let Ok(mut state) = shared.lock() {
        state.finished = true;
    }
    auto_run::clear(app);
    auto_run::clear_cancel(app);

    if show_toast {
        let _ = app
            .notification()
            .builder()
            .title("Image Categorizer")
            .body(format!("Nightly refresh: {summary}"))
            .show();
    }

    app.exit(0);
}

enum HeadlessVisionOutcome {
    Ran,
    Cancelled,
    /// The pass used up its GPU budget with work still pending. Distinct from `Cancelled` because
    /// nobody asked for it and nothing is wrong — it is the normal end of a nightly slice.
    HitTimeLimit,
    /// The GPU never freed up inside the gate's budget, so the descriptions were left for the next
    /// run rather than forced onto a card somebody else is using.
    SkippedGpuBusy,
}

/// Whether the vision model still has to be loaded onto the card.
///
/// This is the input that keeps the GPU gate from deadlocking against itself: a resident 26B model
/// leaves far less free VRAM than loading one needs, so requiring load-sized headroom when nothing
/// needs loading would block forever (see `gpu_gate`).
///
/// `Unknown` counts as "needs loading" — the conservative direction. Being wrong that way costs a
/// wait; being wrong the other way starts a load that does not fit and runs ~10x slower off system
/// RAM. An endpoint that is not LM Studio always answers `Unknown` (only its native `/api/v0/models`
/// reports residency), which is the same situation as a server that is simply down: the pass would
/// fail anyway.
fn vision_model_needs_loading(settings: &AppSettings) -> bool {
    let Some(models_url) = vision_rest_models_url(settings) else {
        return true;
    };
    let api_key = vision_api_key(settings);
    vision::model_state(
        &vision::build_probe_agent(),
        &models_url,
        &vision_model(settings),
        api_key.as_deref(),
    ) != vision::ModelState::Loaded
}

/// Describes every opted-in root with the local vision model, once the GPU is free to do it.
///
/// Called only from the nightly job. The interactive Describe button has no gate on purpose: a user
/// pressing it is asking for the GPU deliberately, and making them wait behind their own game would
/// be absurd. It is the unattended run that has to be a good neighbour.
fn run_headless_vision(
    app: &AppHandle,
    settings: &AppSettings,
    roots: &[String],
    shared: &Arc<Mutex<auto_run::RunState>>,
) -> HeadlessVisionOutcome {
    let cancelled = || app.state::<VisionControl>().cancel.load(Ordering::SeqCst);
    app.state::<VisionControl>().hit_time_limit.store(false, Ordering::SeqCst);

    if settings.auto_refresh_gpu_wait.unwrap_or(true) {
        set_run_phase(shared, "gpu-wait", "Waiting for the GPU to be free");
        let needs_vram = vision_model_needs_loading(settings);
        let show_toast = settings.auto_refresh_toast.unwrap_or(true);
        let announce_wait = |sample: gpu_gate::GpuSample| {
            if show_toast {
                let _ = app
                    .notification()
                    .builder()
                    .title("Image Categorizer")
                    .body(format!(
                        "GPU busy ({}% in use). Waiting for it to free up before describing images.",
                        sample.utilization_pct
                    ))
                    .show();
            }
        };
        match gpu_gate::wait_until_free(needs_vram, &cancelled, &announce_wait) {
            gpu_gate::GateOutcome::Proceed => {}
            gpu_gate::GateOutcome::Cancelled => return HeadlessVisionOutcome::Cancelled,
            gpu_gate::GateOutcome::TimedOut => return HeadlessVisionOutcome::SkippedGpuBusy,
        }
    }

    // Arm the budget only now. Everything above this line is the gate, which has its own (much
    // longer) ceiling and does not touch the card — charging its wait against the GPU budget could
    // let a run that finally got a free card immediately stop again with nothing described.
    let limit_minutes = vision_limit_minutes(settings);
    if limit_minutes > 0 {
        if let Ok(mut state) = shared.lock() {
            state.vision_deadline_ms = auto_run::now_ms() + u64::from(limit_minutes) * 60_000;
            state.vision_limit_minutes = limit_minutes;
        }
    }
    set_run_phase(shared, "vision", "Describing images");

    // The deadline is enforced by the supervisor, which stops the pass through `VisionControl`, so
    // check it first: it sets `cancel` too, and reading that alone would report a cancellation
    // nobody asked for.
    let hit_limit = || app.state::<VisionControl>().hit_time_limit.load(Ordering::SeqCst);

    for root in roots {
        if hit_limit() {
            return HeadlessVisionOutcome::HitTimeLimit;
        }
        if cancelled() {
            return HeadlessVisionOutcome::Cancelled;
        }
        let control = app.state::<VisionControl>();
        control.running.store(true, Ordering::SeqCst);
        run_vision_analysis(app, &PathBuf::from(root), false);
        if hit_limit() {
            return HeadlessVisionOutcome::HitTimeLimit;
        }
        if cancelled() {
            return HeadlessVisionOutcome::Cancelled;
        }
    }
    HeadlessVisionOutcome::Ran
}

// ============================================================================
// Video chunking (Stage A): OCR the title bar, group frames by video, sample N
// ============================================================================

fn chunk_plan_path(root: &Path) -> PathBuf {
    root.join(CHUNK_PLAN_FILE_NAME)
}

fn load_chunk_plan(root: &Path) -> Option<ChunkPlan> {
    fs::read_to_string(chunk_plan_path(root))
        .ok()
        .and_then(|data| serde_json::from_str::<ChunkPlan>(&data).ok())
}

fn save_chunk_plan(root: &Path, plan: &ChunkPlan) -> Result<(), String> {
    let data = serde_json::to_string_pretty(plan)
        .map_err(|error| format!("Failed to serialize chunk plan: {error}"))?;
    fs::write(chunk_plan_path(root), data).map_err(|error| format!("Failed to save chunk plan: {error}"))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChunkPlanSummary {
    exists: bool,
    path: String,
    groups: usize,
    total_frames: usize,
    selected_frames: usize,
    samples_per_group: u32,
    generated_at: Option<String>,
}

fn chunk_plan_summary(root: &Path) -> ChunkPlanSummary {
    let path = chunk_plan_path(root).to_string_lossy().to_string();
    match load_chunk_plan(root) {
        Some(plan) => ChunkPlanSummary {
            exists: true,
            path,
            groups: plan.groups.len(),
            total_frames: plan.groups.iter().map(|g| g.member_hashes.len()).sum(),
            selected_frames: plan.groups.iter().map(|g| g.selected_hashes.len()).sum(),
            samples_per_group: plan.samples_per_group,
            generated_at: Some(plan.generated_at),
        },
        None => ChunkPlanSummary {
            exists: false,
            path,
            groups: 0,
            total_frames: 0,
            selected_frames: 0,
            samples_per_group: DEFAULT_SAMPLES_PER_GROUP,
            generated_at: None,
        },
    }
}

// (Re)builds and saves the plan from every record confirmed as a video frame. `force` re-samples
// every group; otherwise frozen selections carry over so a rescan that only adds frames never
// reshuffles a set you already reviewed. With no video frames at all, any stale plan is removed so
// the vision pass falls back to describing everything.
fn rebuild_and_save_plan(root: &Path, force: bool) -> Result<ChunkPlanSummary, String> {
    let config = load_library_config(root);
    let titled: Vec<(String, String)> = config
        .images
        .iter()
        .filter_map(|(hash, record)| {
            record
                .video_title
                .as_ref()
                .filter(|title| !title.is_empty())
                .map(|title| (hash.clone(), title.clone()))
        })
        .collect();

    if titled.is_empty() {
        let _ = fs::remove_file(chunk_plan_path(root));
        return Ok(chunk_plan_summary(root));
    }

    let previous = load_chunk_plan(root);
    let plan = build_plan(&titled, DEFAULT_SAMPLES_PER_GROUP, now_iso(), previous.as_ref(), force);
    save_chunk_plan(root, &plan)?;
    Ok(chunk_plan_summary(root))
}

#[tauri::command]
fn get_chunk_plan(root: String) -> Result<ChunkPlanSummary, String> {
    let root_buf = root_path(&root)?;
    Ok(chunk_plan_summary(&root_buf))
}

#[tauri::command]
fn regenerate_chunk_plan(root: String) -> Result<ChunkPlanSummary, String> {
    let root_buf = root_path(&root)?;
    rebuild_and_save_plan(&root_buf, true)
}

#[tauri::command]
fn discard_chunk_plan(root: String) -> Result<ChunkPlanSummary, String> {
    let root_buf = root_path(&root)?;
    let path = chunk_plan_path(&root_buf);
    if path.exists() {
        fs::remove_file(&path).map_err(|error| format!("Failed to delete chunk plan: {error}"))?;
    }
    Ok(chunk_plan_summary(&root_buf))
}

#[tauri::command]
fn build_chunk_plan(
    app: AppHandle,
    control: tauri::State<'_, ChunkControl>,
    root: String,
    force: bool,
) -> Result<(), String> {
    if control.running.swap(true, Ordering::SeqCst) {
        return Err("Video chunk scan is already running.".to_string());
    }
    let root_buf = match root_path(&root) {
        Ok(path) => path,
        Err(error) => {
            control.running.store(false, Ordering::SeqCst);
            return Err(error);
        }
    };
    control.cancel.store(false, Ordering::SeqCst);
    let app_handle = app.clone();
    std::thread::spawn(move || run_chunk_scan(&app_handle, &root_buf, force));
    Ok(())
}

#[tauri::command]
fn cancel_chunk_scan(control: tauri::State<'_, ChunkControl>) -> Result<(), String> {
    if !control.running.load(Ordering::SeqCst) {
        return Err("No video chunk scan is running.".to_string());
    }
    control.cancel.store(true, Ordering::SeqCst);
    Ok(())
}

// OCRs the title strip of every not-yet-scanned image (resumable via `video_title`), then rebuilds
// the chunk plan (preserving frozen selections unless `force`). Mirrors the other passes' skeleton.
fn run_chunk_scan(app: &AppHandle, root_buf: &Path, force: bool) {
    let control = app.state::<ChunkControl>();

    let result = (|| -> Result<(&'static str, Option<String>), String> {
        let view = scan_and_reconcile(root_buf)?;
        let config = load_library_config(root_buf);

        let pending: Vec<(String, String, String)> = pending_chunk(&view, &config, force)
            .into_iter()
            .map(|image| (image.hash.clone(), image.path.clone(), image.name.clone()))
            .collect();
        drop(config);

        let total = pending.len();
        let mut cancelled = false;
        let mut results: Vec<(String, String)> = Vec::new();

        for (index, (hash, path, name)) in pending.iter().enumerate() {
            if control.cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            match ocr::extract_title_strip(Path::new(path), TITLE_STRIP_TOP_FRACTION) {
                // `Some(title)` for a video; `""` means "scanned, no video marker found".
                Ok(strip) => results.push((hash.clone(), clean_title(&strip).unwrap_or_default())),
                Err(error) => eprintln!("Title-strip OCR failed for {path}: {error}"),
            }

            if results.len() >= ANALYSIS_CHECKPOINT_EVERY {
                commit_chunk_results(root_buf, &mut results)?;
            }

            let _ = app.emit(
                "chunk-scan-progress",
                TextAnalysisProgress { processed: index + 1, total, current_name: name.clone() },
            );
        }

        commit_chunk_results(root_buf, &mut results)?;

        let summary = rebuild_and_save_plan(root_buf, force)?;
        let message = Some(format!(
            "{} video{} grouped from {} frame{}; {} selected for description.",
            summary.groups,
            if summary.groups == 1 { "" } else { "s" },
            summary.total_frames,
            if summary.total_frames == 1 { "" } else { "s" },
            summary.selected_frames,
        ));
        Ok((if cancelled { "cancelled" } else { "completed" }, message))
    })();

    control.running.store(false, Ordering::SeqCst);
    let (status, message) = match result {
        Ok((status, message)) => (status.to_string(), message),
        Err(error) => ("error".to_string(), Some(error)),
    };
    let _ = app.emit("chunk-scan-finished", TextAnalysisFinished { status, message });
}

// ============================================================================
// Vision descriptions (Stage B): images -> words via a local vision model
// ============================================================================

fn vision_endpoint(settings: &AppSettings) -> String {
    settings
        .vision_endpoint
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_VISION_ENDPOINT.to_string())
}

fn vision_model(settings: &AppSettings) -> String {
    settings
        .vision_model
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_VISION_MODEL.to_string())
}

/// The bearer token sent to the vision endpoint, or `None` when unset. LM Studio rejects every
/// request when its "Require API token" auth is on and no token is supplied; most other local
/// servers ignore the header, so it is optional.
fn vision_api_key(settings: &AppSettings) -> Option<String> {
    settings
        .vision_api_key
        .clone()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Whether the idle lease is armed. On by default: an 18 GB model left resident after a run is a
/// cost every other app on the box pays, and the lease only ever expires a load this app caused.
fn vision_idle_unload(settings: &AppSettings) -> bool {
    settings.vision_idle_unload.unwrap_or(true)
}

fn vision_idle_minutes(settings: &AppSettings) -> u32 {
    settings
        .vision_idle_minutes
        .unwrap_or(model_lease::DEFAULT_IDLE_MINUTES)
        .clamp(model_lease::MIN_IDLE_MINUTES, model_lease::MAX_IDLE_MINUTES)
}

/// The configured idle window, or `None` when the lease is switched off.
fn vision_idle_window(settings: &AppSettings) -> Option<Duration> {
    vision_idle_unload(settings).then(|| Duration::from_secs(u64::from(vision_idle_minutes(settings)) * 60))
}

/// Pushes the current setting into `vision`, which stamps it onto every outgoing request. Call
/// after anything that can change the window — startup and each settings save.
fn apply_idle_ttl(settings: &AppSettings) {
    let seconds = vision_idle_window(settings).map_or(0, |window| window.as_secs() as u32);
    vision::set_idle_ttl_secs(seconds);
}

/// The scheme+authority of the configured endpoint (`http://localhost:1234`), or `None` if it isn't
/// shaped like a URL.
fn endpoint_origin(endpoint: &str) -> Option<String> {
    let (scheme, rest) = endpoint.trim().split_once("://")?;
    let authority = rest.split('/').next().filter(|value| !value.is_empty())?;
    Some(format!("{scheme}://{authority}"))
}

/// LM Studio's **native** `…/api/v0/models`, which is the only endpoint that reports whether a model
/// is actually resident (`/v1/models` lists every downloaded one). Derived from the configured chat
/// endpoint's origin rather than a second setting, and `None` when that isn't a URL — a server that
/// doesn't answer it simply leaves the lease unclaimed.
fn vision_rest_models_url(settings: &AppSettings) -> Option<String> {
    endpoint_origin(&vision_endpoint(settings)).map(|origin| format!("{origin}/api/v0/models"))
}

/// Derives the `…/v1/models` URL from the configured chat-completions endpoint, so the model picker
/// hits the same server without needing a second setting. Strips a trailing `/chat/completions`
/// (the usual shape) — else falls back to appending `/models` beside whatever path is configured.
fn vision_models_url(settings: &AppSettings) -> String {
    let endpoint = vision_endpoint(settings);
    let trimmed = endpoint.trim_end_matches('/');
    let base = trimmed
        .strip_suffix("/chat/completions")
        .or_else(|| trimmed.strip_suffix("/completions"))
        .unwrap_or(trimmed);
    format!("{base}/models")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VisionSettingsView {
    endpoint: String,
    model: String,
    api_key: String,
    idle_unload: bool,
    idle_minutes: u32,
}

fn vision_settings_view(settings: &AppSettings) -> VisionSettingsView {
    VisionSettingsView {
        endpoint: vision_endpoint(settings),
        model: vision_model(settings),
        api_key: vision_api_key(settings).unwrap_or_default(),
        idle_unload: vision_idle_unload(settings),
        idle_minutes: vision_idle_minutes(settings),
    }
}

#[tauri::command]
fn get_vision_settings(app: AppHandle) -> VisionSettingsView {
    vision_settings_view(&load_app_settings(&app))
}

#[tauri::command]
fn set_vision_settings(
    app: AppHandle,
    endpoint: String,
    model: String,
    api_key: String,
    idle_unload: bool,
    idle_minutes: u32,
) -> Result<VisionSettingsView, String> {
    let mut settings = load_app_settings(&app);
    settings.vision_endpoint = Some(endpoint.trim().to_string()).filter(|value| !value.is_empty());
    settings.vision_model = Some(model.trim().to_string()).filter(|value| !value.is_empty());
    settings.vision_api_key = Some(api_key.trim().to_string()).filter(|value| !value.is_empty());
    settings.vision_idle_unload = Some(idle_unload);
    settings.vision_idle_minutes = Some(idle_minutes.clamp(model_lease::MIN_IDLE_MINUTES, model_lease::MAX_IDLE_MINUTES));
    save_app_settings(&app, &settings)?;
    apply_idle_ttl(&settings);
    Ok(vision_settings_view(&settings))
}

/// Reports the idle lease for the Settings panel: whether this app is holding a load, and how long
/// the window and the endpoint have been quiet.
#[tauri::command]
fn get_vision_idle_status(app: AppHandle) -> IdleLeaseStatus {
    let settings = load_app_settings(&app);
    app.state::<ModelLease>()
        .status(vision_idle_unload(&settings), vision_idle_minutes(&settings))
}

/// Frontend heartbeat: the user just did something in the window. Throttled hard on the JS side —
/// this exists to hold the model open for someone who is working in the app between passes, so it
/// only has to be accurate to the minute.
#[tauri::command]
fn note_app_activity(app: AppHandle) {
    app.state::<ModelLease>().note_app_activity();
}

/// Lists the model ids the configured vision endpoint offers, for the Settings model picker.
#[tauri::command]
fn list_vision_models(app: AppHandle) -> Result<Vec<String>, String> {
    let settings = load_app_settings(&app);
    let models_url = vision_models_url(&settings);
    let api_key = vision_api_key(&settings);
    let agent = build_agent();
    list_models(&agent, &models_url, api_key.as_deref())
}

/// Actively loads `model` into the endpoint now (LM Studio JIT-loads it on a tiny poke), so the user
/// can confirm the model comes up before running Describe instead of discovering mid-run that
/// nothing was loaded. Blocks until the model responds — a cold load legitimately takes a while.
#[tauri::command]
fn load_vision_model(app: AppHandle, model: String) -> Result<String, String> {
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err("Pick or type a model first.".to_string());
    }
    let settings = load_app_settings(&app);
    let endpoint = vision_endpoint(&settings);
    let api_key = vision_api_key(&settings);
    let agent = build_agent();
    // Wrapped so that if this poke is what brings the model up, the app owns that load and may let
    // it expire; if the model was already resident it belongs to whoever loaded it.
    model_lease::with_claim(&app, &model, || {
        warm_model(&agent, &endpoint, &model, api_key.as_deref())
    })?;
    Ok(format!("Model \"{model}\" is loaded and ready."))
}

// Writes one image's description sidecar (`<hash>.json` rich + `<hash>.txt` prose) and returns the
// prose character count.
fn write_vision_description(
    desc_dir: &Path,
    hash: &str,
    relative_path: &str,
    name: &str,
    video_title: Option<&str>,
    description: &str,
    model: &str,
) -> Result<u32, String> {
    let record = serde_json::json!({
        "schemaVersion": VISION_DESC_SCHEMA_VERSION,
        "hash": hash,
        "relativePath": relative_path,
        "name": name,
        "videoTitle": video_title,
        "description": description,
        "model": model,
        "promptVersion": VISION_PROMPT_VERSION,
        "analyzedAt": now_iso(),
    });
    let json = serde_json::to_string_pretty(&record)
        .map_err(|error| format!("Failed to serialize description: {error}"))?;
    fs::write(desc_dir.join(format!("{hash}.json")), json)
        .map_err(|error| format!("Failed to save description: {error}"))?;
    fs::write(desc_dir.join(format!("{hash}.txt")), description)
        .map_err(|error| format!("Failed to save description text: {error}"))?;
    Ok(description.chars().count() as u32)
}

// Rebuilds `index.json` (relative path -> hash) from every described record, so a consumer holding
// an image file can resolve it to its `<hash>.json`. Derived from the sidecar, never bookkept
// incrementally, so it can't drift out of sync.
fn write_vision_index(root: &Path, desc_dir: &Path) -> Result<(), String> {
    let config = load_library_config(root);
    let mut by_path = serde_json::Map::new();
    for (hash, record) in &config.images {
        if record.vision_desc_chars.is_some() && !record.last_known_path.is_empty() {
            by_path.insert(record.last_known_path.clone(), serde_json::Value::String(hash.clone()));
        }
    }
    let index = serde_json::json!({
        "version": 1,
        "generatedAt": now_iso(),
        "descriptionDir": VISION_DESC_DIR_NAME,
        "byPath": by_path,
    });
    let json = serde_json::to_string_pretty(&index)
        .map_err(|error| format!("Failed to serialize description index: {error}"))?;
    fs::write(desc_dir.join(VISION_INDEX_FILE_NAME), json)
        .map_err(|error| format!("Failed to save description index: {error}"))
}

#[tauri::command]
fn analyze_vision(
    app: AppHandle,
    control: tauri::State<'_, VisionControl>,
    root: String,
    force: bool,
) -> Result<(), String> {
    if control.running.swap(true, Ordering::SeqCst) {
        return Err("Vision description is already running.".to_string());
    }
    let root_buf = match root_path(&root) {
        Ok(path) => path,
        Err(error) => {
            control.running.store(false, Ordering::SeqCst);
            return Err(error);
        }
    };
    control.cancel.store(false, Ordering::SeqCst);
    let app_handle = app.clone();
    std::thread::spawn(move || run_vision_analysis(&app_handle, &root_buf, force));
    Ok(())
}

#[tauri::command]
fn cancel_vision_analysis(control: tauri::State<'_, VisionControl>) -> Result<(), String> {
    if !control.running.load(Ordering::SeqCst) {
        return Err("No vision description is running.".to_string());
    }
    control.cancel.store(true, Ordering::SeqCst);
    Ok(())
}

// Describes eligible images with the local vision model, one at a time, committing after each so a
// stop/crash resumes cleanly (a half-finished item leaves no sidecar and no marker, so it's just
// redone). Eligible = not in an excluded folder, not explicit (per NSFW score), and — when a chunk
// plan exists — every non-video image plus only the sampled frames of each video. Explicit or
// not-yet-NSFW-scored images are skipped and counted so the summary explains what was left out.
fn run_vision_analysis(app: &AppHandle, root_buf: &Path, force: bool) {
    let control = app.state::<VisionControl>();

    let result = (|| -> Result<(&'static str, Option<String>), String> {
        let settings = load_app_settings(app);
        let endpoint = vision_endpoint(&settings);
        let model = vision_model(&settings);
        let api_key = vision_api_key(&settings);

        let view = scan_and_reconcile(root_buf)?;
        let config = load_library_config(root_buf);
        let plan = load_chunk_plan(root_buf);

        let desc_dir = root_buf.join(VISION_DESC_DIR_NAME);
        fs::create_dir_all(&desc_dir).map_err(|error| format!("Failed to create descriptions folder: {error}"))?;

        let (eligible, skips) = pending_vision(&view, &config, plan.as_ref(), force);
        let VisionSkips {
            video: skipped_video,
            explicit: skipped_explicit,
            unscored: skipped_unscored,
            category: skipped_category,
        } = skips;

        let pending: Vec<(String, String, String, String, Option<String>)> = eligible
            .into_iter()
            .map(|image| {
                let title = config
                    .images
                    .get(&image.hash)
                    .and_then(|r| r.video_title.clone())
                    .filter(|t| !t.is_empty());
                (
                    image.hash.clone(),
                    image.path.clone(),
                    image.name.clone(),
                    image.relative_path.clone(),
                    title,
                )
            })
            .collect();
        drop(config);

        let total = pending.len();
        if total == 0 {
            let _ = write_vision_index(root_buf, &desc_dir);
            let mut notes = vec![];
            if skipped_unscored > 0 {
                notes.push(format!("{skipped_unscored} not yet Explicit-analyzed (run Explicit first)"));
            }
            if skipped_explicit > 0 {
                notes.push(format!("{skipped_explicit} explicit"));
            }
            if skipped_video > 0 {
                notes.push(format!("{skipped_video} deduped video frames"));
            }
            if skipped_category > 0 {
                notes.push(format!("{skipped_category} in omitted categories"));
            }
            let message = if notes.is_empty() {
                "No images needed description.".to_string()
            } else {
                format!("Nothing to describe. Skipped: {}.", notes.join(", "))
            };
            return Ok(("completed", Some(message)));
        }

        let agent = build_agent();

        // Actively load the chosen model before the first image. LM Studio JIT-loads a cold model on
        // this poke; if it can't (model not downloaded, wrong token, server down) every image would
        // fail identically, so surface that now with the server's own message instead of grinding.
        let warmed = model_lease::with_claim(app, &model, || {
            warm_model(&agent, &endpoint, &model, api_key.as_deref())
        });
        if let Err(error) = warmed {
            return Err(format!(
                "Describe couldn't load the model \"{model}\". Pick a model that is downloaded in \
                 LM Studio (Settings → Describe → Load model), and check the endpoint and API token. \
                 Error: {error}"
            ));
        }

        let mut cancelled = false;
        let mut failures = 0usize;
        let mut described_ok = 0usize;
        let mut endpoint_failures = 0usize;
        let mut last_endpoint_error: Option<String> = None;
        let mut results: Vec<(String, u32)> = Vec::new();

        for (index, (hash, path, name, relative_path, title)) in pending.iter().enumerate() {
            if control.cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            match describe_image(&agent, &endpoint, &model, api_key.as_deref(), DESCRIBE_PROMPT, Path::new(path)) {
                Ok(description) => {
                    match write_vision_description(&desc_dir, hash, relative_path, name, title.as_deref(), &description, &model) {
                        Ok(chars) => {
                            described_ok += 1;
                            results.push((hash.clone(), chars));
                        }
                        Err(error) => {
                            failures += 1;
                            eprintln!("Failed to save description for {path}: {error}");
                        }
                    }
                }
                Err(error) => {
                    failures += 1;
                    endpoint_failures += 1;
                    eprintln!("Vision description failed for {path}: {error}");
                    last_endpoint_error = Some(error);
                }
            }
            // Each request re-armed the endpoint's idle countdown, so the keep-alive has nothing to
            // do while a pass is grinding.
            app.state::<ModelLease>().note_request();

            // Commit each result promptly so a stop resumes with at most the in-flight image redone.
            commit_vision_results(root_buf, &mut results)?;

            // Fail fast: if the first few requests all bounce off the endpoint with nothing
            // described, the model is unloaded, the server is down, or the API token is wrong —
            // grinding through (and re-encoding) every remaining image would just fail identically.
            // Abort loudly with the server's own error so the user can fix it, instead of the old
            // behaviour of silent slow progress the run never recovers from.
            if described_ok == 0 && endpoint_failures >= VISION_FAIL_FAST_ATTEMPTS {
                let reason = last_endpoint_error.unwrap_or_else(|| "unknown error".to_string());
                return Err(format!(
                    "Describe aborted: the vision endpoint failed on the first {endpoint_failures} \
                     image(s) with nothing described. Check that a model is loaded in LM Studio and \
                     that the endpoint and API token in Settings are correct. Last error: {reason}"
                ));
            }

            let _ = app.emit(
                "vision-analysis-progress",
                TextAnalysisProgress { processed: index + 1, total, current_name: name.clone() },
            );
        }

        commit_vision_results(root_buf, &mut results)?;
        write_vision_index(root_buf, &desc_dir)?;

        let described = total - failures;
        let mut message = format!("Described {described} image{}.", if described == 1 { "" } else { "s" });
        if failures > 0 {
            message.push_str(&format!(" {failures} failed (see logs; endpoint {endpoint})."));
        }
        if skipped_category > 0 {
            message.push_str(&format!(" {skipped_category} omitted by category."));
        }
        Ok((if cancelled { "cancelled" } else { "completed" }, Some(message)))
    })();

    control.running.store(false, Ordering::SeqCst);
    let (status, message) = match result {
        Ok((status, message)) => (status.to_string(), message),
        Err(error) => ("error".to_string(), Some(error)),
    };
    let _ = app.emit("vision-analysis-finished", TextAnalysisFinished { status, message });
}

// =================================================================================================
// Geo layer
//
// A parallel axis over the vision descriptions: which country each image shows, and country sets
// built from that. It writes only its own three sidecars (see `geo.rs`) and never touches an
// image's `category`, so Low Text / High Text membership — the filter super-image-viewer browses
// on — is unaffected by anything here.
//
// The derive is pure over data already on disk (descriptions × gazetteer × chunk plan), needs no
// model, and takes a couple of seconds on a 10k-description library, so it is cheap to re-run after
// every capture session.
// =================================================================================================

/// Reads every saved vision description. The `.txt` sidecar holds the same prose as the `.json` and
/// needs no parsing, so this walks those and takes the hash from the file stem.
fn load_descriptions(root: &Path) -> Vec<geo::DescribedImage> {
    let desc_dir = root.join(VISION_DESC_DIR_NAME);
    let Ok(entries) = fs::read_dir(&desc_dir) else {
        return Vec::new();
    };
    let paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("txt"))
        .collect();
    paths
        .par_iter()
        .filter_map(|path| {
            let hash = path.file_stem()?.to_str()?.to_string();
            let description = fs::read_to_string(path).ok()?;
            Some(geo::DescribedImage { hash, description })
        })
        .collect()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeoSummary {
    exists: bool,
    generated_at: Option<String>,
    stats: geo::GeoStats,
    sets: usize,
    geo_path: String,
    gazetteer_path: String,
    sets_path: String,
}

fn geo_summary(root: &Path, geo_file: Option<&geo::GeoFile>) -> GeoSummary {
    GeoSummary {
        exists: geo_file.is_some(),
        generated_at: geo_file.map(|file| file.generated_at.clone()),
        stats: geo_file.map(|file| file.stats.clone()).unwrap_or_default(),
        sets: geo::load_sets(root).map(|file| file.sets.len()).unwrap_or(0),
        geo_path: geo::geo_path(root).to_string_lossy().to_string(),
        gazetteer_path: geo::gazetteer_path(root).to_string_lossy().to_string(),
        sets_path: geo::sets_path(root).to_string_lossy().to_string(),
    }
}

/// Rebuilds the geo records from scratch. Also rewrites the gazetteer's `unresolved` worklist —
/// the hand-written `overrides` in that file are read, used, and preserved untouched.
#[tauri::command]
fn derive_geo(root: String) -> Result<GeoSummary, String> {
    let root_buf = root_path(&root)?;
    let descriptions = load_descriptions(&root_buf);
    if descriptions.is_empty() {
        return Err(
            "No vision descriptions found. Run Describe first — geo is derived from those.".to_string(),
        );
    }

    let plan = load_chunk_plan(&root_buf);
    let groups: Vec<geo::SourceGroup<'_>> = plan
        .as_ref()
        .map(|plan| {
            plan.groups
                .iter()
                .map(|group| geo::SourceGroup {
                    title: &group.title,
                    member_hashes: &group.member_hashes,
                })
                .collect()
        })
        .unwrap_or_default();

    let mut gazetteer = geo::load_gazetteer(&root_buf);
    let previous = geo::load_geo(&root_buf);
    let derived = geo::derive(
        &descriptions,
        &groups,
        &mut gazetteer,
        previous.as_ref(),
        now_iso(),
    );

    geo::save_gazetteer(&root_buf, &gazetteer)?;
    let json = serde_json::to_string_pretty(&derived)
        .map_err(|error| format!("Failed to serialize geo records: {error}"))?;
    fs::write(geo::geo_path(&root_buf), json)
        .map_err(|error| format!("Failed to save geo records: {error}"))?;

    Ok(geo_summary(&root_buf, Some(&derived)))
}

// ---- Scene kinds -------------------------------------------------------------------------------
// A text-only pass over the descriptions that answers "does this picture show a place at all", so
// country sets can drop the mall interiors, talking heads and browser screenshots that carry a real
// country but teach no geography. See `kinds.rs` for why it reads prose instead of pixels.

#[tauri::command]
fn classify_kinds(
    app: AppHandle,
    control: tauri::State<'_, KindControl>,
    root: String,
    force: bool,
) -> Result<(), String> {
    if control.running.swap(true, Ordering::SeqCst) {
        return Err("Scene classification is already running.".to_string());
    }
    let root_buf = match root_path(&root) {
        Ok(path) => path,
        Err(error) => {
            control.running.store(false, Ordering::SeqCst);
            return Err(error);
        }
    };
    control.cancel.store(false, Ordering::SeqCst);
    let handle = app.clone();
    std::thread::spawn(move || run_kind_classification(&handle, &root_buf, force));
    Ok(())
}

#[tauri::command]
fn cancel_kind_classification(control: tauri::State<'_, KindControl>) -> Result<(), String> {
    if !control.running.load(Ordering::SeqCst) {
        return Err("No scene classification is running.".to_string());
    }
    control.cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// Classifies every geo-tagged image that has no kind yet (or all of them, with `force`).
///
/// Scoped to geo-tagged images on purpose: they are exactly the population country sets draw from,
/// which is a little over a third of the described library. Results are checkpointed after every
/// batch, so a cancel or a crash costs one batch rather than the whole run.
fn run_kind_classification(app: &AppHandle, root: &Path, force: bool) {
    let control = app.state::<KindControl>();

    let result = (|| -> Result<(&'static str, Option<String>), String> {
        let settings = load_app_settings(app);
        let endpoint = vision_endpoint(&settings);
        let model = vision_model(&settings);
        let api_key = vision_api_key(&settings);

        let geo_file = geo::load_geo(root)
            .ok_or_else(|| "No geo records yet. Run Derive Geo first.".to_string())?;
        let mut kinds_file = kinds::load_kinds(root);
        if force || kinds_file.prompt_version != kinds::KIND_PROMPT_VERSION {
            // A changed prompt invalidates old labels — keeping them would mix two rubrics.
            kinds_file.kinds.clear();
        }

        // Only images that have a description of their OWN are classifiable. The rest of the
        // geo-tagged population was propagated from a video's sampled frames and never described —
        // they get their kind the same way, by inheriting it (see `propagate_kinds`). Iterating all
        // of them would spend the whole pass skipping two images in five.
        let desc_dir = root.join(VISION_DESC_DIR_NAME);
        let pending: Vec<String> = geo_file
            .images
            .keys()
            .filter(|hash| !kinds_file.kinds.contains_key(*hash))
            .filter(|hash| desc_dir.join(format!("{hash}.txt")).exists())
            .cloned()
            .collect();

        if pending.is_empty() {
            let filled = propagate_and_save_kinds(root, &mut kinds_file, &model)?;
            return Ok((
                "completed",
                Some(format!(
                    "Every described image is already classified; {filled} more inherited from their videos."
                )),
            ));
        }

        let agent = build_agent();
        let total = pending.len();
        let mut processed = 0usize;
        let mut failures = 0usize;
        let mut unparsed = 0usize;

        for chunk in pending.chunks(kinds::DEFAULT_BATCH_SIZE) {
            if control.cancel.load(Ordering::SeqCst) {
                kinds_file.prompt_version = kinds::KIND_PROMPT_VERSION;
                kinds_file.version = kinds::KIND_SCHEMA_VERSION;
                kinds_file.generated_at = now_iso();
                kinds_file.model = model.clone();
                kinds_file.note = kinds::KINDS_NOTE.to_string();
                kinds::save_kinds(root, &kinds_file)?;
                return Ok((
                    "cancelled",
                    Some(format!("Stopped after {processed} of {total}.")),
                ));
            }

            let mut scenes = Vec::with_capacity(chunk.len());
            let mut hashes = Vec::with_capacity(chunk.len());
            for hash in chunk {
                let Ok(text) = fs::read_to_string(desc_dir.join(format!("{hash}.txt"))) else {
                    continue;
                };
                scenes.push(kinds::scene_text(&text, KIND_SCENE_MAX_CHARS));
                hashes.push(hash.clone());
            }
            if scenes.is_empty() {
                processed += chunk.len();
                continue;
            }

            // Classify never warms first, so its own first batch can be the request that JIT-loads
            // the model — claim the lease here for the same reason Describe claims it at warm-up.
            let batch = model_lease::with_claim(app, &model, || {
                kinds::classify_batch(&agent, &endpoint, &model, api_key.as_deref(), &scenes)
            });
            match batch {
                Ok(labels) => {
                    for (hash, label) in hashes.iter().zip(labels) {
                        match label {
                            Some(kind) => {
                                kinds_file.kinds.insert(hash.clone(), kind);
                            }
                            // Left unlabelled rather than guessed; the next run picks it up.
                            None => unparsed += 1,
                        }
                    }
                }
                Err(error) => {
                    failures += 1;
                    // A dead endpoint would otherwise burn through every batch failing identically.
                    if failures >= VISION_FAIL_FAST_ATTEMPTS && kinds_file.kinds.is_empty() {
                        return Err(error);
                    }
                    eprintln!("[kinds] batch failed: {error}");
                }
            }

            processed += chunk.len();
            kinds_file.version = kinds::KIND_SCHEMA_VERSION;
            kinds_file.prompt_version = kinds::KIND_PROMPT_VERSION;
            kinds_file.generated_at = now_iso();
            kinds_file.model = model.clone();
            kinds_file.note = kinds::KINDS_NOTE.to_string();
            kinds::save_kinds(root, &kinds_file)?;

            let _ = app.emit(
                "kind-classification-progress",
                TextAnalysisProgress {
                    processed,
                    total,
                    current_name: format!("{} classified", kinds_file.kinds.len()),
                },
            );
        }

        let filled = propagate_and_save_kinds(root, &mut kinds_file, &model)?;
        let mut message = format!(
            "Classified {} images, {filled} more inherited from their videos.",
            kinds_file.kinds.len()
        );
        if unparsed > 0 {
            message.push_str(&format!(" {unparsed} unlabelled (run again to fill in)."));
        }
        if failures > 0 {
            message.push_str(&format!(" {failures} batches failed."));
        }
        Ok(("completed", Some(message)))
    })();

    control.running.store(false, Ordering::SeqCst);
    let (status, message) = match result {
        Ok((status, message)) => (status.to_string(), message),
        Err(error) => ("error".to_string(), Some(error)),
    };
    let _ = app.emit("kind-classification-finished", TextAnalysisFinished { status, message });
}

/// Recomputes the inherited kinds from the chunk plan and writes the sidecar. Always rebuilt from
/// scratch rather than accumulated, so a later classification round can revise what a video's
/// undescribed frames inherit. Returns how many frames ended up inheriting a kind.
fn propagate_and_save_kinds(
    root: &Path,
    kinds_file: &mut kinds::KindsFile,
    model: &str,
) -> Result<usize, String> {
    let groups: Vec<Vec<String>> = load_chunk_plan(root)
        .map(|plan| plan.groups.into_iter().map(|group| group.member_hashes).collect())
        .unwrap_or_default();
    kinds_file.propagated = kinds::propagate_kinds(&kinds_file.kinds, &groups);
    kinds_file.version = kinds::KIND_SCHEMA_VERSION;
    kinds_file.prompt_version = kinds::KIND_PROMPT_VERSION;
    kinds_file.generated_at = now_iso();
    kinds_file.model = model.to_string();
    kinds_file.note = kinds::KINDS_NOTE.to_string();
    kinds::save_kinds(root, kinds_file)?;
    Ok(kinds_file.propagated.len())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KindSummary {
    exists: bool,
    generated_at: Option<String>,
    classified: usize,
    /// Geo-tagged images still without a kind — what a run would work through.
    pending: usize,
    counts: HashMap<String, usize>,
    allowed_kinds: Vec<String>,
    kinds_path: String,
}

#[tauri::command]
fn get_kind_summary(root: String) -> Result<KindSummary, String> {
    let root_buf = root_path(&root)?;
    let file = kinds::load_kinds(&root_buf);
    // Counts cover own + inherited, because that is what set building actually filters against.
    let effective = file.effective();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for kind in effective.values() {
        *counts.entry(kind.clone()).or_insert(0) += 1;
    }
    // Pending = geo-tagged images that still have no kind from either route.
    let pending = geo::load_geo(&root_buf)
        .map(|geo| {
            geo.images
                .keys()
                .filter(|hash| !effective.contains_key(*hash))
                .count()
        })
        .unwrap_or(0);
    Ok(KindSummary {
        exists: !effective.is_empty(),
        generated_at: (!file.generated_at.is_empty()).then(|| file.generated_at.clone()),
        classified: effective.len(),
        pending,
        counts,
        allowed_kinds: kinds::allowed_kinds(&file),
        kinds_path: kinds::kinds_path(&root_buf).to_string_lossy().to_string(),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepropagateResult {
    /// Frames that inherited a kind from their video's described frames.
    propagated: usize,
    /// Frames whose video's described frames disagreed, so nothing was vouched for.
    mixed: usize,
    /// How many of those changed away from a real kind — the mislabels this run removed.
    corrected: usize,
}

/// Re-runs ONLY the inheritance step: which unlooked-at frames may take their video's scene kind.
///
/// Pure and instant — it recomputes a derived field from the labels already on disk and calls no
/// model. That is the whole point: when the propagation RULE changes, a library does not have to
/// re-classify ten thousand descriptions to get the benefit, and the frames a stricter rule now
/// declines to vouch for stop being set material as soon as the sets are rebuilt.
#[tauri::command]
fn repropagate_kinds(root: String) -> Result<RepropagateResult, String> {
    let root_buf = root_path(&root)?;
    let mut kinds_file = kinds::load_kinds(&root_buf);
    if kinds_file.kinds.is_empty() {
        return Err(
            "Nothing has been scene-classified yet, so there is nothing to inherit from. Run \
             Classify Scenes first."
                .to_string(),
        );
    }
    let before = kinds_file.propagated.clone();
    let model = kinds_file.model.clone();
    propagate_and_save_kinds(&root_buf, &mut kinds_file, &model)?;

    let mixed = kinds_file
        .propagated
        .values()
        .filter(|kind| kind.as_str() == kinds::KIND_MIXED)
        .count();
    // A frame that used to carry a real kind and is now held out is precisely a mislabel removed.
    let corrected = kinds_file
        .propagated
        .iter()
        .filter(|(hash, kind)| {
            kind.as_str() == kinds::KIND_MIXED
                && before.get(*hash).is_some_and(|old| old != kinds::KIND_MIXED)
        })
        .count();
    Ok(RepropagateResult {
        propagated: kinds_file.propagated.len() - mixed,
        mixed,
        corrected,
    })
}

#[tauri::command]
fn get_geo_summary(root: String) -> Result<GeoSummary, String> {
    let root_buf = root_path(&root)?;
    let geo_file = geo::load_geo(&root_buf);
    Ok(geo_summary(&root_buf, geo_file.as_ref()))
}

#[tauri::command]
fn get_geo_coverage(root: String) -> Result<Option<geo::CoverageView>, String> {
    let root_buf = root_path(&root)?;
    Ok(geo::load_geo(&root_buf).map(|file| geo::coverage_view(&root_buf, &file)))
}

#[tauri::command]
fn build_geo_sets(root: String, target_size: Option<usize>) -> Result<geo::GeoSetsFile, String> {
    let root_buf = root_path(&root)?;
    let geo_file = geo::load_geo(&root_buf)
        .ok_or_else(|| "No geo records yet. Run Derive Geo first.".to_string())?;
    let excluded = geo::load_excluded(&root_buf);
    let kind_file = kinds::load_kinds(&root_buf);
    let allowed = kinds::allowed_kinds(&kind_file);
    let built = geo::build_sets(
        &geo_file,
        target_size.unwrap_or(geo::DEFAULT_SET_SIZE),
        &excluded.excluded,
        &kind_file.effective(),
        &allowed,
        now_iso(),
    );
    let json = serde_json::to_string_pretty(&built)
        .map_err(|error| format!("Failed to serialize geo sets: {error}"))?;
    fs::write(geo::sets_path(&root_buf), json)
        .map_err(|error| format!("Failed to save geo sets: {error}"))?;
    Ok(built)
}

#[tauri::command]
fn get_geo_sets(root: String) -> Result<Option<geo::GeoSetsFile>, String> {
    let root_buf = root_path(&root)?;
    Ok(geo::load_sets(&root_buf))
}

/// Write time in epoch seconds, or `None` when the file has never been written.
fn modified_secs(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs())
}

/// Write time + size, which together move whenever a sidecar is rewritten — by this app or by
/// super-image-viewer, which owns the exclusion list.
fn file_stamp(path: &Path) -> String {
    match fs::metadata(path) {
        Ok(meta) => format!("{}:{}", modified_secs(path).unwrap_or(0), meta.len()),
        Err(_) => "-".to_string(),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeoStatusView {
    /// Changes whenever any geo sidecar is rewritten. The panel keeps the value it loaded with and
    /// re-checks it when the window comes back — the only way it can notice a write made by another
    /// app while it was in the background.
    fingerprint: String,
    /// When this answer was produced, so the panel can say out loud that it did look again.
    checked_at: String,
    #[serde(flatten)]
    status: geo::GeoStatus,
}

/// Whether what the Geo panel is showing has been overtaken by something written since.
///
/// Cheap on purpose: it stats five sidecars and parses the four SMALL ones, never the multi-megabyte
/// records file. That is what lets the panel re-check freshness on every focus rather than only when
/// the user happens to navigate away and back.
/// How many screenshots were TAKEN over the last `days`, from screenshot-tool's own log.
///
/// Deliberately says nothing about this library. The library counts what survived on disk; this
/// counts the act, and the two disagreeing after a cleanup is expected rather than a fault. The UI
/// shows them side by side and must never present the gap as missing data — see `capture_log`.
///
/// Cheap: a few hundred KB of TSV, versus the 14.6 GB the save folder was measured at. That is the
/// main reason this exists as a log at all rather than as a walk over the filenames.
#[tauri::command]
fn get_capture_activity(days: Option<u32>) -> capture_log::CaptureActivity {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    capture_log::read(days.unwrap_or(30).clamp(1, 3650), now_ms)
}

#[tauri::command]
fn get_geo_status(root: String) -> Result<GeoStatusView, String> {
    let root_buf = root_path(&root)?;
    let sets = geo::load_sets(&root_buf);
    let excluded = geo::load_excluded(&root_buf);
    let kind_file = kinds::load_kinds(&root_buf);
    let allowed = kinds::allowed_kinds(&kind_file);
    let kinds_map = kind_file.effective();

    let geo_file = geo::geo_path(&root_buf);
    let gazetteer_file = geo::gazetteer_path(&root_buf);
    let sets_file = geo::sets_path(&root_buf);
    let status = geo::status(&geo::StatusInput {
        sets: sets.as_ref(),
        excluded: &excluded.excluded,
        kinds: &kinds_map,
        allowed_kinds: &allowed,
        derived_at: modified_secs(&geo_file),
        gazetteer_at: modified_secs(&gazetteer_file),
        sets_at: modified_secs(&sets_file),
    });

    let fingerprint = [
        geo_file,
        gazetteer_file,
        sets_file,
        geo::excluded_path(&root_buf),
        kinds::kinds_path(&root_buf),
    ]
    .iter()
    .map(|path| file_stamp(path))
    .collect::<Vec<String>>()
    .join("|");

    Ok(GeoStatusView { fingerprint, checked_at: now_iso(), status })
}

/// Resolves a set's member hashes to real file paths so the set can be opened or handed to a viewer.
/// Hashes that no longer correspond to a file on disk are dropped rather than returned as blanks.
#[tauri::command]
fn get_geo_set_images(root: String, set_id: String) -> Result<Vec<ImageView>, String> {
    let root_buf = root_path(&root)?;
    let sets = geo::load_sets(&root_buf).ok_or_else(|| "No geo sets have been built.".to_string())?;
    let set = sets
        .sets
        .iter()
        .find(|candidate| candidate.id == set_id)
        .ok_or_else(|| format!("Set '{set_id}' not found."))?;

    // Consume the scan's images into a by-hash map rather than cloning out of it: `ImageView` is a
    // plain serialize-only view and there is no reason to make it Clone just for this.
    let view = scan_and_reconcile(&root_buf)?;
    let mut by_hash: HashMap<String, ImageView> = view
        .images
        .into_iter()
        .map(|image| (image.hash.clone(), image))
        .collect();
    Ok(set
        .members
        .iter()
        .filter_map(|hash| by_hash.remove(hash))
        .collect())
}

// ---- Set review ---------------------------------------------------------------------------
//
// The backward pass over the forward pipeline: what is wrong with the sets that already exist. See
// `review.rs`. Descriptions are read for set members only (a few thousand small files) rather than
// for the whole library, so opening the panel costs a fraction of a derive.

/// Reads relative path -> hash from the description index and inverts it, so a finding can say
/// which file it is about without a full library scan.
fn description_paths(root: &Path) -> HashMap<String, String> {
    let index_path = root.join(VISION_DESC_DIR_NAME).join(VISION_INDEX_FILE_NAME);
    let Ok(text) = fs::read_to_string(index_path) else {
        return HashMap::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HashMap::new();
    };
    let Some(by_path) = value.get("byPath").and_then(|value| value.as_object()) else {
        return HashMap::new();
    };
    by_path
        .iter()
        .filter_map(|(path, hash)| Some((hash.as_str()?.to_string(), path.clone())))
        .collect()
}

#[tauri::command]
fn review_geo_sets(root: String) -> Result<review::SetReview, String> {
    let root_buf = root_path(&root)?;
    let geo_file = geo::load_geo(&root_buf)
        .ok_or_else(|| "No geo records yet. Run Derive Geo first.".to_string())?;
    let sets = geo::load_sets(&root_buf)
        .ok_or_else(|| "No country sets have been built yet.".to_string())?;
    let kind_file = kinds::load_kinds(&root_buf);
    let kinds_map = kind_file.effective();
    let allowed = kinds::allowed_kinds(&kind_file);
    let group_titles: Vec<String> = load_chunk_plan(&root_buf)
        .map(|plan| plan.groups.into_iter().map(|group| group.title).collect())
        .unwrap_or_default();

    let desc_dir = root_buf.join(VISION_DESC_DIR_NAME);
    let mut members: Vec<&String> = sets
        .sets
        .iter()
        .flat_map(|set| set.members.iter())
        .collect();
    members.sort();
    members.dedup();
    let descriptions: HashMap<String, String> = members
        .into_iter()
        .filter_map(|hash| {
            fs::read_to_string(desc_dir.join(format!("{hash}.txt")))
                .ok()
                .map(|text| (hash.clone(), text))
        })
        .collect();

    let paths = description_paths(&root_buf);
    let input = review::ReviewInput {
        geo: &geo_file,
        sets: &sets,
        kinds: &kinds_map,
        allowed_kinds: &allowed,
        group_titles: &group_titles,
        descriptions: &descriptions,
        paths: &paths,
    };
    Ok(review::review(&input, now_iso()))
}

/// Writes the ticked fixes. Deliberately does NOT re-derive or rebuild: the frontend chains those
/// through the existing commands, so a fix and a rebuild stay separately visible (and separately
/// cancellable) rather than hiding a two-minute pipeline inside a checkbox.
#[tauri::command]
fn apply_geo_review(root: String, fixes: review::ReviewApply) -> Result<review::ReviewApplied, String> {
    let root_buf = root_path(&root)?;
    review::apply(&root_buf, &fixes, &now_iso())
}

/// One image behind a worklist string: the hash the frontend resolves against the library it is
/// already holding, plus what the model actually said about it. The description is the other half
/// of the evidence — a frame can be unreadable on its own and obvious from the sentence that
/// produced the location line.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeoLocationImage {
    hash: String,
    description: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeoLocationImages {
    /// Everything that carries the string, even when `images` was capped.
    total: usize,
    images: Vec<GeoLocationImage>,
}

/// The images whose OWN description carries one worklist string, so a decision can be made by
/// looking at them instead of guessing at the words. Matched exactly the way `derive` matches —
/// the raw `Location:` line, lowercased — so this is precisely the set that decision will retag.
///
/// Returns hashes rather than resolved paths: the frontend is already holding the scanned library,
/// so this stays a description read (parallel, no library walk) instead of a second full scan.
/// Frames that merely inherit the location from their video are deliberately absent — they are not
/// evidence for what the string means.
#[tauri::command]
fn get_geo_location_images(
    root: String,
    location: String,
    limit: usize,
) -> Result<GeoLocationImages, String> {
    let root_buf = root_path(&root)?;
    let needle = location.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(GeoLocationImages { total: 0, images: Vec::new() });
    }

    let mut matches: Vec<GeoLocationImage> = load_descriptions(&root_buf)
        .into_iter()
        .filter(|described| {
            geo::extract_location_line(&described.description)
                .is_some_and(|raw| raw.trim().to_lowercase() == needle)
        })
        .map(|described| GeoLocationImage {
            hash: described.hash,
            description: described.description,
        })
        .collect();

    // Hash order is directory order, i.e. arbitrary and unstable between calls. Sorting keeps the
    // strip in the same order every time it is opened, so "the third one" stays the third one.
    matches.sort_by(|a, b| a.hash.cmp(&b.hash));
    let total = matches.len();
    if limit > 0 {
        matches.truncate(limit);
    }
    Ok(GeoLocationImages { total, images: matches })
}

/// The hand-decision table, so the worklist can show what each unresolved string has already been
/// decided to mean — including decisions made by editing the file directly.
#[tauri::command]
fn get_geo_overrides(root: String) -> Result<BTreeMap<String, Option<String>>, String> {
    let root_buf = root_path(&root)?;
    Ok(geo::overrides(&root_buf))
}

/// Decide one worklist line in place. Writing the gazetteer is the whole fix — the decision only
/// reaches the records on the next derive, which is why the UI counts undrived decisions.
#[tauri::command]
fn set_geo_override(
    root: String,
    location: String,
    action: String,
    country: Option<String>,
) -> Result<BTreeMap<String, Option<String>>, String> {
    let root_buf = root_path(&root)?;
    geo::set_override(&root_buf, &location, &action, country.as_deref())
}

/// Opens the gazetteer in whatever the OS associates with .json — for bulk edits and for the parts
/// the worklist UI does not cover (`fictionTitlePatterns`).
#[tauri::command]
fn open_geo_gazetteer(root: String) -> Result<(), String> {
    let root_buf = root_path(&root)?;
    let path = geo::gazetteer_path(&root_buf);
    if !path.exists() {
        // Nothing to open before the first derive; write the empty template so the file always
        // exists once asked for.
        geo::save_gazetteer(&root_buf, &geo::Gazetteer::default())?;
    }
    open_image(path.to_string_lossy().to_string())
}

// The selectors behind both `Analyze New`'s pre-count and the passes themselves. What is pinned
// here is mostly *what does NOT count as new*: every one of these is a way the number on screen
// could quietly disagree with the run that follows it.
#[cfg(test)]
mod pending_selection_tests {
    use super::*;

    fn image(hash: &str, relative_path: &str, folder: &str) -> ImageView {
        ImageView {
            hash: hash.to_string(),
            path: format!("D:/library/{relative_path}"),
            thumbnail_path: None,
            relative_path: relative_path.to_string(),
            name: relative_path.rsplit('/').next().unwrap_or(relative_path).to_string(),
            source_folder: folder.to_string(),
            size: 1,
            modified_ms: 1,
            category: None,
            classified_by: None,
            classified_at: None,
            ocr_word_count: None,
            ocr_text_area_ratio: None,
            ocr_text_chars: None,
            nsfw_score: None,
            nsfw_labels: None,
            video_title: None,
            vision_desc_chars: None,
        }
    }

    fn library_view(images: Vec<ImageView>, folders: &[&str]) -> LibraryView {
        LibraryView {
            root: "D:/library".to_string(),
            source_pattern_preset: None,
            source_pattern_regex: None,
            ocr_word_threshold: 20,
            ocr_area_threshold: 0.1,
            nsfw_score_threshold: DEFAULT_NSFW_THRESHOLD,
            source_folders: folders
                .iter()
                .map(|name| SourceFolderView {
                    name: name.to_string(),
                    relative_path: name.to_string(),
                    is_manual: false,
                    image_count: 0,
                    included_in_analysis: true,
                })
                .collect(),
            categories: vec![],
            unclassified_count: 0,
            images,
            pending: PendingAnalysis::default(),
        }
    }

    /// The frontend's side of the mask table: sum the buckets that intersect the ticked passes.
    fn new_images_for(pending: &PendingAnalysis, selection: usize) -> usize {
        pending
            .by_pass_mask
            .iter()
            .enumerate()
            .filter(|(mask, _)| mask & selection != 0)
            .map(|(_, count)| count)
            .sum()
    }

    fn hashes(images: &[&ImageView]) -> Vec<String> {
        images.iter().map(|image| image.hash.clone()).collect()
    }

    #[test]
    fn a_scanned_but_untitled_frame_is_not_pending_for_video_dedup() {
        // `Some("")` means "we looked and there was no video marker". Treating it as pending would
        // re-OCR the title strip of every standalone screenshot on every single run.
        let mut config = LibraryConfig::default();
        config.images.insert("scanned".to_string(), ImageRecord {
            video_title: Some(String::new()),
            ..Default::default()
        });
        config.images.insert("titled".to_string(), ImageRecord {
            video_title: Some("Some Video".to_string()),
            ..Default::default()
        });
        let view = library_view(
            vec![
                image("scanned", "2026-01/a.png", "2026-01"),
                image("titled", "2026-01/b.png", "2026-01"),
                image("fresh", "2026-01/c.png", "2026-01"),
            ],
            &["2026-01"],
        );

        assert_eq!(hashes(&pending_chunk(&view, &config, false)), vec!["fresh"]);
        assert_eq!(pending_chunk(&view, &config, true).len(), 3, "force takes everything");
    }

    #[test]
    fn excluded_folders_and_categories_drop_out_of_every_pass() {
        let mut config = LibraryConfig::default();
        config.excluded_analysis_folders = vec!["skipme".to_string()];
        config.excluded_analysis_categories = vec!["High Text".to_string()];
        config.images.insert("categorized".to_string(), ImageRecord {
            category: Some("High Text".to_string()),
            ..Default::default()
        });
        let view = library_view(
            vec![
                image("kept", "2026-01/a.png", "2026-01"),
                image("infolder", "skipme/b.png", "skipme"),
                image("categorized", "2026-01/c.png", "2026-01"),
            ],
            &["2026-01", "skipme"],
        );

        assert_eq!(hashes(&pending_text(&view, &config, false)), vec!["kept"]);
        assert_eq!(hashes(&pending_nsfw(&view, &config, false)), vec!["kept"]);
        assert_eq!(hashes(&pending_text_extraction(&view, &config, false)), vec!["kept"]);
        assert_eq!(hashes(&pending_chunk(&view, &config, false)), vec!["kept"]);
        assert!(analysis_has_included_folder(&view, &config));
    }

    #[test]
    fn a_library_with_every_folder_excluded_is_not_the_same_as_a_finished_one() {
        let mut config = LibraryConfig::default();
        config.excluded_analysis_folders = vec!["2026-01".to_string()];
        let view = library_view(vec![image("a", "2026-01/a.png", "2026-01")], &["2026-01"]);
        assert!(!analysis_has_included_folder(&view, &config));

        // A library with no source folders at all is analyzed out of the root, not excluded.
        let rootonly = library_view(vec![image("a", "a.png", ROOT_SOURCE_FOLDER)], &[]);
        assert!(analysis_has_included_folder(&rootonly, &LibraryConfig::default()));
    }

    #[test]
    fn describe_reports_where_its_missing_images_went() {
        // The four reasons Describe's queue is shorter than the library, each of which has to be
        // countable — "3 to describe" out of thousands reads as a bug until the skips explain it.
        let mut config = LibraryConfig::default();
        config.excluded_analysis_categories = vec!["High Text".to_string()];
        config.images.insert("explicit".to_string(), ImageRecord {
            nsfw_score: Some(0.99),
            ..Default::default()
        });
        config.images.insert("unsampled".to_string(), ImageRecord {
            nsfw_score: Some(0.0),
            ..Default::default()
        });
        config.images.insert("sampled".to_string(), ImageRecord {
            nsfw_score: Some(0.0),
            ..Default::default()
        });
        config.images.insert("hightext".to_string(), ImageRecord {
            nsfw_score: Some(0.0),
            category: Some("High Text".to_string()),
            ..Default::default()
        });
        config.images.insert("described".to_string(), ImageRecord {
            nsfw_score: Some(0.0),
            vision_desc_chars: Some(400),
            ..Default::default()
        });
        let view = library_view(
            vec![
                image("explicit", "2026-01/a.png", "2026-01"),
                image("unsampled", "2026-01/b.png", "2026-01"),
                image("sampled", "2026-01/c.png", "2026-01"),
                image("hightext", "2026-01/d.png", "2026-01"),
                image("described", "2026-01/e.png", "2026-01"),
                image("unscored", "2026-01/f.png", "2026-01"),
            ],
            &["2026-01"],
        );
        let plan = ChunkPlan {
            version: 1,
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            samples_per_group: 1,
            groups: vec![chunker::ChunkGroup {
                title: "Some Video".to_string(),
                member_hashes: vec!["sampled".to_string(), "unsampled".to_string()],
                selected_hashes: vec!["sampled".to_string()],
            }],
        };

        let (pending, skips) = pending_vision(&view, &config, Some(&plan), false);
        assert_eq!(hashes(&pending), vec!["sampled"]);
        assert_eq!(skips.explicit, 1);
        assert_eq!(skips.video, 1);
        assert_eq!(skips.category, 1);
        // An image nobody has scored yet is work the Explicit pass unlocks, not work that is done.
        assert_eq!(skips.unscored, 1);
    }

    #[test]
    fn the_mask_table_answers_a_tick_combination_as_a_union_not_a_sum() {
        // "a" is new to Explicit and Text both; "b" only to Text. Ticking both must read as two
        // images to analyze, not three — the whole reason the counts ship as a table of masks.
        let mut config = LibraryConfig::default();
        config.images.insert("a".to_string(), ImageRecord {
            video_title: Some(String::new()),
            ocr_text_chars: Some(0),
            ..Default::default()
        });
        config.images.insert("b".to_string(), ImageRecord {
            nsfw_score: Some(0.0),
            video_title: Some(String::new()),
            ocr_text_chars: Some(0),
            ..Default::default()
        });
        let view = library_view(
            vec![
                image("a", "2026-01/a.png", "2026-01"),
                image("b", "2026-01/b.png", "2026-01"),
            ],
            &["2026-01"],
        );

        let pending = pending_analysis(&view, &config, None);
        assert_eq!(new_images_for(&pending, PASS_BIT_NSFW), 1);
        assert_eq!(new_images_for(&pending, PASS_BIT_TEXT), 2);
        assert_eq!(new_images_for(&pending, PASS_BIT_NSFW | PASS_BIT_TEXT), 2, "union, not 3");
        assert_eq!(new_images_for(&pending, PASS_BIT_CHUNK), 0);
        assert_eq!(pending.by_pass_mask.iter().sum::<usize>(), view.images.len(), "every image lands in exactly one bucket");
        assert_eq!(pending.eligible_images, 2);
    }

    #[test]
    fn the_mask_table_leaves_out_what_no_pass_would_touch() {
        // An image in an excluded folder is in bucket 0 with the finished ones: counted as a real
        // image, never as work. Nothing the readout adds up can include it.
        let mut config = LibraryConfig::default();
        config.excluded_analysis_folders = vec!["skipme".to_string()];
        let view = library_view(
            vec![
                image("kept", "2026-01/a.png", "2026-01"),
                image("dropped", "skipme/b.png", "skipme"),
            ],
            &["2026-01", "skipme"],
        );

        let pending = pending_analysis(&view, &config, None);
        assert_eq!(pending.eligible_images, 1);
        assert_eq!(new_images_for(&pending, PASS_MASK_COUNT - 1), 1, "every pass ticked still finds one");
        assert_eq!(pending.by_pass_mask[0], 1, "the excluded image sits in the no-work bucket");
    }

    #[test]
    fn duplicates_are_two_images_to_analyze_even_though_they_share_a_hash() {
        // The pre-count's "new images" number is a union over passes, and it counts files: both
        // copies of a duplicate are read, so counting by hash would under-report the run.
        let config = LibraryConfig::default();
        let view = library_view(
            vec![
                image("same", "2026-01/a.png", "2026-01"),
                image("same", "2026-02/a.png", "2026-02"),
            ],
            &["2026-01", "2026-02"],
        );

        let pending = pending_nsfw(&view, &config, false);
        assert_eq!(pending.len(), 2);
        let files: std::collections::HashSet<&str> =
            pending.iter().map(|image| image.relative_path.as_str()).collect();
        assert_eq!(files.len(), 2);
    }

    /// The same selectors over a real library, printed rather than asserted. Read-only on purpose:
    /// it reconstructs the view from the records already in the sidecar instead of scanning, so it
    /// can be run while a pass is mid-flight — a scan would save the sidecar back and the running
    /// pass would erase it. The cost of not scanning is that images added since the last scan are
    /// missing and records for deleted files linger, so treat the numbers as close, not exact.
    ///
    /// `ICAT_LIBRARY=<root> cargo test pending_counts_against_real_library -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn pending_counts_against_real_library() {
        let Ok(root) = std::env::var("ICAT_LIBRARY") else {
            eprintln!("set ICAT_LIBRARY to a library root");
            return;
        };
        let root = PathBuf::from(root);
        let config = load_library_config(&root);
        assert!(!config.images.is_empty(), "no records in {}", sidecar_path(&root).display());

        let images: Vec<ImageView> = config
            .images
            .iter()
            .map(|(hash, record)| {
                let relative = record.last_known_path.clone();
                let folder = record_source_folder(&relative).to_string();
                let mut view = image(hash, &relative, &folder);
                view.category = record.category.clone();
                view.ocr_word_count = record.ocr_word_count;
                view.ocr_text_chars = record.ocr_text_chars;
                view.nsfw_score = record.nsfw_score;
                view.video_title = record.video_title.clone();
                view.vision_desc_chars = record.vision_desc_chars;
                view
            })
            .collect();
        let folders: Vec<String> = images
            .iter()
            .map(|image| image.source_folder.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let view = library_view(images, &folders.iter().map(String::as_str).collect::<Vec<_>>());
        let plan = load_chunk_plan(&root);

        let (vision, skips) = pending_vision(&view, &config, plan.as_ref(), false);
        println!("{} records, folders included: {}", view.images.len(), analysis_has_included_folder(&view, &config));
        println!("  Explicit     {}", pending_nsfw(&view, &config, false).len());
        println!("  Video Dedup  {}", pending_chunk(&view, &config, false).len());
        println!("  Text         {}", pending_text(&view, &config, false).len());
        println!("  Extract Text {}", pending_text_extraction(&view, &config, false).len());
        println!("  Describe     {}  (skipped: {} unscored, {} explicit, {} deduped frames, {} omitted categories)",
            vision.len(), skips.unscored, skips.explicit, skips.video, skips.category);
    }

    #[test]
    fn ocr_word_count_leaves_explicit_images_alone_but_extraction_does_not() {
        let mut config = LibraryConfig::default();
        config.images.insert("explicit".to_string(), ImageRecord {
            nsfw_score: Some(0.99),
            ..Default::default()
        });
        let view = library_view(
            vec![
                image("explicit", "2026-01/a.png", "2026-01"),
                image("safe", "2026-01/b.png", "2026-01"),
            ],
            &["2026-01"],
        );

        assert_eq!(hashes(&pending_text(&view, &config, false)), vec!["safe"]);
        assert_eq!(pending_text_extraction(&view, &config, false).len(), 2);
    }
}

// Env-gated harness that runs the real geo derive against a real library and prints the resulting
// coverage, without a GUI or a model. Set ICAT_GEO_LIBRARY to the library root and run
// `cargo test geo_derive_against_real_library -- --ignored --nocapture`. Read-only: it derives in
// memory and writes nothing.
#[cfg(test)]
mod geo_real_library_tests {
    use super::*;

    #[test]
    #[ignore]
    fn geo_derive_against_real_library() {
        let Ok(root) = std::env::var("ICAT_GEO_LIBRARY") else {
            eprintln!("set ICAT_GEO_LIBRARY to a library root");
            return;
        };
        let root = PathBuf::from(root);

        let descriptions = load_descriptions(&root);
        let plan = load_chunk_plan(&root);
        let groups: Vec<geo::SourceGroup<'_>> = plan
            .as_ref()
            .map(|plan| {
                plan.groups
                    .iter()
                    .map(|group| geo::SourceGroup {
                        title: &group.title,
                        member_hashes: &group.member_hashes,
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Which title readings were judged the same video. Merging is the one step here that can
        // silently lose information, so the harness prints its biggest decisions to be eyeballed
        // rather than trusted.
        let titles: Vec<&str> = groups.iter().map(|group| group.title).collect();
        let canonical = geo::canonical_groups(&titles);
        let mut merged: HashMap<usize, Vec<usize>> = HashMap::new();
        for (index, root_index) in canonical.iter().enumerate() {
            merged.entry(*root_index).or_default().push(index);
        }
        let mut biggest: Vec<&Vec<usize>> = merged.values().filter(|list| list.len() > 1).collect();
        biggest.sort_by_key(|list| std::cmp::Reverse(list.len()));
        println!("\nlargest title merges (verify these really are one video each):");
        for list in biggest.iter().take(8) {
            println!("  {} readings:", list.len());
            for index in list.iter().take(4) {
                println!("      {:?}", titles[*index]);
            }
            if list.len() > 4 {
                println!("      … and {} more", list.len() - 4);
            }
        }
        println!();

        let mut gazetteer = geo::load_gazetteer(&root);
        let derived = geo::derive(&descriptions, &groups, &mut gazetteer, None, "test".into());
        let coverage = geo::coverage_view(&root, &derived);
        let stats = &derived.stats;

        println!("descriptions read      {}", stats.described);
        println!("with a Location: line  {}", stats.with_location);
        println!("tagged (own)           {}", stats.tagged_own);
        println!("tagged (propagated)    {}", stats.tagged_propagated);
        println!("TAGGED TOTAL           {}", stats.tagged_total);
        println!("rejected as junk       {}", stats.rejected_junk);
        println!("rejected ship registry {}", stats.rejected_registry_port);
        println!(
            "videos                 {} (from {} title readings — {} were OCR splits)",
            stats.sources,
            stats.source_groups,
            stats.source_groups.saturating_sub(stats.sources)
        );
        println!("unresolved images      {}", stats.unresolved_images);
        println!("unresolved strings     {}", stats.unresolved_strings);
        println!("fiction videos skipped {}", stats.fiction_groups_skipped);
        println!("countries seen         {}", stats.countries_seen);
        println!();
        for (tier, count) in &coverage.tiers {
            println!("  tier {tier:<6} {count}");
        }
        println!();
        for cluster in &coverage.clusters {
            let names: Vec<String> = cluster
                .countries
                .iter()
                .filter(|country| country.sources > 0)
                .map(|country| format!("{}:{}", country.name, country.sources))
                .collect();
            println!(
                "{:<28} {}/{} ready | {}",
                cluster.name,
                cluster.ready,
                cluster.total,
                names.join("  ")
            );
        }
        if !coverage.off_reference.is_empty() {
            println!("\noff-reference countries: {:?}", coverage.off_reference);
        }
        println!("\ntop unresolved (gazetteer worklist):");
        for entry in coverage.worklist.iter().take(20) {
            println!("  {:>4}  {}", entry.images, entry.location);
        }

        let excluded = geo::load_excluded(&root);
        let kind_file = kinds::load_kinds(&root);
        let allowed = kinds::allowed_kinds(&kind_file);
        let sets = geo::build_sets(
            &derived,
            geo::DEFAULT_SET_SIZE,
            &excluded.excluded,
            &kind_file.effective(),
            &allowed,
            "test".into(),
        );
        let diverse = sets.sets.iter().filter(|set| set.quality == "diverse").count();
        println!(
            "\nsets: {} total, {} diverse, {} limited",
            sets.sets.len(),
            diverse,
            sets.sets.len() - diverse
        );
        for set in sets.sets.iter().take(12) {
            println!(
                "  {:<22} {:>3} images  {:>3} videos  {}",
                set.title, set.size, set.sources, set.quality
            );
        }

        assert!(stats.tagged_total > 0, "the real library should produce geo tags");
    }
}

// Env-gated harness that classifies a sample of REAL descriptions through the configured local
// model and prints scene / kind pairs to eyeball. Proves the prompt before a 7k-image pass commits
// to it. Needs ICAT_GEO_LIBRARY (+ optional ICAT_KIND_SAMPLE, default 40); reads the endpoint,
// model and token from the app's own saved settings so no secret is handled here.
// `cargo test classify_kinds_sample -- --ignored --nocapture`
#[cfg(test)]
mod kind_sample_tests {
    use super::*;

    #[test]
    #[ignore]
    fn classify_kinds_sample() {
        let Ok(root) = std::env::var("ICAT_GEO_LIBRARY") else {
            eprintln!("set ICAT_GEO_LIBRARY");
            return;
        };
        let root = PathBuf::from(root);
        let sample_size: usize = std::env::var("ICAT_KIND_SAMPLE")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(40);

        // Sample from images that are actually IN sets — the population the filter has to fix.
        let sets = geo::load_sets(&root).expect("build sets first");
        let mut members: Vec<String> = sets
            .sets
            .iter()
            .flat_map(|set| set.members.iter().cloned())
            .collect();
        members.sort();
        members.dedup();
        // Deterministic spread across the sorted hash space rather than a clock-seeded shuffle.
        let step = (members.len() / sample_size.max(1)).max(1);
        let picked: Vec<String> = members.iter().step_by(step).take(sample_size).cloned().collect();

        let desc_dir = root.join(VISION_DESC_DIR_NAME);
        let mut scenes = Vec::new();
        let mut kept = Vec::new();
        for hash in &picked {
            let Ok(text) = fs::read_to_string(desc_dir.join(format!("{hash}.txt"))) else {
                continue;
            };
            scenes.push(kinds::scene_text(&text, 700));
            kept.push(hash.clone());
        }

        // Same location Tauri's app_data_dir resolves to on Windows, reached without an AppHandle.
        let settings_path = PathBuf::from(std::env::var("APPDATA").expect("APPDATA"))
            .join("com.slaur.image-categorizer")
            .join("settings.json");
        let settings: AppSettings = fs::read_to_string(&settings_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let endpoint = vision_endpoint(&settings);
        let model = vision_model(&settings);
        let api_key = vision_api_key(&settings);
        let agent = build_agent();

        let mut counts: HashMap<String, usize> = HashMap::new();
        for (chunk_index, chunk) in scenes.chunks(kinds::DEFAULT_BATCH_SIZE).enumerate() {
            let labels = kinds::classify_batch(&agent, &endpoint, &model, api_key.as_deref(), chunk)
                .expect("classify batch");
            for (offset, label) in labels.iter().enumerate() {
                let index = chunk_index * kinds::DEFAULT_BATCH_SIZE + offset;
                let kind = label.clone().unwrap_or_else(|| "<unparsed>".to_string());
                *counts.entry(kind.clone()).or_insert(0) += 1;
                let preview: String = scenes[index].chars().take(150).collect();
                println!("{kind:<10} | {preview}");
            }
        }
        println!("\ncounts: {counts:?}");
        assert!(!counts.is_empty());
    }
}

// A RUNNING window's taskbar icon comes from WM_SETICON, not from the exe's embedded .ico — Tauri
// seeds it from the .ico unless it is overridden, and Windows then upscales a too-small entry at
// scaled DPI, which is what a fuzzy taskbar icon actually is. Handing it a 1024x1024 buffer lets
// Windows do the downscale itself, crisply, at whatever in-between size it asks for. The raw RGBA
// blob is generated from icon.png by icons/build-icons.py; `include_bytes!` gives a `'static` slice
// so `Image::new` borrows it rather than allocating 4 MB at startup.
fn apply_window_icon(window: &tauri::WebviewWindow) {
    let bytes = include_bytes!("../icons/icon.rgba");
    let _ = window.set_icon(tauri::image::Image::new(bytes, 1024, 1024));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let headless_refresh = std::env::args().any(|arg| arg == HEADLESS_REFRESH_ARG);

    // In headless mode, drop the declarative "main" window from the generated config entirely —
    // rather than merely skipping `.show()` on it — because the frontend's own startup script
    // (`renderer.js`'s `init()`) unconditionally shows the window and kicks off its own scan of
    // the last-used root once it loads. Not creating the webview at all is what actually keeps
    // this run invisible and avoids it racing the GUI's logic against this function's own passes.
    let mut context = tauri::generate_context!();
    if headless_refresh {
        context.config_mut().app.windows.clear();
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(AnalysisControl::default())
        .manage(NsfwControl::default())
        .manage(OcrTextControl::default())
        .manage(ChunkControl::default())
        .manage(VisionControl::default())
        .manage(KindControl::default())
        .manage(ModelLease::default())
        .manage(TextIndexCache::default())
        .manage(TopicControl::default())
        .setup(move |app| {
            let startup_settings = load_app_settings(&app.handle().clone());
            if let Some(window) = app.get_webview_window("main") {
                apply_window_icon(&window);
                // Before the frontend unhides it, so the window never appears at one place and then
                // jumps to another. The config's size is the fallback when nothing was saved.
                restore_saved_window_bounds(&window, &startup_settings);
            }
            // Arm the idle lease before anything can send a request: the `ttl` is bound by LM Studio
            // at load time, so a request that goes out before this is set would pin the model for
            // the server's default hour. Headless refresh gets the watchdog too — with no window to
            // report activity, its model is released as soon as the run stops.
            apply_idle_ttl(&startup_settings);
            model_lease::spawn_watchdog(app.handle().clone());
            if headless_refresh {
                let cancel_item = MenuItemBuilder::with_id("cancel-refresh", "Cancel refresh").build(app)?;
                let cancel_item_id = cancel_item.id().clone();
                let menu = MenuBuilder::new(app).item(&cancel_item).build()?;

                let mut tray_builder = TrayIconBuilder::new()
                    .tooltip("Image Categorizer — nightly refresh running")
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(move |app, event| {
                        if event.id() == &cancel_item_id {
                            cancel_all_passes(app);
                        }
                    });
                if let Some(icon) = app.default_window_icon().cloned() {
                    tray_builder = tray_builder.icon(icon);
                }
                tray_builder.build(app)?;

                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    run_headless_refresh(&app_handle);
                });
            } else {
                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500));
                    if let Some(window) = app_handle.get_webview_window("main") {
                        let _ = window.show();
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            add_manual_source_folder,
            analyze_nsfw,
            apply_geo_review,
            analyze_text,
            analyze_vision,
            assign_category,
            build_chunk_plan,
            build_geo_sets,
            build_text_index,
            cancel_chunk_scan,
            cancel_kind_classification,
            cancel_nsfw_analysis,
            cancel_text_analysis,
            cancel_text_extraction,
            cancel_topics,
            cancel_vision_analysis,
            choose_root_folder,
            clear_window_defaults,
            classify_kinds,
            categorize_images,
            create_category,
            delete_category,
            derive_geo,
            discard_chunk_plan,
            download_nsfw_model,
            extract_text,
            get_app_settings,
            get_auto_refresh_settings,
            get_auto_refresh_run,
            cancel_auto_refresh_run,
            get_capture_activity,
            get_chunk_plan,
            get_geo_coverage,
            get_geo_location_images,
            get_geo_overrides,
            get_geo_set_images,
            get_geo_sets,
            get_geo_status,
            get_geo_summary,
            get_image_text,
            get_kind_summary,
            get_nsfw_model_info,
            generate_topics,
            get_text_status,
            get_text_timeline,
            get_topic_status,
            get_vision_idle_status,
            get_vision_settings,
            get_window_defaults,
            import_images,
            list_vision_models,
            load_vision_model,
            move_image,
            note_app_activity,
            open_geo_gazetteer,
            open_image,
            open_root_folder,
            regenerate_chunk_plan,
            remove_manual_source_folder,
            rename_category,
            repropagate_kinds,
            review_geo_sets,
            reveal_image,
            save_window_defaults,
            scan_library,
            search_text,
            select_root_folder,
            set_auto_refresh_settings,
            set_dark_mode,
            set_folder_analysis_included,
            set_category_analysis_included,
            set_geo_override,
            set_nsfw_threshold,
            set_source_pattern,
            set_text_index_folder_included,
            set_text_thresholds,
            set_tile_size,
            set_vision_settings
        ])
        .run(context)
        .expect("error while running tauri application");
}

// End-to-end harness (env-gated) that runs the whole non-GUI pipeline — real scan, real title-strip
// OCR, real grouping/sampling, and the real `describe_image` HTTP path — against a stub endpoint,
// with Claude standing in for the vision model (its per-image descriptions live in DESCRIPTIONS,
// routed by filename and handed to the stub one image at a time). Proves the plumbing on real
// screenshots without a running LM Studio. Set ICAT_TEST_LIBRARY, ICAT_TEST_VISION_ENDPOINT, and
// ICAT_TEST_STUB_RESPONSE_FILE to run it.
#[cfg(test)]
mod e2e_tests {
    use super::*;

    const PYRENEES_DESC: &str = "A first-person dashcam driving still on a two-lane mountain highway, filmed from inside a moving car. The road curves gently right with a white van ahead; a dry-stone retaining wall topped with rockfall netting climbs a steep rock face on the right, while a roadside billboard reading 'caldea' and forested green mountains rise on the left under a clear blue sky with bright sun. The browser title bar reads 'Driving across the Pyrenees mountains from France FR to Andorra AD - YouTube', identifying a YouTube driving video, and the 'caldea' billboard is an Andorran thermal-spa brand consistent with the Andorra approach.\nLocation: Pyrenees mountains, on the France to Andorra route.";

    const NYC_DESC: &str = "A dark night-time aerial shot looking down on a single low, brightly lit commercial building beside a large parking lot, with a tall illuminated pole flying a US flag in the foreground and scattered light poles, a few parked cars, and mostly black surroundings. The window title bar reads 'New York City Skyline at Night Live Screensaver HD, Aerial Landscapes Wallpaper HD Live - YouTube', so the source video claims a New York City skyline, though the visible frame shows an isolated lit building and lot rather than a recognizable skyline. The American flag is consistent with a United States location.\nLocation: United States (title claims New York City; not confirmed by the visible frame).";

    const VSCODE_DESC: &str = "A screenshot of the Visual Studio Code editor (not a video), with the 'aikoodaus' workspace open and several Claude Code chat panels tiled side by side — visible tab titles include 'Evaluate image categorizer Tauri', 'Build neon city asset package', and 'Add deep mining mode to voxel-frontier'. A right-hand sidebar lists a chat history under CHAT / CLAUDE CODE / CODEX, and terminal panes at the bottom show pwsh/node sessions (agent-asset-forge, asset-forge) with a dev server on 127.0.0.1. This is a software-development screenshot, so no geographic location applies.\nLocation: none (code editor screenshot).";

    fn my_description_for(name: &str) -> &'static str {
        if name.contains("052645_109") {
            NYC_DESC
        } else if name.contains("075543_178") {
            VSCODE_DESC
        } else {
            PYRENEES_DESC
        }
    }

    #[test]
    fn end_to_end_describe_with_claude_as_the_model() {
        let (Ok(root_str), Ok(endpoint)) =
            (std::env::var("ICAT_TEST_LIBRARY"), std::env::var("ICAT_TEST_VISION_ENDPOINT"))
        else {
            eprintln!("skipping e2e: ICAT_TEST_LIBRARY / ICAT_TEST_VISION_ENDPOINT not set");
            return;
        };
        let resp_file = std::env::var("ICAT_TEST_STUB_RESPONSE_FILE").expect("ICAT_TEST_STUB_RESPONSE_FILE");
        let model = "claude-as-stub";
        let root = Path::new(&root_str);

        // 1. Real scan — builds the sidecar, thumbnails, and one record per copied screenshot.
        let view = scan_and_reconcile(root).expect("scan");
        eprintln!("\n[1] scanned {} images", view.images.len());

        // 2. Mark every record NSFW-safe. In real use you run Explicit first; Describe skips explicit
        //    AND not-yet-scored images, so this stands in for that prerequisite.
        {
            let mut config = load_library_config(root);
            for record in config.images.values_mut() {
                record.nsfw_score = Some(0.0);
            }
            save_library_config(root, &config).unwrap();
        }

        // 3. Real Video Dedup: OCR each title strip, resolve the video title, then build the plan
        //    with samples_per_group = 2 to show de-duplication on the Pyrenees group.
        let mut chunk_results: Vec<(String, String)> = Vec::new();
        for image in &view.images {
            let strip = ocr::extract_title_strip(Path::new(&image.path), TITLE_STRIP_TOP_FRACTION).unwrap_or_default();
            chunk_results.push((image.hash.clone(), clean_title(&strip).unwrap_or_default()));
        }
        commit_chunk_results(root, &mut chunk_results).unwrap();

        let config = load_library_config(root);
        let titled: Vec<(String, String)> = config
            .images
            .iter()
            .filter_map(|(hash, record)| {
                record.video_title.as_ref().filter(|t| !t.is_empty()).map(|t| (hash.clone(), t.clone()))
            })
            .collect();
        let plan = build_plan(&titled, 2, now_iso(), None, false);
        save_chunk_plan(root, &plan).unwrap();
        eprintln!("[3] chunk plan: {} group(s)", plan.groups.len());
        for group in &plan.groups {
            eprintln!("    {:?}: {} frames -> {} selected", group.title, group.member_hashes.len(), group.selected_hashes.len());
        }

        // 4. Vision pass with Claude as the model: describe non-video images + only the sampled video
        //    frames, writing the real sidecars + index via the real functions.
        let mut selected = std::collections::HashSet::new();
        let mut video_members = std::collections::HashSet::new();
        for group in &plan.groups {
            for hash in &group.member_hashes {
                video_members.insert(hash.clone());
            }
            for hash in &group.selected_hashes {
                selected.insert(hash.clone());
            }
        }

        let desc_dir = root.join(VISION_DESC_DIR_NAME);
        fs::create_dir_all(&desc_dir).unwrap();
        let agent = build_agent();

        let mut described = 0usize;
        eprintln!("[4] describing:");
        for image in &view.images {
            if video_members.contains(&image.hash) && !selected.contains(&image.hash) {
                eprintln!("    SKIP (deduped video frame) {}", image.name);
                continue;
            }
            let my_desc = my_description_for(&image.name);
            fs::write(&resp_file, my_desc).unwrap();

            let returned = describe_image(&agent, &endpoint, model, None, DESCRIBE_PROMPT, Path::new(&image.path))
                .expect("describe_image should reach the stub");
            assert_eq!(returned.trim(), my_desc.trim(), "round-trip must return exactly what the model produced");

            let title = config.images.get(&image.hash).and_then(|r| r.video_title.clone()).filter(|t| !t.is_empty());
            let chars = write_vision_description(&desc_dir, &image.hash, &image.relative_path, &image.name, title.as_deref(), &returned, model).unwrap();
            commit_vision_results(root, &mut vec![(image.hash.clone(), chars)]).unwrap();
            described += 1;
            eprintln!("    DESCRIBED {} ({} chars)", image.name, chars);
        }
        write_vision_index(root, &desc_dir).unwrap();

        eprintln!("[done] described {described} images; plan + index written under {}", desc_dir.display());
        assert!(chunk_plan_path(root).exists(), "chunk plan file must exist");
        assert!(desc_dir.join(VISION_INDEX_FILE_NAME).exists(), "description index must exist");
        assert!(described >= 3, "should describe the sampled frames plus the standalone images");
    }
}
