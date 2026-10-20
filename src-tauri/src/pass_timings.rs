//! How fast each analysis pass actually runs on this machine, so the Analysis panel can say how
//! long a run would take before it is started.
//!
//! Every finished pass appends (images, seconds) here; an estimate is the pooled rate over the last
//! few runs. Kept in the app's data folder, not beside a library: the rate is a property of the
//! machine (its GPU, its OCR engine), and the same pass over two libraries runs at the same speed.
//!
//! The defaults are what was measured on this machine before the log existed, stated so the panel
//! can say "estimated" rather than "measured" until a real run has been timed.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const TIMINGS_FILE_NAME: &str = "pass-timings.json";

/// Runs kept per pass. Enough to smooth one odd night out; few enough that a hardware or model
/// change shows within a week.
const RUNS_KEPT: usize = 12;

/// A run shorter than this says more about start-up than about the pass.
const MIN_ITEMS: usize = 3;
const MIN_SECS: f64 = 1.0;

/// Seconds per image before any run has been timed. Each from a measurement on this machine:
/// - `vision`: median 5.4 s / mean 6.0 s between consecutive description timestamps over 50,241
///   descriptions (6.8 s over the last 30 days), so 6.2.
/// - `chunk`: the title band is cropped at decode, ~44 frames/s in the real nightly run.
/// - `ocr` (extraction): ~12 images/s, 5,307 images in ~7.5 min.
/// - `text`: the same whole-image Windows OCR as extraction.
/// - `nsfw`: one NudeNet ONNX score is ~100 ms, plus the decode.
pub fn default_secs_per_item(pass: &str) -> f64 {
    match pass {
        "vision" => 6.2,
        "chunk" => 0.023,
        "ocr" | "text" => 0.085,
        "nsfw" => 0.12,
        _ => 0.1,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimedRun {
    at: String,
    items: usize,
    secs: f64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimingsFile {
    #[serde(default)]
    runs: BTreeMap<String, Vec<TimedRun>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PassRate {
    pub secs_per_item: f64,
    /// False while no real run has been timed and the figure is [`default_secs_per_item`].
    pub measured: bool,
    pub runs: usize,
    pub items: usize,
    pub last_at: Option<String>,
}

fn timings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join(TIMINGS_FILE_NAME))
}

fn load(app: &AppHandle) -> TimingsFile {
    timings_path(app)
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|data| serde_json::from_str(&data).ok())
        .unwrap_or_default()
}

/// Logs one finished (or stopped) pass. `items` is what it actually got through, so a run stopped
/// part way still contributes an honest rate.
pub fn record(app: &AppHandle, pass: &str, items: usize, elapsed: Duration) {
    let secs = elapsed.as_secs_f64();
    if items < MIN_ITEMS || secs < MIN_SECS {
        return;
    }
    let Some(path) = timings_path(app) else { return };
    let mut file = load(app);
    let runs = file.runs.entry(pass.to_string()).or_default();
    runs.push(TimedRun { at: crate::now_iso(), items, secs });
    if runs.len() > RUNS_KEPT {
        let excess = runs.len() - RUNS_KEPT;
        runs.drain(..excess);
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string_pretty(&file) {
        let _ = fs::write(path, data);
    }
}

/// The current estimate for one pass: total seconds over total images across the kept runs, so a
/// long run counts for more than a short one.
pub fn rate(app: &AppHandle, pass: &str) -> PassRate {
    rate_from(load(app).runs.get(pass).map(Vec::as_slice).unwrap_or(&[]), pass)
}

fn rate_from(runs: &[TimedRun], pass: &str) -> PassRate {
    let items: usize = runs.iter().map(|run| run.items).sum();
    let secs: f64 = runs.iter().map(|run| run.secs).sum();
    if items == 0 {
        return PassRate {
            secs_per_item: default_secs_per_item(pass),
            measured: false,
            runs: 0,
            items: 0,
            last_at: None,
        };
    }
    PassRate {
        secs_per_item: secs / items as f64,
        measured: true,
        runs: runs.len(),
        items,
        last_at: runs.last().map(|run| run.at.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_is_pooled_and_falls_back_to_the_default() {
        let fallback = rate_from(&[], "vision");
        assert!(!fallback.measured);
        assert_eq!(fallback.secs_per_item, 6.2);

        let runs = vec![
            TimedRun { at: "a".into(), items: 100, secs: 500.0 },
            TimedRun { at: "b".into(), items: 10, secs: 100.0 },
        ];
        let pooled = rate_from(&runs, "vision");
        assert!(pooled.measured);
        // 600 s over 110 images, not the mean of 5.0 and 10.0.
        assert!((pooled.secs_per_item - 600.0 / 110.0).abs() < 1e-9);
        assert_eq!(pooled.last_at.as_deref(), Some("b"));
    }
}
