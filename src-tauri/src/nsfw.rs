use std::path::Path;

use image::RgbImage;
use ndarray::Array4;
use ort::session::Session;
use ort::value::TensorRef;

const MODEL_INPUT_SIZE: u32 = 320;
const DETECTION_MIN_CONFIDENCE: f32 = 0.25;

// NudeNet v3 320n.onnx class order from the packaged model.
pub const CLASS_NAMES: &[&str] = &[
    "FEMALE_GENITALIA_COVERED", // 0
    "FACE_FEMALE",              // 1
    "BUTTOCKS_EXPOSED",         // 2
    "FEMALE_BREAST_EXPOSED",    // 3
    "FEMALE_GENITALIA_EXPOSED", // 4
    "MALE_BREAST_EXPOSED",      // 5
    "ANUS_EXPOSED",             // 6
    "FEET_EXPOSED",             // 7
    "BELLY_COVERED",            // 8
    "FEET_COVERED",             // 9
    "ARMPITS_COVERED",          // 10
    "ARMPITS_EXPOSED",          // 11
    "FACE_MALE",                // 12
    "BELLY_EXPOSED",            // 13
    "MALE_GENITALIA_EXPOSED",   // 14
    "ANUS_COVERED",             // 15
    "FEMALE_BREAST_COVERED",    // 16
    "BUTTOCKS_COVERED",         // 17
];

// Indices treated as sexually explicit content
const EXPLICIT_INDICES: &[usize] = &[2, 3, 4, 5, 6, 14];

pub struct NsfwStats {
    pub score: f32,
    pub labels: Vec<String>,
}

pub fn create_session(model_path: &Path) -> Result<Session, String> {
    Session::builder()
        .map_err(|e| format!("Failed to create ORT builder: {e}"))?
        .commit_from_file(model_path)
        .map_err(|e| format!("Failed to load NudeNet model from {}: {e}", model_path.display()))
}

pub fn analyze_image_nsfw(session: &mut Session, path: &Path) -> Result<NsfwStats, String> {
    let img = image::open(path)
        .map_err(|e| format!("Cannot open image: {e}"))?
        .into_rgb8();

    let tensor = letterbox_to_tensor(&img);

    let input_name = session
        .inputs()
        .first()
        .map(|input| input.name().to_string())
        .ok_or_else(|| "NudeNet model has no inputs.".to_string())?;

    let tensor_ref = TensorRef::from_array_view(tensor.view())
        .map_err(|e| format!("Failed to create tensor: {e}"))?;
    let outputs = session
        .run(ort::inputs![input_name.as_str() => tensor_ref])
        .map_err(|e| format!("Inference error using model input '{input_name}': {e}"))?;

    // Expected output shape: [1, 22, anchors]  (4 box coords + 18 class scores)
    let view = outputs[0]
        .try_extract_array::<f32>()
        .map_err(|e| format!("Cannot extract output tensor: {e}"))?;
    let shape = view.shape();

    let class_count = CLASS_NAMES.len();
    if shape.len() < 3 || shape[1] < 4 + class_count {
        return Err(format!("Unexpected model output shape: {shape:?}"));
    }

    let num_anchors = shape[2];
    let mut per_class_max = vec![0.0f32; class_count];

    for anchor in 0..num_anchors {
        for cls in 0..class_count {
            let score = view[[0, 4 + cls, anchor]];
            if score > per_class_max[cls] {
                per_class_max[cls] = score;
            }
        }
    }

    let score = EXPLICIT_INDICES
        .iter()
        .map(|&i| per_class_max[i])
        .fold(0.0f32, f32::max);

    let mut top_classes: Vec<(usize, f32)> = per_class_max
        .iter()
        .copied()
        .enumerate()
        .collect();
    top_classes.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut labels: Vec<String> = EXPLICIT_INDICES
        .iter()
        .filter(|&&i| per_class_max[i] >= DETECTION_MIN_CONFIDENCE)
        .map(|&i| format!("explicit {} {:.0}%", CLASS_NAMES[i], per_class_max[i] * 100.0))
        .collect();
    labels.extend(
        top_classes
            .into_iter()
            .take(6)
            .filter(|(_, score)| *score >= 0.01)
            .map(|(i, score)| format!("top {} {:.0}%", CLASS_NAMES[i], score * 100.0)),
    );
    if labels.is_empty() {
        labels.push("No NudeNet class reached 1% confidence.".to_string());
    }

    Ok(NsfwStats { score, labels })
}

fn letterbox_to_tensor(img: &RgbImage) -> Array4<f32> {
    let size = MODEL_INPUT_SIZE as usize;
    let (w, h) = img.dimensions();
    let scale = (size as f32 / w as f32).min(size as f32 / h as f32);
    let new_w = ((w as f32 * scale) as u32).max(1);
    let new_h = ((h as f32 * scale) as u32).max(1);
    let pad_x = (size - new_w as usize) / 2;
    let pad_y = (size - new_h as usize) / 2;

    let resized = image::imageops::resize(img, new_w, new_h, image::imageops::FilterType::Triangle);

    // Fill with neutral gray (114/255), then stamp resized pixels
    let mut tensor = Array4::<f32>::from_elem([1, 3, size, size], 114.0 / 255.0);
    for y in 0..new_h as usize {
        for x in 0..new_w as usize {
            let px = resized.get_pixel(x as u32, y as u32);
            tensor[[0, 0, pad_y + y, pad_x + x]] = px[0] as f32 / 255.0;
            tensor[[0, 1, pad_y + y, pad_x + x]] = px[1] as f32 / 255.0;
            tensor[[0, 2, pad_y + y, pad_x + x]] = px[2] as f32 / 255.0;
        }
    }
    tensor
}
