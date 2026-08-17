//! Read `screenshot-tool`'s capture-activity log — how many screenshots were *taken*.
//!
//! **This library is what survived; that log is the act.** Everything else in this app counts
//! files on disk, which is the right answer to "what can I categorize" and the wrong answer to
//! "how much was I capturing". A month cleared out of the save folder is gone from every figure
//! this app computes, and it should not be gone from a metric about the user's own habit.
//!
//! So the two are shown side by side and are **never reconciled**. A gap between them is the
//! normal result of deleting, moving or archiving pictures, plus the handful of captures whose
//! PNG never landed (`ok = 0` — the shot was still taken). It is not missing data, it is not a
//! scan that failed, and it must never be presented as either.
//!
//! STRICTLY READ-ONLY with respect to that tool. Nothing here creates, appends to, renames or
//! deletes one of its files; screenshot-tool is the sole writer.
//!
//! ## Where it is
//!
//! ```text
//! %USERPROFILE%\.screenshot-tool\          (override: SCREENSHOT_TOOL_DIR)
//!   config.json                  the tool's settings — `stats` (the switch) and `save_folder`
//!   stats\shots-YYYY-MM.v1.tsv   ts  mode  ok  file
//!   stats\uptime-YYYY-MM.v1.tsv  ts  event  pid  interval_s
//! ```
//!
//! ## ⚠ Absence is not zero, and it has three causes
//!
//! The tool is a tray app that records only while it is running, so a quiet span may be a span
//! with no tool. It heartbeats every `interval_s`, which is what makes uptime knowable — hence
//! [`Coverage`], and hence the rule that a count is never displayed without it:
//!
//! | What you have | What it means |
//! |---|---|
//! | coverage high, 0 shots | a real, measured zero |
//! | coverage 0, `conclusive` | the tool was closed — **not recorded**, say nothing about the user |
//! | `!conclusive` | window too short, or the tool just started — 0 is not evidence |
//! | [`Blocked::LoggingDisabled`] | the user switched the log off — a **setting**, not an outage |
//!
//! That last one is the one that reads wrong most easily: `stats: false` leaves the tool
//! capturing perfectly and simply not writing rows. Reporting it as a stopped app would be
//! stating something false about software that is running.
//!
//! ## ⚠ Schema is pinned in the filename
//!
//! `shots-YYYY-MM.v1.tsv` — the `v1` means the columns above. A file at another version is
//! **reported and skipped**, never parsed positionally: a half-parsed file reads exactly like a
//! quiet month. Columns added to the right of a `v1` file are safe and are ignored here; anything
//! that changes what an existing column *means* is a `v2` filename.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The schema this reader understands. Part of the filename by design.
pub const SCHEMA: &str = "v1";

/// Heartbeat gaps up to `interval_s * this` still count as continuous uptime. The heartbeat is a
/// `tkinter.after` on the tool's UI thread, so a busy moment can push one late without the tool
/// having stopped.
pub const HEARTBEAT_GRACE: i64 = 2;

/// Why a read produced nothing. Four different sentences, and collapsing any two of them into
/// "no data" is how this store reports something false.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Blocked {
    /// The tool has never run on this machine.
    NotInstalled,
    /// The user turned capture logging off. **A setting, not an outage** — the tool may be
    /// running and capturing right now.
    LoggingDisabled,
    /// Installed, logging on, but nothing has been written yet (a build older than the log, or a
    /// tool that has not been started since).
    NeverLogged,
}

/// The tool's own settings, for the two facts a consumer needs.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolSettings {
    pub present: bool,
    /// The switch. `None` when there is no config to read.
    pub logging: Option<bool>,
    /// Where the PNGs actually are. Read, never assumed — the default in the tool's own
    /// `config.py` is *not* what is in use on this machine.
    pub save_folder: Option<String>,
}

/// One capture. `ok = false` is a shot whose PNG never landed — the act happened, the file does
/// not exist, and this is the row the save folder will never agree with.
#[derive(Debug, Clone)]
pub struct Shot {
    /// Local civil date, `YYYY-MM-DD`, from the row's own timestamp text.
    pub day: String,
    /// Local hour, 0-23, likewise.
    pub hour: u8,
    pub epoch_ms: i64,
    pub mode: String,
    pub ok: bool,
}

/// One uptime row: `start` | `alive` | `stop`.
#[derive(Debug, Clone)]
struct Beat {
    epoch_ms: i64,
    event: String,
    pid: Option<i64>,
    interval_s: i64,
}

/// How much of a span the tool is known to have been running for.
///
/// **A floor, deliberately.** The tail of a live run is missing up to one heartbeat interval,
/// because the only honest claim about the moment after the last row is that nothing was
/// recorded. `running` is how "it is up right now" gets stated instead of by extrapolating.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Coverage {
    pub observed_min: i64,
    pub window_min: i64,
    pub coverage_pct: i64,
    /// `false` means a zero here is **not evidence** the tool was shut, and no caller may print
    /// "not recorded" on it.
    pub conclusive: bool,
    /// The tool heartbeat within the grace window, so it is up right now.
    pub running: bool,
    pub heartbeat_s: i64,
    /// True when the requested window reaches back further than the log itself.
    ///
    /// Time before the first row is **not applicable**, not unmonitored: there was nothing
    /// recording and nothing to record. Counting it as a gap would make a freshly-installed log
    /// read as 0% coverage forever, and a permanent alarm is one the eye learns to skip.
    pub truncated_to_log: bool,
    /// The instant coverage is actually measured from, once truncated.
    pub covers_from: Option<String>,
}

/// Everything the UI needs to draw the capture-activity readout.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CaptureActivity {
    pub blocked: Option<Blocked>,
    pub settings: ToolSettings,
    /// `YYYY-MM-DD` -> captures taken that day, over the requested window.
    pub by_day: BTreeMap<String, u32>,
    /// 24 buckets by local hour of day.
    pub by_hour: Vec<u32>,
    /// Capture mode -> count.
    pub by_mode: BTreeMap<String, u32>,
    pub total: u32,
    /// Captures with no file on disk. Kept apart from `total` so the difference against the
    /// library is explainable rather than alarming.
    pub failed: u32,
    pub active_days: u32,
    /// Median over **active days only**. Silent days are mostly days the tool was shut, and
    /// mixing them in drags the baseline toward a number the user's behaviour never produced.
    pub median_per_active_day: Option<u32>,
    pub peak_hour: Option<u8>,
    pub first: Option<String>,
    pub last: Option<String>,
    /// Every capture ever recorded, across all months — exact, so nobody sums windows.
    pub total_on_record: u32,
    pub coverage: Coverage,
    pub days: u32,
    /// Files at a schema version this reader does not understand. Loud on purpose: a silently
    /// halved data set reads exactly like a quiet month.
    pub unknown_schema: Vec<String>,
    pub malformed_rows: u32,
}

fn data_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("SCREENSHOT_TOOL_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    Some(Path::new(&home).join(".screenshot-tool"))
}

fn stats_dir(dir: &Path) -> PathBuf {
    dir.join("stats")
}

/// The tool's settings. An absent `stats` key means the tool predates the switch, whose default
/// is on — so it is read as on rather than as unknown.
fn read_settings(dir: &Path) -> ToolSettings {
    let Ok(raw) = std::fs::read_to_string(dir.join("config.json")) else {
        return ToolSettings::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return ToolSettings { present: true, logging: None, save_folder: None };
    };
    ToolSettings {
        present: true,
        logging: Some(value.get("stats").and_then(|v| v.as_bool()).unwrap_or(true)),
        save_folder: value
            .get("save_folder")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
    }
}

/// Monthly files of one kind at the schema this reader understands, oldest first.
fn log_files(dir: &Path, kind: &str) -> Vec<PathBuf> {
    let suffix = format!(".{SCHEMA}.tsv");
    let prefix = format!("{kind}-");
    let mut out: Vec<PathBuf> = std::fs::read_dir(stats_dir(dir))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(&suffix))
        })
        .collect();
    out.sort();
    out
}

/// Files this reader will not parse. Surfaced rather than skipped in silence.
fn unknown_schema_files(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(stats_dir(dir))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            (n.starts_with("shots-") || n.starts_with("uptime-"))
                && n.ends_with(".tsv")
                && !n.ends_with(&format!(".{SCHEMA}.tsv"))
        })
        .collect();
    out.sort();
    out
}

/// Parse `2026-08-16T14:03:21.482+03:00` into (day, hour, epoch-ms).
///
/// The day and hour come from the **text**, not from a conversion: they are the user's own local
/// clock, the same clock the PNG filenames carry, and a shot taken at 09:00 must not become 06:00
/// because something downstream normalised it. The epoch is parsed separately and only for
/// windowing arithmetic.
fn parse_ts(ts: &str) -> Option<(String, u8, i64)> {
    let bytes = ts.as_bytes();
    if bytes.len() < 19 || bytes[10] != b'T' {
        return None;
    }
    let day = ts.get(0..10)?.to_owned();
    if !day.as_bytes().iter().enumerate().all(|(i, c)| {
        if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() }
    }) {
        return None;
    }
    let hour: u8 = ts.get(11..13)?.parse().ok()?;
    if hour > 23 {
        return None;
    }
    let epoch_ms = epoch_ms(ts)?;
    Some((day, hour, epoch_ms))
}

/// Milliseconds since the epoch for an ISO local-with-offset timestamp.
///
/// Hand-rolled rather than pulled from a date crate: this is the only place in the app that needs
/// it, the input shape is fixed by a format this project controls the contract for, and the
/// alternative is a dependency for one function.
fn epoch_ms(ts: &str) -> Option<i64> {
    let y: i64 = ts.get(0..4)?.parse().ok()?;
    let mo: i64 = ts.get(5..7)?.parse().ok()?;
    let d: i64 = ts.get(8..10)?.parse().ok()?;
    let h: i64 = ts.get(11..13)?.parse().ok()?;
    let mi: i64 = ts.get(14..16)?.parse().ok()?;
    let s: i64 = ts.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }

    // Days from the civil epoch — Howard Hinnant's algorithm, valid for any proleptic Gregorian
    // date and free of the leap-year edge cases a naive version gets wrong.
    let y_adj = if mo <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    let mut ms = ((days * 86_400) + h * 3600 + mi * 60 + s) * 1000;

    // Fractional seconds, if present.
    let rest = ts.get(19..).unwrap_or("");
    let rest = if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(v) = format!("{:0<3}", &digits[..digits.len().min(3)]).parse::<i64>() {
            ms += v;
        }
        &rest[1 + digits.len()..]
    } else {
        rest
    };

    // The offset makes this an instant. Without subtracting it, two machines in different zones
    // would place the same row at different moments.
    if let Some(sign) = rest.chars().next() {
        if sign == '+' || sign == '-' {
            let oh: i64 = rest.get(1..3)?.parse().ok()?;
            let om: i64 = rest.get(4..6).unwrap_or("00").parse().unwrap_or(0);
            let offset = (oh * 60 + om) * 60_000;
            ms += if sign == '+' { -offset } else { offset };
        }
    }
    Some(ms)
}

/// Read one kind of log, calling `visit` per valid row with the header-mapped fields.
///
/// The header is read from each file rather than assumed, so a column added to the right of an
/// existing `v1` file costs nothing here. Ragged rows are counted and skipped rather than parsed
/// positionally.
fn read_rows(dir: &Path, kind: &str, mut visit: impl FnMut(&[&str], &[&str])) -> u32 {
    let mut malformed = 0u32;
    for path in log_files(dir, kind) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let mut cols: Option<Vec<&str>> = None;
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with("ts\t") {
                cols = Some(line.split('\t').collect());
                continue;
            }
            let Some(ref header) = cols else {
                malformed += 1;
                continue;
            };
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < header.len() {
                malformed += 1;
                continue;
            }
            visit(header, &fields);
        }
    }
    malformed
}

fn field<'a>(header: &[&str], fields: &'a [&'a str], name: &str) -> Option<&'a str> {
    header.iter().position(|c| *c == name).map(|i| fields[i])
}

fn read_shots(dir: &Path, since_ms: i64) -> (Vec<Shot>, u32, u32) {
    let mut shots = Vec::new();
    let mut total_on_record = 0u32;
    let malformed = read_rows(dir, "shots", |header, fields| {
        total_on_record += 1;
        let Some((day, hour, epoch_ms)) = parse_ts(fields[0]) else { return };
        if epoch_ms < since_ms {
            return;
        }
        shots.push(Shot {
            day,
            hour,
            epoch_ms,
            mode: field(header, fields, "mode").unwrap_or("unknown").to_owned(),
            // Anything other than a literal "0" is a landed file; a torn value should not
            // silently invent a failure.
            ok: field(header, fields, "ok") != Some("0"),
        });
    });
    shots.sort_by_key(|s| s.epoch_ms);
    (shots, total_on_record, malformed)
}

fn read_uptime(dir: &Path, since_ms: i64) -> (Vec<Beat>, u32) {
    let mut beats = Vec::new();
    let malformed = read_rows(dir, "uptime", |header, fields| {
        let Some(epoch_ms) = epoch_ms(fields[0]) else { return };
        if epoch_ms < since_ms {
            return;
        }
        beats.push(Beat {
            epoch_ms,
            event: field(header, fields, "event").unwrap_or("alive").to_owned(),
            pid: field(header, fields, "pid").and_then(|v| v.parse().ok()),
            interval_s: field(header, fields, "interval_s")
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
        });
    });
    beats.sort_by_key(|b| b.epoch_ms);
    (beats, malformed)
}

/// Contiguous spans the tool is known to have been running.
///
/// A span extends from one row to the next while they share a pid and the gap is within
/// `interval_s * HEARTBEAT_GRACE`. A `stop` ends one at its own instant; a new pid, or a gap past
/// the grace, ends the previous span **at its last known row** — a process that was killed said
/// nothing more, and inventing time after its final heartbeat would report uptime that may not
/// have happened.
fn spans(beats: &[Beat]) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    let mut cur: Option<(i64, i64, Option<i64>)> = None;

    for b in beats {
        let grace = b.interval_s * HEARTBEAT_GRACE * 1000;
        let continues = match cur {
            Some((_, end, pid)) => {
                b.event != "start"
                    && (b.pid.is_none() || pid.is_none() || b.pid == pid)
                    && b.epoch_ms - end <= grace
            }
            None => false,
        };

        if continues {
            if let Some((start, _, pid)) = cur {
                if b.event == "stop" {
                    out.push((start, b.epoch_ms));
                    cur = None;
                } else {
                    cur = Some((start, b.epoch_ms, pid));
                }
            }
            continue;
        }
        if let Some((start, end, _)) = cur {
            out.push((start, end));
        }
        // An `alive` with no open span is a normal way in: the window under examination began in
        // the middle of a run that started before it.
        cur = if b.event == "stop" { None } else { Some((b.epoch_ms, b.epoch_ms, b.pid)) };
    }
    if let Some((start, end, _)) = cur {
        out.push((start, end));
    }
    out
}

/// The first uptime row ever written, across every month file.
///
/// Reads only until the first data line of the oldest file — this exists to clamp a window, not to
/// build rows, and the caller already pays for a full parse of the recent months.
fn log_begins_at(dir: &Path) -> Option<i64> {
    for path in log_files(dir, "uptime") {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') || line.starts_with("ts\t") {
                continue;
            }
            if let Some(ms) = line.split('\t').next().and_then(epoch_ms) {
                return Some(ms);
            }
        }
    }
    None
}

fn coverage(beats: &[Beat], from: i64, to: i64, now: i64) -> Coverage {
    coverage_from(beats, from, to, now, None)
}

/// `begins_at` clamps the window to when the log started. Without it, asking for 30 days of a
/// log that is two hours old reports 0% coverage — true against the question asked, and a
/// completely misleading answer to the question meant.
fn coverage_from(
    beats: &[Beat],
    requested_from: i64,
    to: i64,
    now: i64,
    begins_at: Option<i64>,
) -> Coverage {
    let truncated = begins_at.is_some_and(|b| b > requested_from);
    let from = if truncated { begins_at.unwrap() } else { requested_from };
    let window_ms = (to - from).max(0);
    let interval_ms = beats.last().map_or(300, |b| b.interval_s) * 1000;
    let newest = beats.last().map(|b| b.epoch_ms);
    let running = newest.is_some_and(|n| now - n <= interval_ms * HEARTBEAT_GRACE);

    let observed: i64 = spans(beats)
        .into_iter()
        .map(|(s, e)| (e.min(to) - s.max(from)).max(0))
        .sum();

    // Three ways a zero is not evidence: the tool is up right now and predates its first
    // heartbeat in this window; the window is shorter than the heartbeat; or there are no uptime
    // rows at all, in which case shots may still be present and are still true.
    let conclusive = observed > 0
        || !(running || window_ms < interval_ms * HEARTBEAT_GRACE || beats.is_empty());

    Coverage {
        observed_min: observed / 60_000,
        window_min: window_ms / 60_000,
        coverage_pct: if window_ms > 0 { observed * 100 / window_ms } else { 0 },
        conclusive,
        running,
        heartbeat_s: interval_ms / 1000,
        truncated_to_log: truncated,
        covers_from: if truncated { Some(iso_day(from)) } else { None },
    }
}

/// `YYYY-MM-DD` for an instant, in UTC. Only ever used to label a truncated window, where a day's
/// precision is the whole claim being made.
fn iso_day(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn median_active(by_day: &BTreeMap<String, u32>) -> Option<u32> {
    let mut counts: Vec<u32> = by_day.values().copied().filter(|n| *n > 0).collect();
    if counts.is_empty() {
        return None;
    }
    counts.sort_unstable();
    let mid = counts.len() / 2;
    Some(if counts.len() % 2 == 1 {
        counts[mid]
    } else {
        (counts[mid - 1] + counts[mid]).div_ceil(2)
    })
}

/// Read the capture log over the last `days`.
///
/// `now_ms` is a parameter rather than a call to the clock so the whole reader is testable
/// without waiting for a calendar.
pub fn read(days: u32, now_ms: i64) -> CaptureActivity {
    let Some(dir) = data_dir() else {
        return CaptureActivity { blocked: Some(Blocked::NotInstalled), ..Default::default() };
    };
    let settings = read_settings(&dir);

    if !dir.exists() {
        return CaptureActivity {
            blocked: Some(Blocked::NotInstalled),
            settings,
            ..Default::default()
        };
    }
    // Checked before "never logged": a switched-off log looks identical to an absent one on
    // disk, and calling the user's own setting an outage is the misreading with the worst
    // consequences here.
    if settings.logging == Some(false) {
        return CaptureActivity {
            blocked: Some(Blocked::LoggingDisabled),
            settings,
            ..Default::default()
        };
    }
    if log_files(&dir, "shots").is_empty() && log_files(&dir, "uptime").is_empty() {
        return CaptureActivity {
            blocked: Some(Blocked::NeverLogged),
            settings,
            ..Default::default()
        };
    }

    let since_ms = now_ms - i64::from(days) * 86_400_000;
    let (shots, total_on_record, shot_bad) = read_shots(&dir, since_ms);
    let (beats, beat_bad) = read_uptime(&dir, since_ms);

    let mut by_day: BTreeMap<String, u32> = BTreeMap::new();
    let mut by_mode: BTreeMap<String, u32> = BTreeMap::new();
    let mut by_hour = vec![0u32; 24];
    let mut failed = 0u32;

    for s in &shots {
        *by_day.entry(s.day.clone()).or_default() += 1;
        *by_mode.entry(s.mode.clone()).or_default() += 1;
        by_hour[s.hour as usize] += 1;
        if !s.ok {
            failed += 1;
        }
    }

    let peak_hour = if shots.is_empty() {
        None
    } else {
        by_hour
            .iter()
            .enumerate()
            .max_by_key(|(_, n)| **n)
            .map(|(i, _)| i as u8)
    };

    CaptureActivity {
        blocked: None,
        settings,
        total: shots.len() as u32,
        failed,
        active_days: by_day.len() as u32,
        median_per_active_day: median_active(&by_day),
        peak_hour,
        first: shots.first().map(|s| format!("{} {:02}:00", s.day, s.hour)),
        last: shots.last().map(|s| format!("{} {:02}:00", s.day, s.hour)),
        by_day,
        by_hour,
        by_mode,
        total_on_record,
        coverage: coverage_from(&beats, since_ms, now_ms, now_ms, log_begins_at(&dir)),
        days,
        unknown_schema: unknown_schema_files(&dir),
        malformed_rows: shot_bad + beat_bad,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_handles_offsets_and_fractions() {
        // The same instant written three ways must land on the same millisecond, or a row from a
        // summer month and one from a winter month are silently an hour apart.
        let utc = epoch_ms("2026-08-16T12:00:00.000+00:00").unwrap();
        assert_eq!(epoch_ms("2026-08-16T15:00:00.000+03:00").unwrap(), utc);
        assert_eq!(epoch_ms("2026-08-16T09:00:00.000-03:00").unwrap(), utc);
        assert_eq!(epoch_ms("2026-08-16T12:00:00.250+00:00").unwrap(), utc + 250);
    }

    #[test]
    fn epoch_survives_leap_years_and_century_rules() {
        let a = epoch_ms("2024-02-29T00:00:00.000+00:00").unwrap();
        let b = epoch_ms("2024-03-01T00:00:00.000+00:00").unwrap();
        assert_eq!(b - a, 86_400_000);
        // 2000 was a leap year, 1900 was not — the rule a naive implementation gets wrong.
        let c = epoch_ms("2000-02-29T00:00:00.000+00:00").unwrap();
        let d = epoch_ms("2000-03-01T00:00:00.000+00:00").unwrap();
        assert_eq!(d - c, 86_400_000);
    }

    #[test]
    fn day_and_hour_come_from_the_text_not_a_conversion() {
        // 09:00 in a +03:00 day is 06:00 UTC. The claim is about the user's morning.
        let (day, hour, _) = parse_ts("2026-08-16T09:30:00.000+03:00").unwrap();
        assert_eq!(day, "2026-08-16");
        assert_eq!(hour, 9);
    }

    #[test]
    fn malformed_timestamps_are_rejected_not_guessed() {
        assert!(parse_ts("not-a-date").is_none());
        assert!(parse_ts("2026-08-16 09:30:00").is_none());
        assert!(parse_ts("2026-08-16T99:30:00.000+03:00").is_none());
    }

    fn beat(ms: i64, event: &str, pid: i64) -> Beat {
        Beat { epoch_ms: ms, event: event.to_owned(), pid: Some(pid), interval_s: 300 }
    }

    const MIN: i64 = 60_000;

    #[test]
    fn a_killed_run_ends_at_its_last_heartbeat() {
        // Not at the moment the next process appeared — that 55-minute hole is time the tool was
        // provably not recording, and counting it would be uptime that never happened.
        let beats = vec![
            beat(0, "start", 100),
            beat(5 * MIN, "alive", 100),
            beat(60 * MIN, "alive", 200),
            beat(65 * MIN, "alive", 200),
        ];
        let s = spans(&beats);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0], (0, 5 * MIN));
        assert_eq!(s[1], (60 * MIN, 65 * MIN));
    }

    #[test]
    fn a_new_pid_ends_the_previous_span_even_inside_the_grace() {
        let beats = vec![
            beat(0, "start", 100),
            beat(5 * MIN, "alive", 100),
            beat(6 * MIN, "start", 300),
            beat(11 * MIN, "alive", 300),
        ];
        assert_eq!(spans(&beats).len(), 2);
    }

    #[test]
    fn a_stop_closes_the_span_at_its_own_instant() {
        let beats = vec![beat(0, "start", 100), beat(2 * MIN, "stop", 100)];
        assert_eq!(spans(&beats), vec![(0, 2 * MIN)]);
    }

    #[test]
    fn an_alive_with_no_start_opens_a_span() {
        // A window can legitimately begin in the middle of a run.
        let beats = vec![beat(0, "alive", 100), beat(5 * MIN, "alive", 100)];
        assert_eq!(spans(&beats), vec![(0, 5 * MIN)]);
    }

    #[test]
    fn a_closed_tool_gives_a_conclusive_zero() {
        let now = 120 * MIN;
        let beats = vec![beat(0, "start", 100), beat(5 * MIN, "stop", 100)];
        let c = coverage(&beats, now - 60 * MIN, now, now);
        assert_eq!(c.observed_min, 0);
        assert!(!c.running);
        // The point: this zero IS evidence, so a caller may say "not recorded".
        assert!(c.conclusive);
    }

    #[test]
    fn a_running_tool_makes_a_zero_inconclusive() {
        let now = 120 * MIN;
        // Heartbeat two minutes ago: up right now, just started after the window opened. Saying
        // "not recorded" for this window would be false.
        let beats = vec![beat(118 * MIN, "start", 100)];
        let c = coverage(&beats, now - 60 * MIN, now, now);
        assert_eq!(c.observed_min, 0);
        assert!(c.running);
        assert!(!c.conclusive);
    }

    #[test]
    fn no_uptime_rows_at_all_is_inconclusive() {
        // An older tool build wrote shots but no uptime. The shots are still true; the coverage
        // is simply unknown, which is not the same as zero.
        let c = coverage(&[], 0, 60 * MIN, 60 * MIN);
        assert!(!c.conclusive);
    }

    #[test]
    fn coverage_counts_only_the_part_inside_the_window() {
        let now = 120 * MIN;
        let beats = vec![
            beat(60 * MIN, "start", 100),
            beat(65 * MIN, "alive", 100),
            beat(70 * MIN, "alive", 100),
        ];
        // Span is 60..70; window is 60..120. Ten minutes of a sixty-minute window.
        let c = coverage(&beats, now - 60 * MIN, now, now);
        assert_eq!(c.observed_min, 10);
        assert_eq!(c.coverage_pct, 16);
    }

    #[test]
    fn the_heartbeat_interval_comes_from_the_rows() {
        // A four-minute gap is continuous at 300 s and a break at 60 s. A cadence change must not
        // silently rescale history, so the row decides, never a constant here.
        let fast = |ms: i64, e: &str| Beat {
            epoch_ms: ms, event: e.to_owned(), pid: Some(1), interval_s: 60,
        };
        let beats = vec![fast(0, "start"), fast(MIN, "alive"), fast(5 * MIN, "alive")];
        assert_eq!(spans(&beats).len(), 2);
    }

    #[test]
    fn a_window_older_than_the_log_is_clamped_to_the_log() {
        // The bug this fixes: a log two hours old, asked for 30 days, reported 0% coverage and
        // fired the "was open for 0% of this window" caveat permanently. Time before the first
        // row is not a gap in the record — there was no record.
        let now = 30 * 24 * 60 * MIN;
        let begins = now - 2 * 60 * MIN;
        // A real run: heartbeats all the way from `begins` to five minutes ago.
        let mut beats = vec![beat(begins, "start", 100)];
        let mut t = begins + 5 * MIN;
        while t <= now - 5 * MIN {
            beats.push(beat(t, "alive", 100));
            t += 5 * MIN;
        }
        let c = coverage_from(&beats, now - 30 * 24 * 60 * MIN, now, now, Some(begins));

        assert!(c.truncated_to_log);
        assert_eq!(c.window_min, 120);
        // Nearly the whole life of the log, rather than a rounding error against 30 days.
        assert!(c.coverage_pct > 90, "expected >90%, got {}", c.coverage_pct);
        assert!(c.covers_from.is_some());
    }

    #[test]
    fn a_window_inside_the_log_is_not_clamped() {
        let now = 30 * 24 * 60 * MIN;
        let begins = now - 20 * 24 * 60 * MIN;
        let beats = vec![beat(now - 60 * MIN, "start", 100), beat(now - 5 * MIN, "alive", 100)];
        let c = coverage_from(&beats, now - 60 * MIN, now, now, Some(begins));
        assert!(!c.truncated_to_log);
        assert_eq!(c.window_min, 60);
        assert_eq!(c.covers_from, None);
    }

    #[test]
    fn iso_day_is_right_across_a_month_and_a_leap_boundary() {
        assert_eq!(iso_day(epoch_ms("2026-08-16T12:00:00.000+00:00").unwrap()), "2026-08-16");
        assert_eq!(iso_day(epoch_ms("2024-02-29T23:59:59.000+00:00").unwrap()), "2024-02-29");
        assert_eq!(iso_day(epoch_ms("2026-01-01T00:00:00.000+00:00").unwrap()), "2026-01-01");
    }

    #[test]
    fn blocked_reasons_serialize_to_the_strings_the_ui_switches_on() {
        // renderer.js `captureBlockedText` matches these literals. A rename here would fall
        // through its default and silently say nothing — which for `logging_disabled` means the
        // one message that stops a user's own setting reading as a broken app disappears.
        let s = |b: Blocked| serde_json::to_string(&b).unwrap();
        assert_eq!(s(Blocked::NotInstalled), "\"not_installed\"");
        assert_eq!(s(Blocked::LoggingDisabled), "\"logging_disabled\"");
        assert_eq!(s(Blocked::NeverLogged), "\"never_logged\"");
    }

    #[test]
    fn the_baseline_is_a_median_over_active_days_only() {
        let mut d = BTreeMap::new();
        d.insert("a".into(), 10);
        d.insert("b".into(), 20);
        d.insert("c".into(), 30);
        assert_eq!(median_active(&d), Some(20));
        assert_eq!(median_active(&BTreeMap::new()), None);
    }
}
