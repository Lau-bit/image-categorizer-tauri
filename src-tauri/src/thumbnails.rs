use std::path::{Path, PathBuf};

pub const THUMBNAIL_DIR_NAME: &str = ".image-categorizer-thumbnails";
const THUMBNAIL_MAX_DIM: u32 = 480;

/// Returns the cached thumbnail path for `source_path`, generating and caching it first if
/// missing. Returns `None` if the source format isn't decodable (e.g. HEIC), so callers can
/// fall back to the original file.
pub fn ensure_thumbnail(thumb_dir: &Path, hash: &str, source_path: &Path) -> Option<PathBuf> {
    let thumb_path = thumb_dir.join(format!("{hash}.jpg"));
    if thumb_path.is_file() {
        return Some(thumb_path);
    }

    let image = image::open(source_path).ok()?;
    image
        .thumbnail(THUMBNAIL_MAX_DIM, THUMBNAIL_MAX_DIM)
        .into_rgb8()
        .save(&thumb_path)
        .ok()?;
    Some(thumb_path)
}
