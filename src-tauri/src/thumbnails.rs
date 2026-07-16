use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const THUMBNAIL_DIR_NAME: &str = ".image-categorizer-thumbnails";
const THUMBNAIL_MAX_DIM: u32 = 480;

// Makes each in-flight temp file unique, so two workers rendering concurrently never share one.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Returns the cached thumbnail path for `source_path`, generating and caching it first if
/// missing. Returns `None` if the source format isn't decodable (e.g. HEIC), so callers can
/// fall back to the original file.
pub fn ensure_thumbnail(thumb_dir: &Path, hash: &str, source_path: &Path) -> Option<PathBuf> {
    let thumb_path = thumb_dir.join(format!("{hash}.jpg"));
    if thumb_path.is_file() {
        return Some(thumb_path);
    }

    let image = image::open(source_path).ok()?;
    let thumbnail = image.thumbnail(THUMBNAIL_MAX_DIM, THUMBNAIL_MAX_DIM).into_rgb8();

    // Render to a private temp file and swap it in, rather than encoding straight to `thumb_path`.
    // Callers run this over the whole library with rayon, and duplicate files share a hash — so two
    // workers can target the identical path at once. Writing in place let them interleave inside a
    // half-written JPEG, and because `is_file()` is true from the first byte, the torn result was
    // cached forever. `rename` replaces the file atomically on Windows, so a reader sees the old
    // file or the new one and never a partial one.
    let temp_path = thumb_dir.join(format!(
        "{hash}.{}.tmp",
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if thumbnail.save_with_format(&temp_path, image::ImageFormat::Jpeg).is_err() {
        let _ = fs::remove_file(&temp_path);
        return None;
    }
    if fs::rename(&temp_path, &thumb_path).is_err() {
        let _ = fs::remove_file(&temp_path);
        return None;
    }
    Some(thumb_path)
}
