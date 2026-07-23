//! Local vision-model description pass.
//!
//! Sends each image to an OpenAI-compatible chat-completions endpoint (LM Studio at
//! `http://localhost:1234/v1/chat/completions` by default) with a base64 data URL, and returns the
//! model's prose description. The prose is what other apps (e.g. Local LLM Chat) read back as a
//! text stand-in for the picture.
//!
//! The image is re-encoded to a size-bounded JPEG before sending: it guarantees a format the model
//! accepts and keeps the POST body small enough that inference — not upload — is the bottleneck.

use std::io::Cursor;
use std::path::Path;
use std::time::Duration;

use image::ExtendedColorType;

/// Longest edge (px) the image is downscaled to before sending. Big enough to keep a title bar and
/// road signs legible to the model, small enough to keep the request light.
const MAX_EDGE: u32 = 1280;
const JPEG_QUALITY: u8 = 82;

/// The instruction that shapes every description. Bump `PROMPT_VERSION` in lib.rs when this changes
/// so re-runs can be reasoned about. It deliberately asks the model to (a) read any window/app title
/// bar text and (b) name any location, which is what makes the output useful for geo work.
pub const DESCRIBE_PROMPT: &str = "You are describing an image so another AI can understand and find it later from your text alone, without ever seeing the picture. \
First, read any application or browser/window title-bar text, tab title, or prominent on-screen UI caption, and quote it verbatim if present. \
Then write a concise, factual description that covers: the kind of content (photo, screenshot, app, game, map, video still); \
the location — name any country, region, city, road, or landmark that is either visible in the scene or stated in a title; \
notable objects, on-screen text, and the activity or subject. \
If the content indicates a geographic place, state that place explicitly on its own short line beginning 'Location:'. \
Describe only what is actually visible or written. Use 3 to 6 sentences. Do not add commentary about being an AI.";

/// Encodes bytes as standard (padded) base64. Kept dependency-free — it is a few lines and avoids
/// pulling in a crate just for the data URL.
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Loads `path`, downscales it to fit `MAX_EDGE`, and returns a `data:image/jpeg;base64,...` URL.
fn image_to_jpeg_data_url(path: &Path) -> Result<String, String> {
    let img = image::open(path).map_err(|e| format!("Cannot open image: {e}"))?;
    let (w, h) = (img.width(), img.height());
    let img = if w.max(h) > MAX_EDGE {
        let scale = MAX_EDGE as f32 / w.max(h) as f32;
        let nw = ((w as f32 * scale) as u32).max(1);
        let nh = ((h as f32 * scale) as u32).max(1);
        img.resize(nw, nh, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgb = img.to_rgb8();

    let mut buffer: Vec<u8> = Vec::new();
    {
        let mut cursor = Cursor::new(&mut buffer);
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, JPEG_QUALITY);
        encoder
            .encode(rgb.as_raw(), rgb.width(), rgb.height(), ExtendedColorType::Rgb8)
            .map_err(|e| format!("JPEG encode failed: {e}"))?;
    }

    Ok(format!("data:image/jpeg;base64,{}", base64_encode(&buffer)))
}

/// A ureq agent with sane timeouts for a local model. Connection is quick; a big vision request can
/// legitimately take a couple of minutes, so the read timeout is generous but bounded (a hung server
/// must not wedge the pass forever).
pub fn build_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(300))
        .build()
}

/// Describes one image via the endpoint, returning the model's trimmed prose.
pub fn describe_image(
    agent: &ureq::Agent,
    endpoint: &str,
    model: &str,
    prompt: &str,
    path: &Path,
) -> Result<String, String> {
    let data_url = image_to_jpeg_data_url(path)?;

    let body = serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": data_url } }
            ]
        }],
        "max_tokens": 512,
        "temperature": 0.2,
        "stream": false
    });
    let body_str = serde_json::to_string(&body).map_err(|e| format!("Failed to build request: {e}"))?;

    let response = agent
        .post(endpoint)
        .set("Content-Type", "application/json")
        .send_string(&body_str)
        .map_err(|e| format!("Vision request to {endpoint} failed: {e}"))?;

    let response_text = response
        .into_string()
        .map_err(|e| format!("Failed to read vision response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&response_text).map_err(|e| format!("Vision response was not JSON: {e}"))?;

    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            let preview: String = response_text.chars().take(240).collect();
            format!("Vision response had no text content. Preview: {preview}")
        })?;

    Ok(content.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The whole vision *request* is built without the LLM — if base64 is wrong the model just gets
    // garbage, so pin it to the RFC 4648 vectors before anyone blames the endpoint.
    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    // Env-gated (needs ICAT_TEST_TITLE_IMAGE): proves the image actually re-encodes to a JPEG data
    // URL the endpoint would accept — everything up to the HTTP call.
    #[test]
    fn jpeg_data_url_is_well_formed() {
        let Ok(path) = std::env::var("ICAT_TEST_TITLE_IMAGE") else {
            eprintln!("skipping jpeg data-url test: ICAT_TEST_TITLE_IMAGE not set");
            return;
        };
        let url = image_to_jpeg_data_url(std::path::Path::new(&path)).expect("should encode a data URL");
        assert!(url.starts_with("data:image/jpeg;base64,"), "wrong prefix: {}", &url[..40.min(url.len())]);

        let payload = &url["data:image/jpeg;base64,".len()..];
        assert!(payload.len() > 100, "payload too small: {}", payload.len());
        assert_eq!(payload.len() % 4, 0, "base64 must be padded to a multiple of 4");
        assert!(
            payload.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='),
            "payload contains non-base64 characters"
        );
        // The JPEG SOI marker (0xFF 0xD8) always base64-encodes to a leading "/9".
        assert!(payload.starts_with("/9"), "payload should decode to a JPEG: {}", &payload[..8]);
    }
}
