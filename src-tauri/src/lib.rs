use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{self, Read},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use rayon::prelude::*;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
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
use vision::{build_agent, describe_image, DESCRIBE_PROMPT};

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
    auto_refresh_low_priority: Option<bool>,
    auto_refresh_toast: Option<bool>,
    last_auto_refresh_at: Option<String>,
    last_auto_refresh_summary: Option<String>,
    // OpenAI-compatible vision endpoint (LM Studio by default) + the model name to send. Global, so
    // one setting drives the description pass across every library.
    vision_endpoint: Option<String>,
    vision_model: Option<String>,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AutoRefreshSettingsView {
    enabled: bool,
    time: String,
    roots: Vec<String>,
    run_nsfw: bool,
    run_text_analysis: bool,
    run_text_extraction: bool,
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
}

#[derive(Debug, Clone, Serialize)]
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
        })
        .collect();
    categories.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let (ocr_word_threshold, ocr_area_threshold) = ocr_thresholds(&config);
    let nsfw_score_threshold = nsfw_threshold(&config);

    Ok(LibraryView {
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
    })
}

fn launch_path(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]).arg(path);
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
    let mut config = load_library_config(&root);
    config.source_pattern_preset = preset;
    config.source_pattern_regex = regex.filter(|value| !value.trim().is_empty());
    save_library_config(&root, &config)?;
    scan_and_reconcile(&root)
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
        let excluded_folders: std::collections::HashSet<String> =
            config.excluded_analysis_folders.iter().cloned().collect();

        let included_folder_exists = view
            .source_folders
            .iter()
            .any(|folder| !excluded_folders.contains(&folder.name));
        if !view.source_folders.is_empty() && !included_folder_exists {
            return Ok(("completed", Some("No source folders are included in analysis.".to_string())));
        }

        let pending: Vec<(String, String, String)> = view
            .images
            .iter()
            .filter(|image| !excluded_folders.contains(&image.source_folder))
            .filter(|image| {
                config
                    .images
                    .get(&image.hash)
                    .and_then(|record| record.nsfw_score)
                    .map(|score| score < nsfw_threshold(&config))
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
    let mut config = load_library_config(&root_buf);
    config.ocr_word_threshold = Some(word_threshold);
    config.ocr_area_threshold = Some(area_threshold.clamp(0.0, 1.0));
    reclassify_text_categories(&mut config);
    save_library_config(&root_buf, &config)?;
    scan_and_reconcile(&root_buf)
}

#[tauri::command]
fn extract_text(app: AppHandle, control: tauri::State<'_, OcrTextControl>, root: String, force: bool) -> Result<(), String> {
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
        run_text_extraction(&app_handle, &root_buf, force);
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
fn run_text_extraction(app: &AppHandle, root_buf: &Path, force: bool) {
    let control = app.state::<OcrTextControl>();

    let result = (|| -> Result<(&'static str, Option<String>), String> {
        let view = scan_and_reconcile(root_buf)?;
        let config = load_library_config(root_buf);
        let excluded_folders: std::collections::HashSet<String> =
            config.excluded_analysis_folders.iter().cloned().collect();

        let text_dir = root_buf.join(OCR_TEXT_DIR_NAME);
        fs::create_dir_all(&text_dir)
            .map_err(|error| format!("Failed to create text folder: {error}"))?;

        let included_folder_exists = view
            .source_folders
            .iter()
            .any(|folder| !excluded_folders.contains(&folder.name));
        if !view.source_folders.is_empty() && !included_folder_exists {
            return Ok(("completed", Some("No source folders are included in extraction.".to_string())));
        }

        let pending: Vec<(String, String, String)> = view
            .images
            .iter()
            .filter(|image| !excluded_folders.contains(&image.source_folder))
            .filter(|image| {
                force
                    || config
                        .images
                        .get(&image.hash)
                        .map(|record| record.ocr_text_chars.is_none())
                        .unwrap_or(true)
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

    let mut config = load_library_config(&root);
    if !config.manual_source_folders.iter().any(|item| item == &name) {
        config.manual_source_folders.push(name);
        save_library_config(&root, &config)?;
    }
    scan_and_reconcile(&root)
}

#[tauri::command]
fn remove_manual_source_folder(root: String, folder_name: String) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    let mut config = load_library_config(&root);
    config.manual_source_folders.retain(|item| item != &folder_name);
    save_library_config(&root, &config)?;
    scan_and_reconcile(&root)
}

#[tauri::command]
fn set_folder_analysis_included(root: String, folder_name: String, included: bool) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    let mut config = load_library_config(&root);
    config.excluded_analysis_folders.retain(|item| item != &folder_name);
    if !included {
        config.excluded_analysis_folders.push(folder_name);
    }
    save_library_config(&root, &config)?;
    scan_and_reconcile(&root)
}

#[tauri::command]
fn create_category(root: String, name: String) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    let name = validate_child_name(&name, "Category")?;
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
        Command::new("explorer.exe")
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
        let excluded_folders: std::collections::HashSet<String> =
            config.excluded_analysis_folders.iter().cloned().collect();

        let pending: Vec<(String, String, String)> = view
            .images
            .iter()
            .filter(|img| !excluded_folders.contains(&img.source_folder))
            .filter(|img| {
                force
                    || config
                        .images
                        .get(&img.hash)
                        .map(|r| r.nsfw_score.is_none())
                        .unwrap_or(true)
            })
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
    let tr_value = format!("\"{}\" {HEADLESS_REFRESH_ARG}", exe.to_string_lossy());
    let status = Command::new("schtasks")
        .arg("/Create")
        .arg("/F")
        .arg("/SC")
        .arg("DAILY")
        .arg("/ST")
        .arg(time)
        .arg("/TN")
        .arg(AUTO_REFRESH_TASK_NAME)
        .arg("/TR")
        .arg(&tr_value)
        .arg("/RL")
        .arg("LIMITED")
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|error| format!("Failed to run schtasks: {error}"))?;
    if !status.success() {
        return Err("schtasks failed to create the scheduled task.".to_string());
    }
    Ok(())
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
    AutoRefreshSettingsView {
        enabled: settings.auto_refresh_enabled,
        time: settings.auto_refresh_time.unwrap_or_else(|| DEFAULT_AUTO_REFRESH_TIME.to_string()),
        roots: settings.auto_refresh_roots,
        run_nsfw: settings.auto_refresh_nsfw.unwrap_or(true),
        run_text_analysis: settings.auto_refresh_text_analysis.unwrap_or(true),
        run_text_extraction: settings.auto_refresh_text_extraction.unwrap_or(false),
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

#[tauri::command]
fn set_auto_refresh_settings(
    app: AppHandle,
    enabled: bool,
    time: String,
    roots: Vec<String>,
    run_nsfw: bool,
    run_text_analysis: bool,
    run_text_extraction: bool,
    low_priority: bool,
    toast: bool,
) -> Result<AutoRefreshSettingsView, String> {
    let time = validate_time_of_day(&time)?;
    let mut settings = load_app_settings(&app);
    settings.auto_refresh_enabled = enabled;
    settings.auto_refresh_time = Some(time.clone());
    settings.auto_refresh_roots = roots;
    settings.auto_refresh_nsfw = Some(run_nsfw);
    settings.auto_refresh_text_analysis = Some(run_text_analysis);
    settings.auto_refresh_text_extraction = Some(run_text_extraction);
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

    let total_roots = roots.len();
    let mut folders_done = 0usize;
    let mut cancelled = false;

    for root in &roots {
        let root_buf = PathBuf::from(root);
        if scan_and_reconcile(&root_buf).is_err() {
            continue;
        }

        if run_nsfw && !cancelled {
            let control = app.state::<NsfwControl>();
            control.running.store(true, Ordering::SeqCst);
            control.cancel.store(false, Ordering::SeqCst);
            run_nsfw_analysis(app, &root_buf, false);
            if app.state::<NsfwControl>().cancel.load(Ordering::SeqCst) {
                cancelled = true;
            }
        }
        if run_text_analysis_pass && !cancelled {
            let control = app.state::<AnalysisControl>();
            control.running.store(true, Ordering::SeqCst);
            control.cancel.store(false, Ordering::SeqCst);
            run_text_analysis(app, &root_buf, false);
            if app.state::<AnalysisControl>().cancel.load(Ordering::SeqCst) {
                cancelled = true;
            }
        }
        if run_text_extraction_pass && !cancelled {
            let control = app.state::<OcrTextControl>();
            control.running.store(true, Ordering::SeqCst);
            control.cancel.store(false, Ordering::SeqCst);
            run_text_extraction(app, &root_buf, false);
            if app.state::<OcrTextControl>().cancel.load(Ordering::SeqCst) {
                cancelled = true;
            }
        }

        folders_done += 1;
        if cancelled {
            break;
        }
    }

    let summary = if cancelled {
        format!(
            "Cancelled after {folders_done} of {total_roots} folder{}.",
            if total_roots == 1 { "" } else { "s" }
        )
    } else {
        format!("Completed {folders_done} folder{}.", if folders_done == 1 { "" } else { "s" })
    };

    let mut settings = load_app_settings(app);
    settings.last_auto_refresh_at = Some(now_iso());
    settings.last_auto_refresh_summary = Some(summary.clone());
    let _ = save_app_settings(app, &settings);

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
        let excluded_folders: std::collections::HashSet<String> =
            config.excluded_analysis_folders.iter().cloned().collect();

        let pending: Vec<(String, String, String)> = view
            .images
            .iter()
            .filter(|image| !excluded_folders.contains(&image.source_folder))
            .filter(|image| {
                force
                    || config
                        .images
                        .get(&image.hash)
                        .map(|record| record.video_title.is_none())
                        .unwrap_or(true)
            })
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VisionSettingsView {
    endpoint: String,
    model: String,
}

#[tauri::command]
fn get_vision_settings(app: AppHandle) -> VisionSettingsView {
    let settings = load_app_settings(&app);
    VisionSettingsView { endpoint: vision_endpoint(&settings), model: vision_model(&settings) }
}

#[tauri::command]
fn set_vision_settings(app: AppHandle, endpoint: String, model: String) -> Result<VisionSettingsView, String> {
    let mut settings = load_app_settings(&app);
    settings.vision_endpoint = Some(endpoint.trim().to_string()).filter(|value| !value.is_empty());
    settings.vision_model = Some(model.trim().to_string()).filter(|value| !value.is_empty());
    save_app_settings(&app, &settings)?;
    Ok(VisionSettingsView { endpoint: vision_endpoint(&settings), model: vision_model(&settings) })
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

        let view = scan_and_reconcile(root_buf)?;
        let config = load_library_config(root_buf);
        let threshold = nsfw_threshold(&config);
        let excluded_folders: std::collections::HashSet<String> =
            config.excluded_analysis_folders.iter().cloned().collect();

        // The chunk plan decides which video frames are allowed (only the sampled ones) and which
        // hashes are video members at all (the rest are non-video and always eligible).
        let plan = load_chunk_plan(root_buf);
        let mut selected: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut video_members: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(plan) = &plan {
            for group in &plan.groups {
                for hash in &group.member_hashes {
                    video_members.insert(hash.clone());
                }
                for hash in &group.selected_hashes {
                    selected.insert(hash.clone());
                }
            }
        }

        let desc_dir = root_buf.join(VISION_DESC_DIR_NAME);
        fs::create_dir_all(&desc_dir).map_err(|error| format!("Failed to create descriptions folder: {error}"))?;

        let mut skipped_video = 0usize;
        let mut skipped_explicit = 0usize;
        let mut skipped_unscored = 0usize;

        let pending: Vec<(String, String, String, String, Option<String>)> = view
            .images
            .iter()
            .filter(|image| !excluded_folders.contains(&image.source_folder))
            .filter(|image| {
                if video_members.contains(&image.hash) && !selected.contains(&image.hash) {
                    skipped_video += 1;
                    return false;
                }
                true
            })
            .filter(|image| match config.images.get(&image.hash).and_then(|r| r.nsfw_score) {
                Some(score) if score >= threshold => {
                    skipped_explicit += 1;
                    false
                }
                Some(_) => true,
                None => {
                    skipped_unscored += 1;
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
            let message = if notes.is_empty() {
                "No images needed description.".to_string()
            } else {
                format!("Nothing to describe. Skipped: {}.", notes.join(", "))
            };
            return Ok(("completed", Some(message)));
        }

        let agent = build_agent();
        let mut cancelled = false;
        let mut failures = 0usize;
        let mut results: Vec<(String, u32)> = Vec::new();

        for (index, (hash, path, name, relative_path, title)) in pending.iter().enumerate() {
            if control.cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            match describe_image(&agent, &endpoint, &model, DESCRIBE_PROMPT, Path::new(path)) {
                Ok(description) => {
                    match write_vision_description(&desc_dir, hash, relative_path, name, title.as_deref(), &description, &model) {
                        Ok(chars) => results.push((hash.clone(), chars)),
                        Err(error) => {
                            failures += 1;
                            eprintln!("Failed to save description for {path}: {error}");
                        }
                    }
                }
                Err(error) => {
                    failures += 1;
                    eprintln!("Vision description failed for {path}: {error}");
                }
            }

            // Commit each result promptly so a stop resumes with at most the in-flight image redone.
            commit_vision_results(root_buf, &mut results)?;

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
        Ok((if cancelled { "cancelled" } else { "completed" }, Some(message)))
    })();

    control.running.store(false, Ordering::SeqCst);
    let (status, message) = match result {
        Ok((status, message)) => (status.to_string(), message),
        Err(error) => ("error".to_string(), Some(error)),
    };
    let _ = app.emit("vision-analysis-finished", TextAnalysisFinished { status, message });
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
        .setup(move |app| {
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
                            app.state::<NsfwControl>().cancel.store(true, Ordering::SeqCst);
                            app.state::<AnalysisControl>().cancel.store(true, Ordering::SeqCst);
                            app.state::<OcrTextControl>().cancel.store(true, Ordering::SeqCst);
                            app.state::<ChunkControl>().cancel.store(true, Ordering::SeqCst);
                            app.state::<VisionControl>().cancel.store(true, Ordering::SeqCst);
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
            analyze_text,
            analyze_vision,
            assign_category,
            build_chunk_plan,
            cancel_chunk_scan,
            cancel_nsfw_analysis,
            cancel_text_analysis,
            cancel_text_extraction,
            cancel_vision_analysis,
            choose_root_folder,
            create_category,
            delete_category,
            discard_chunk_plan,
            download_nsfw_model,
            extract_text,
            get_app_settings,
            get_auto_refresh_settings,
            get_chunk_plan,
            get_nsfw_model_info,
            get_vision_settings,
            import_images,
            move_image,
            open_image,
            open_root_folder,
            regenerate_chunk_plan,
            remove_manual_source_folder,
            rename_category,
            reveal_image,
            scan_library,
            select_root_folder,
            set_auto_refresh_settings,
            set_dark_mode,
            set_folder_analysis_included,
            set_nsfw_threshold,
            set_source_pattern,
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

            let returned = describe_image(&agent, &endpoint, model, DESCRIBE_PROMPT, Path::new(&image.path))
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
