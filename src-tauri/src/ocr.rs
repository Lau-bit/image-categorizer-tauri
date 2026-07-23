use std::cell::Cell;
use std::path::Path;

use windows::core::HSTRING;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::{OcrEngine, OcrResult};
use windows::Storage::{FileAccessMode, StorageFile};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows_future::{AsyncStatus, IAsyncOperation};

// `IAsyncOperation` only implements `IntoFuture` (no public blocking accessor), but everything
// here already runs on a dedicated OCR worker thread, so a short poll loop is fine.
fn block_on<T: windows_core::RuntimeType + 'static>(op: IAsyncOperation<T>) -> windows_core::Result<T> {
    loop {
        if op.Status()? != AsyncStatus::Started {
            return op.GetResults();
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

pub struct OcrStats {
    pub word_count: u32,
    pub text_area_ratio: f32,
}

thread_local! {
    static COM_INITIALIZED: Cell<bool> = Cell::new(false);
}

fn ensure_com_initialized() {
    COM_INITIALIZED.with(|flag| {
        if !flag.get() {
            unsafe {
                let _ = RoInitialize(RO_INIT_MULTITHREADED);
            }
            flag.set(true);
        }
    });
}

// Decodes `path`, runs the Windows OCR engine over it, and returns the recognition result along
// with the image's pixel width and height. Shared by the text-classification stats pass, the full
// text-extraction pass, and the title-strip pass so the heavy decode/recognize path lives in
// exactly one place.
fn recognize(path: &Path) -> Result<(OcrResult, f32, f32), String> {
    ensure_com_initialized();

    let path_str = path.to_string_lossy().to_string();
    let hpath = HSTRING::from(path_str.as_str());

    let file = block_on(StorageFile::GetFileFromPathAsync(&hpath).map_err(|error| format!("Failed to open file: {error}"))?)
        .map_err(|error| format!("Failed to open file: {error}"))?;

    let stream = block_on(
        file.OpenAsync(FileAccessMode::Read)
            .map_err(|error| format!("Failed to open file stream: {error}"))?,
    )
    .map_err(|error| format!("Failed to open file stream: {error}"))?;

    let decoder = block_on(
        BitmapDecoder::CreateAsync(&stream).map_err(|error| format!("Failed to create image decoder: {error}"))?,
    )
    .map_err(|error| format!("Unsupported or corrupt image: {error}"))?;

    let raw_bitmap = block_on(
        decoder
            .GetSoftwareBitmapAsync()
            .map_err(|error| format!("Failed to decode image: {error}"))?,
    )
    .map_err(|error| format!("Failed to decode image: {error}"))?;

    // OcrEngine only accepts Gray8 or Bgra8 pixel formats.
    let bitmap = SoftwareBitmap::ConvertWithAlpha(&raw_bitmap, BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied)
        .map_err(|error| format!("Failed to convert image for OCR: {error}"))?;

    let width = bitmap
        .PixelWidth()
        .map_err(|error| format!("Failed to read image size: {error}"))? as f32;
    let height = bitmap
        .PixelHeight()
        .map_err(|error| format!("Failed to read image size: {error}"))? as f32;

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|error| format!("Failed to start OCR engine: {error}"))?;

    let result = block_on(
        engine
            .RecognizeAsync(&bitmap)
            .map_err(|error| format!("OCR recognition failed: {error}"))?,
    )
    .map_err(|error| format!("OCR recognition failed: {error}"))?;

    Ok((result, width, height))
}

/// Reads back only the text found in the top `top_fraction` band of the image — the region that
/// holds a borderless browser/app title bar. Used by the video-chunking pass to pull the on-screen
/// window title (e.g. a YouTube video title) without OCRing (or being confused by) the rest of the
/// frame. A line counts as "in the band" when its highest word starts within the band.
pub fn extract_title_strip(path: &Path, top_fraction: f32) -> Result<String, String> {
    let (result, _width, height) = recognize(path)?;
    let band = (height * top_fraction).max(1.0);

    let lines = result
        .Lines()
        .map_err(|error| format!("Failed to read OCR lines: {error}"))?;
    let line_count = lines
        .Size()
        .map_err(|error| format!("Failed to read OCR lines: {error}"))?;

    let mut parts: Vec<String> = Vec::new();
    for line_index in 0..line_count {
        let line = lines
            .GetAt(line_index)
            .map_err(|error| format!("Failed to read OCR line: {error}"))?;
        let words = line
            .Words()
            .map_err(|error| format!("Failed to read OCR words: {error}"))?;
        let word_count = words
            .Size()
            .map_err(|error| format!("Failed to read OCR words: {error}"))?;
        if word_count == 0 {
            continue;
        }
        let mut line_top = f32::MAX;
        for word_index in 0..word_count {
            let word = words
                .GetAt(word_index)
                .map_err(|error| format!("Failed to read OCR word: {error}"))?;
            let rect = word
                .BoundingRect()
                .map_err(|error| format!("Failed to read OCR word bounds: {error}"))?;
            if rect.Y < line_top {
                line_top = rect.Y;
            }
        }
        if line_top <= band {
            let text = line
                .Text()
                .map_err(|error| format!("Failed to read OCR line text: {error}"))?;
            parts.push(text.to_string_lossy());
        }
    }

    Ok(parts.join(" "))
}

/// Reads back the full recognized text of an image, one OCR line per output line. Returns an
/// empty string when the engine found no text. Used by the text-extraction pass.
pub fn extract_image_text(path: &Path) -> Result<String, String> {
    let (result, _width, _height) = recognize(path)?;

    let lines = result
        .Lines()
        .map_err(|error| format!("Failed to read OCR lines: {error}"))?;
    let line_count = lines
        .Size()
        .map_err(|error| format!("Failed to read OCR lines: {error}"))?;

    let mut out = String::new();
    for line_index in 0..line_count {
        let line = lines
            .GetAt(line_index)
            .map_err(|error| format!("Failed to read OCR line: {error}"))?;
        let text = line
            .Text()
            .map_err(|error| format!("Failed to read OCR line text: {error}"))?;
        if line_index > 0 {
            out.push('\n');
        }
        out.push_str(&text.to_string_lossy());
    }
    Ok(out)
}

pub fn analyze_image_text(path: &Path) -> Result<OcrStats, String> {
    let (result, width, height) = recognize(path)?;
    let image_area = (width * height).max(1.0);

    let lines = result
        .Lines()
        .map_err(|error| format!("Failed to read OCR lines: {error}"))?;

    let line_count = lines
        .Size()
        .map_err(|error| format!("Failed to read OCR lines: {error}"))?;

    let mut word_count = 0u32;
    let mut text_area = 0f32;

    for line_index in 0..line_count {
        let line = lines
            .GetAt(line_index)
            .map_err(|error| format!("Failed to read OCR line: {error}"))?;
        let words = line
            .Words()
            .map_err(|error| format!("Failed to read OCR words: {error}"))?;
        let word_count_in_line = words
            .Size()
            .map_err(|error| format!("Failed to read OCR words: {error}"))?;

        for word_index in 0..word_count_in_line {
            let word = words
                .GetAt(word_index)
                .map_err(|error| format!("Failed to read OCR word: {error}"))?;
            let rect = word
                .BoundingRect()
                .map_err(|error| format!("Failed to read OCR word bounds: {error}"))?;
            word_count += 1;
            text_area += rect.Width * rect.Height;
        }
    }

    Ok(OcrStats {
        word_count,
        text_area_ratio: (text_area / image_area).clamp(0.0, 1.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercises the real Windows OCR path end to end against a rendered title-bar screenshot.
    // Env-gated so `cargo test` stays hermetic when the fixture isn't provided; the test harness
    // sets ICAT_TEST_TITLE_IMAGE to the image to run it.
    #[test]
    fn title_strip_reads_top_bar_and_excludes_content() {
        let Ok(path) = std::env::var("ICAT_TEST_TITLE_IMAGE") else {
            eprintln!("skipping title_strip test: ICAT_TEST_TITLE_IMAGE not set");
            return;
        };

        let strip = extract_title_strip(std::path::Path::new(&path), 0.06)
            .expect("title-strip OCR should succeed");
        eprintln!("OCR title strip = {strip:?}");

        let lower = strip.to_lowercase();
        assert!(lower.contains("youtube"), "should read the video-site marker in the top bar: {strip:?}");
        assert!(lower.contains("driving"), "should read the title text in the top bar: {strip:?}");
        assert!(
            !strip.to_uppercase().contains("CONTENTZONE"),
            "top-band filter must exclude content below the title bar: {strip:?}"
        );

        // Full pipeline: the read strip must resolve to a clean video title via the chunker.
        let title = crate::chunker::clean_title(&strip).expect("marker present -> Some(title)");
        eprintln!("cleaned title = {title:?}");
        assert!(title.to_lowercase().contains("driving"), "cleaned title should keep the real title: {title:?}");
        assert!(!title.to_lowercase().contains("youtube"), "cleaned title should drop the marker: {title:?}");
    }

    // Diagnostic (env-gated): OCR the title strip of up to ICAT_TEST_SCAN_LIMIT images (newest by
    // name) in ICAT_TEST_SCAN_DIR and print each result. Validates title reading on REAL screenshots
    // and surfaces which ones are video frames. Always passes — it's a scan tool, not an assertion.
    #[test]
    fn list_real_title_strips() {
        let Ok(dir) = std::env::var("ICAT_TEST_SCAN_DIR") else {
            eprintln!("skipping list_real_title_strips: ICAT_TEST_SCAN_DIR not set");
            return;
        };
        let limit: usize = std::env::var("ICAT_TEST_SCAN_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .expect("scan dir readable")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .and_then(|x| x.to_str())
                    .map(|x| matches!(x.to_lowercase().as_str(), "png" | "jpg" | "jpeg"))
                    .unwrap_or(false)
            })
            .collect();
        entries.sort();
        entries.reverse();

        let mut videos = 0usize;
        for path in entries.iter().take(limit) {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            match extract_title_strip(path, 0.06) {
                Ok(strip) => {
                    let title = crate::chunker::clean_title(&strip);
                    if title.is_some() {
                        videos += 1;
                    }
                    eprintln!("{name} => title={title:?} | strip={strip:?}");
                }
                Err(e) => eprintln!("{name} => ERROR {e}"),
            }
        }
        eprintln!("=== {videos} of {} looked like videos ===", entries.len().min(limit));
    }
}
