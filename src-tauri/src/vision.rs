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

/// Completion-token ceiling per description. Deliberately generous: modern local vision models
/// (e.g. Gemma 4) are *reasoning* models that spend hundreds–thousands of tokens "thinking" in a
/// separate `reasoning_content` channel BEFORE writing the answer into `content`. At the old value
/// of 512, reasoning on a busy screenshot consumed the whole budget and the response was cut off
/// (finish_reason: length) with an EMPTY `content` — which read to the app as "no text content" and
/// failed the image. A ceiling only caps runaway generations; well-behaved ones stop early on their
/// own, so raising it costs nothing on the common path while letting reasoning models finish.
const MAX_COMPLETION_TOKENS: u32 = 4096;

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
///
/// `api_key`, when `Some`, is sent as an `Authorization: Bearer <key>` header — LM Studio rejects
/// every request with `invalid_api_key` when its "Require API token" auth is on and no token is
/// supplied, so this is what makes the pass work against a token-protected server.
pub fn describe_image(
    agent: &ureq::Agent,
    endpoint: &str,
    model: &str,
    api_key: Option<&str>,
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
        "max_tokens": MAX_COMPLETION_TOKENS,
        "temperature": 0.2,
        "stream": false
    });
    let body_str = serde_json::to_string(&body).map_err(|e| format!("Failed to build request: {e}"))?;

    let mut request = agent.post(endpoint).set("Content-Type", "application/json");
    if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
        request = request.set("Authorization", &format!("Bearer {key}"));
    }

    let response = match request.send_string(&body_str) {
        Ok(response) => response,
        // Surface the server's own error body (LM Studio's is explicit, e.g. "An LM Studio API
        // token is required…" or "no model loaded") instead of a bare status code, so the failure
        // is actionable in the UI toast rather than a mystery.
        Err(ureq::Error::Status(code, response)) => return Err(status_error(endpoint, code, response)),
        Err(error) => return Err(format!("Vision request to {endpoint} failed: {error}")),
    };

    let response_text = response
        .into_string()
        .map_err(|e| format!("Failed to read vision response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&response_text).map_err(|e| format!("Vision response was not JSON: {e}"))?;

    let choice = &json["choices"][0];
    let content = choice["message"]["content"].as_str().map(str::trim).unwrap_or("");
    if !content.is_empty() {
        return Ok(content.to_string());
    }

    // Empty `content`. The dominant cause with local reasoning models: the model spent the whole
    // token budget in its `reasoning_content` channel and got cut off (finish_reason: length) before
    // emitting an answer. Diagnose it specifically so the toast is actionable instead of a raw JSON
    // dump — this is exactly what made Gemma-4 "error with no useful output".
    let finish = choice["finish_reason"].as_str().unwrap_or("unknown");
    let reasoned = choice["message"]["reasoning_content"].as_str().map(|s| !s.trim().is_empty()).unwrap_or(false);
    if reasoned && finish == "length" {
        return Err(format!(
            "The model used its entire {MAX_COMPLETION_TOKENS}-token budget on internal reasoning and \
             never wrote a description (finish_reason: length). This is a 'thinking' model that ran \
             out of room — use a vision model that answers without a long reasoning phase."
        ));
    }
    if reasoned {
        return Err(format!(
            "The model produced only internal reasoning and no description (finish_reason: {finish}). \
             It may not actually support image input in this configuration."
        ));
    }
    let preview: String = response_text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(240).collect();
    Err(format!(
        "The model returned an empty description (finish_reason: {finish}) — it likely can't read \
         images. Pick a vision-capable model. Raw response: {preview}"
    ))
}

/// Sends a text-only chat completion and returns the model's trimmed reply.
///
/// Same endpoint, auth and error handling as `describe_image`, minus the picture — used by passes
/// that reason over descriptions the vision pass already produced. That distinction matters for
/// cost: re-reading 10k images through a vision model is hours of GPU, whereas re-reading their
/// prose is a text pass that batches many items per request.
pub fn chat_text(
    agent: &ureq::Agent,
    endpoint: &str,
    model: &str,
    api_key: Option<&str>,
    prompt: &str,
    max_tokens: u32,
) -> Result<String, String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "max_tokens": max_tokens,
        "temperature": 0.0,
        "stream": false
    });
    let body_str = serde_json::to_string(&body).map_err(|e| format!("Failed to build request: {e}"))?;

    let mut request = agent.post(endpoint).set("Content-Type", "application/json");
    if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
        request = request.set("Authorization", &format!("Bearer {key}"));
    }

    let response = match request.send_string(&body_str) {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => return Err(status_error(endpoint, code, response)),
        Err(error) => return Err(format!("Request to {endpoint} failed: {error}")),
    };

    let response_text = response
        .into_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&response_text).map_err(|e| format!("Response was not JSON: {e}"))?;

    let choice = &json["choices"][0];
    let content = choice["message"]["content"].as_str().map(str::trim).unwrap_or("");
    if !content.is_empty() {
        return Ok(content.to_string());
    }
    // Same reasoning-model trap as the vision path: a thinking model can burn the whole budget in
    // `reasoning_content` and return empty `content` with finish_reason "length".
    let finish = choice["finish_reason"].as_str().unwrap_or("unknown");
    Err(format!(
        "The model returned an empty reply (finish_reason: {finish}). If it is a reasoning model, \
         raise the token budget or pick one that answers directly."
    ))
}

/// Reads the server's error body on a non-2xx response and folds it into a one-line message, so a
/// failure carries LM Studio's own explanation (missing token, no model loaded) instead of a code.
fn status_error(context: &str, code: u16, response: ureq::Response) -> String {
    let body = response.into_string().unwrap_or_default();
    let preview: String = body.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect();
    format!("{context} returned HTTP {code}: {preview}")
}

/// Lists the model ids the endpoint advertises. LM Studio's `/v1/models` returns every *downloaded*
/// model (not only the loaded one), which is exactly what a picker needs. `models_url` is the
/// `…/v1/models` URL; `api_key` is sent as a bearer token when the server requires one.
pub fn list_models(agent: &ureq::Agent, models_url: &str, api_key: Option<&str>) -> Result<Vec<String>, String> {
    let mut request = agent.get(models_url);
    if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
        request = request.set("Authorization", &format!("Bearer {key}"));
    }
    let text = match request.call() {
        Ok(response) => response.into_string().map_err(|e| format!("Failed to read models response: {e}"))?,
        Err(ureq::Error::Status(code, response)) => return Err(status_error(models_url, code, response)),
        Err(error) => return Err(format!("Request to {models_url} failed: {error}")),
    };
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Models response was not JSON: {e}"))?;
    let ids = json["data"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(ids)
}

/// Actively loads `model` by sending a tiny 1-token completion — LM Studio JIT-loads a cold model on
/// the first request and answers instantly if it is already warm. This is how the app *loads* the
/// picked model instead of failing the whole run against an empty server. Returns `Ok` once the
/// model responds; the read timeout is generous because a cold load can take a while.
pub fn warm_model(agent: &ureq::Agent, endpoint: &str, model: &str, api_key: Option<&str>) -> Result<(), String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": "ok" }],
        "max_tokens": 1,
        "temperature": 0.0,
        "stream": false
    });
    let body_str = serde_json::to_string(&body).map_err(|e| format!("Failed to build request: {e}"))?;

    let mut request = agent.post(endpoint).set("Content-Type", "application/json");
    if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
        request = request.set("Authorization", &format!("Bearer {key}"));
    }
    match request.send_string(&body_str) {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, response)) => Err(status_error(endpoint, code, response)),
        Err(error) => Err(format!("Request to {endpoint} failed: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-gated live integration check (needs a running endpoint): exercises the FULL app path —
    // JPEG re-encode, data URL, the real request with the raised token budget, and the new
    // empty-content diagnosis — against an actual model. Set ICAT_LIVE_ENDPOINT, ICAT_LIVE_MODEL,
    // ICAT_TEST_TITLE_IMAGE (and ICAT_LIVE_TOKEN if the server needs one) to run it; otherwise skips.
    #[test]
    fn describe_image_hits_live_endpoint() {
        let (Ok(endpoint), Ok(model), Ok(image)) = (
            std::env::var("ICAT_LIVE_ENDPOINT"),
            std::env::var("ICAT_LIVE_MODEL"),
            std::env::var("ICAT_TEST_TITLE_IMAGE"),
        ) else {
            eprintln!("skipping live describe test: ICAT_LIVE_ENDPOINT/ICAT_LIVE_MODEL/ICAT_TEST_TITLE_IMAGE not all set");
            return;
        };
        let token = std::env::var("ICAT_LIVE_TOKEN").ok();
        let agent = build_agent();
        let result = describe_image(&agent, &endpoint, &model, token.as_deref(), DESCRIBE_PROMPT, std::path::Path::new(&image));
        match result {
            Ok(text) => {
                eprintln!("LIVE DESCRIBE OK ({} chars):\n{text}", text.len());
                assert!(!text.trim().is_empty(), "description must not be empty");
            }
            Err(error) => panic!("live describe failed: {error}"),
        }
    }

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
