use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    fs::{self, File},
    hash::{Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager};

const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "webp", "tiff", "tif", "heic", "heif"];
const SIDECAR_FILE_NAME: &str = ".image-categorizer.json";
const MAX_SCAN_DEPTH: usize = 4;
const HASH_SAMPLE_BYTES: usize = 65536;

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppSettings {
    last_root: Option<String>,
    tile_size: Option<u32>,
    dark_mode: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageRecord {
    last_known_path: String,
    category: Option<String>,
    classified_by: Option<String>,
    classified_at: Option<String>,
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppSettingsView {
    last_root: Option<String>,
    last_root_exists: bool,
    tile_size: u32,
    dark_mode: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceFolderView {
    name: String,
    relative_path: String,
    is_manual: bool,
    image_count: usize,
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
    relative_path: String,
    name: String,
    source_folder: String,
    size: u64,
    modified_ms: u64,
    category: Option<String>,
    classified_by: Option<String>,
    classified_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryView {
    root: String,
    source_pattern_preset: Option<String>,
    source_pattern_regex: Option<String>,
    source_folders: Vec<SourceFolderView>,
    categories: Vec<CategoryView>,
    unclassified_count: usize,
    images: Vec<ImageView>,
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
    AppSettingsView {
        last_root: settings.last_root,
        last_root_exists,
        tile_size: clamp_tile_size(settings.tile_size.unwrap_or(DEFAULT_TILE_SIZE)),
        dark_mode: settings.dark_mode.unwrap_or(true),
    }
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

fn scan_and_reconcile(root: &Path) -> Result<LibraryView, String> {
    let mut config = load_library_config(root);
    let source_folders = detect_source_folders(root, &config)?;

    let mut all_images: Vec<ScannedImage> = Vec::new();
    for (folder_name, _) in &source_folders {
        collect_images_in_folder(root, folder_name, &root.join(folder_name), 0, &mut all_images)?;
    }

    let mut seen_hashes = std::collections::HashSet::new();
    for image in &all_images {
        seen_hashes.insert(image.hash.clone());
        let record = config.images.entry(image.hash.clone()).or_insert_with(|| ImageRecord {
            last_known_path: image.relative_path.clone(),
            category: None,
            classified_by: None,
            classified_at: None,
        });
        record.last_known_path = image.relative_path.clone();
    }
    config.images.retain(|hash, _| seen_hashes.contains(hash));

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
            relative_path: image.relative_path.clone(),
            name: image.name.clone(),
            source_folder: image.source_folder.clone(),
            size: image.size,
            modified_ms: image.modified_ms,
            category: record.category,
            classified_by: record.classified_by,
            classified_at: record.classified_at,
        });
    }

    image_views.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms).then_with(|| a.name.cmp(&b.name)));

    let mut folder_counts: HashMap<String, usize> = HashMap::new();
    for image in &all_images {
        *folder_counts.entry(image.source_folder.clone()).or_insert(0) += 1;
    }

    let source_folder_views = source_folders
        .into_iter()
        .map(|(name, is_manual)| SourceFolderView {
            relative_path: name.clone(),
            image_count: folder_counts.get(&name).copied().unwrap_or(0),
            name,
            is_manual,
        })
        .collect();

    let mut categories: Vec<CategoryView> = config
        .categories
        .iter()
        .map(|name| CategoryView {
            name: name.clone(),
            count: category_counts.get(name).copied().unwrap_or(0),
        })
        .collect();
    categories.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    Ok(LibraryView {
        root: root.to_string_lossy().to_string(),
        source_pattern_preset: config.source_pattern_preset.clone(),
        source_pattern_regex: config.source_pattern_regex.clone(),
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
    let mut settings = load_app_settings(&app);
    settings.last_root = Some(root.to_string_lossy().to_string());
    save_app_settings(&app, &settings)?;
    scan_and_reconcile(&root)
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            add_manual_source_folder,
            assign_category,
            choose_root_folder,
            create_category,
            delete_category,
            get_app_settings,
            move_image,
            open_image,
            open_root_folder,
            remove_manual_source_folder,
            rename_category,
            reveal_image,
            scan_library,
            set_dark_mode,
            set_source_pattern,
            set_tile_size
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
