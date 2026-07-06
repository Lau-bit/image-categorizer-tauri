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

const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "webp", "tiff", "tif", "heic", "heif"];
const SIDECAR_FILE_NAME: &str = ".image-categorizer.json";
const MAX_SCAN_DEPTH: usize = 4;
const HASH_SAMPLE_BYTES: usize = 65536;

// Extracted OCR text is written here, one `<hash>.txt` per image, so the folder stays stable
// across renames/moves and dedupes identical images — same keying scheme as the thumbnail cache.
const OCR_TEXT_DIR_NAME: &str = ".image-categorizer-ocr-text";

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
    let mut file = File::open(path).map_err(|error| format!("Failed to open {}: {error}", path.display()))?;
    let mut buffer = vec![0u8; HASH_SAMPLE_BYTES.min(size as usize).max(1)];
    let read = file
        .read(&mut buffer)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;

    let mut hasher = DefaultHasher::new();
    size.hash(&mut hasher);
    buffer[..read].hash(&mut hasher);
    Ok(format!("{:016x}", hasher.finish()))
}

fn collect_images_in_folder(
    root: &Path,
    source_folder: &str,
    folder: &Path,
    depth: usize,
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
            let size = metadata.len();
            let hash = hash_file(&path, size)?;
            let relative_path = path
                .strip_prefix(root)
                .map(|value| value.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| name.clone());
            images.push(ScannedImage {
                relative_path,
                absolute_path: path.clone(),
                name,
                source_folder: source_folder.to_string(),
                size,
                modified_ms: metadata.modified().map(system_time_ms).unwrap_or_default(),
                hash,
            });
        } else if path.is_dir() && depth < MAX_SCAN_DEPTH {
            collect_images_in_folder(root, source_folder, &path, depth + 1, images)?;
        }
    }
    Ok(())
}

fn collect_direct_images_in_folder(
    root: &Path,
    source_folder: &str,
    folder: &Path,
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
        let size = metadata.len();
        let hash = hash_file(&path, size)?;
        let relative_path = path
            .strip_prefix(root)
            .map(|value| value.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| name.clone());
        images.push(ScannedImage {
            relative_path,
            absolute_path: path,
            name,
            source_folder: source_folder.to_string(),
            size,
            modified_ms: metadata.modified().map(system_time_ms).unwrap_or_default(),
            hash,
        });
    }
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
            record.ocr_word_count = None;
            record.ocr_text_area_ratio = None;
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

    let mut all_images: Vec<ScannedImage> = Vec::new();
    collect_direct_images_in_folder(root, ROOT_SOURCE_FOLDER, root, &mut all_images)?;
    for (folder_name, _) in &source_folders {
        collect_images_in_folder(root, folder_name, &root.join(folder_name), 0, &mut all_images)?;
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
        let record = config.images.entry(image.hash.clone()).or_insert_with(|| ImageRecord {
            last_known_path: image.relative_path.clone(),
            category: None,
            classified_by: None,
            classified_at: None,
            ocr_word_count: None,
            ocr_text_area_ratio: None,
            ocr_text_chars: None,
            nsfw_score: None,
            nsfw_labels: None,
        });
        record.last_known_path = image.relative_path.clone();
    }
    config.images.retain(|hash, _| seen_hashes.contains(hash));

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
        let mut config = load_library_config(root_buf);
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

        let total = pending.len();
        let mut cancelled = false;

        for (index, (hash, path, name)) in pending.iter().enumerate() {
            if control.cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }

            match analyze_image_text(Path::new(path)) {
                Ok(stats) => {
                    if let Some(record) = config.images.get_mut(hash) {
                        record.ocr_word_count = Some(stats.word_count);
                        record.ocr_text_area_ratio = Some(stats.text_area_ratio);
                    }
                }
                Err(error) => {
                    eprintln!("OCR failed for {path}: {error}");
                }
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

        reclassify_text_categories(&mut config);
        save_library_config(root_buf, &config)?;

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
        let mut config = load_library_config(root_buf);
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

        let total = pending.len();
        let mut cancelled = false;

        for (index, (hash, path, name)) in pending.iter().enumerate() {
            if control.cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }

            match extract_image_text(Path::new(path)) {
                Ok(text) => {
                    let text_path = text_dir.join(format!("{hash}.txt"));
                    match fs::write(&text_path, &text) {
                        Ok(()) => {
                            if let Some(record) = config.images.get_mut(hash) {
                                record.ocr_text_chars = Some(text.chars().count() as u32);
                            }
                        }
                        Err(error) => eprintln!("Failed to save OCR text for {path}: {error}"),
                    }
                }
                Err(error) => {
                    eprintln!("Text extraction failed for {path}: {error}");
                }
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

        save_library_config(root_buf, &config)?;

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

#[tauri::command]
fn assign_category(root: String, hash: String, category: Option<String>) -> Result<LibraryView, String> {
    let root = root_path(&root)?;
    let mut config = load_library_config(&root);

    if let Some(category) = &category {
        if !config.categories.iter().any(|item| item == category) {
            return Err("Category does not exist.".to_string());
        }
    }

    let record = config.images.entry(hash).or_insert_with(ImageRecord::default);
    if category.is_some() {
        record.category = category;
        record.classified_by = Some("manual".to_string());
        record.classified_at = Some(now_iso());
    } else {
        record.category = None;
        record.classified_by = None;
        record.classified_at = None;
    }

    save_library_config(&root, &config)?;
    scan_and_reconcile(&root)
}

#[tauri::command]
fn move_image(root: String, hash: String, target_folder: String) -> Result<LibraryView, String> {
    let root_buf = root_path(&root)?;
    let config = load_library_config(&root_buf);
    let record = config.images.get(&hash).ok_or_else(|| "Image not found.".to_string())?;
    let source = root_buf.join(&record.last_known_path);
    if !source.is_file() {
        return Err("Source file no longer exists at the known path.".to_string());
    }

    let target_name = validate_child_name(&target_folder, "Folder")?;
    let target_dir = root_buf.join(&target_name);
    fs::create_dir_all(&target_dir).map_err(|error| format!("Failed to create target folder: {error}"))?;

    let file_name = path_name(&source);
    let mut destination = target_dir.join(&file_name);
    if destination != source {
        let stem = Path::new(&file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("image").to_string();
        let ext = Path::new(&file_name).extension().and_then(|s| s.to_str()).unwrap_or("").to_string();
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
        fs::rename(&source, &destination).map_err(|error| format!("Failed to move file: {error}"))?;
    }

    let mut config = load_library_config(&root_buf);
    if let Some(record) = config.images.get_mut(&hash) {
        record.last_known_path = destination
            .strip_prefix(&root_buf)
            .map(|value| value.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| file_name.clone());
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
        let mut config = load_library_config(root_buf);
        ensure_category(&mut config, EXPLICIT_CATEGORY);
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

        let total = pending.len();
        let mut cancelled = false;

        for (index, (hash, path, name)) in pending.iter().enumerate() {
            if control.cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            match analyze_image_nsfw(&mut session, Path::new(path)) {
                Ok(stats) => {
                    if let Some(record) = config.images.get_mut(hash) {
                        record.nsfw_score = Some(stats.score);
                        record.nsfw_labels = Some(stats.labels);
                    }
                }
                Err(e) => {
                    if let Some(record) = config.images.get_mut(hash) {
                        record.nsfw_score = Some(0.0);
                        record.nsfw_labels = Some(vec![format!("NSFW analysis error: {e}")]);
                    }
                    eprintln!("NSFW analysis failed for {path}: {e}");
                }
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

        reclassify_nsfw_categories(&mut config);
        reclassify_text_categories(&mut config);
        save_library_config(root_buf, &config)?;

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
            assign_category,
            cancel_nsfw_analysis,
            cancel_text_analysis,
            cancel_text_extraction,
            choose_root_folder,
            create_category,
            delete_category,
            download_nsfw_model,
            extract_text,
            get_app_settings,
            get_auto_refresh_settings,
            get_nsfw_model_info,
            move_image,
            open_image,
            open_root_folder,
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
            set_tile_size
        ])
        .run(context)
        .expect("error while running tauri application");
}
