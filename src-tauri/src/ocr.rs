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
// with the image's pixel area. Shared by both the text-classification stats pass and the full
// text-extraction pass so the heavy decode/recognize path lives in exactly one place.
fn recognize(path: &Path) -> Result<(OcrResult, f32), String> {
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
    let image_area = (width * height).max(1.0);

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|error| format!("Failed to start OCR engine: {error}"))?;

    let result = block_on(
        engine
            .RecognizeAsync(&bitmap)
            .map_err(|error| format!("OCR recognition failed: {error}"))?,
    )
    .map_err(|error| format!("OCR recognition failed: {error}"))?;

    Ok((result, image_area))
}

/// Reads back the full recognized text of an image, one OCR line per output line. Returns an
/// empty string when the engine found no text. Used by the text-extraction pass.
pub fn extract_image_text(path: &Path) -> Result<String, String> {
    let (result, _image_area) = recognize(path)?;

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
    let (result, image_area) = recognize(path)?;

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
