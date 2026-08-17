//! Makes the nightly headless refresh visible to — and stoppable from — the GUI.
//!
//! The nightly job is a **separate process**: the Task Scheduler launches this same exe with
//! `--headless-refresh`, and `run()` then drops the main window from the config entirely. That is
//! what makes it invisible, and it is also why none of the app's normal progress plumbing reaches
//! the GUI. `app.emit("vision-analysis-progress", …)` goes to *that* process's own listeners; a
//! window open in a different process cannot hear it, and `VisionControl.cancel` is an `AtomicBool`
//! in that process's memory, which a second process cannot reach either.
//!
//! Before this module the only handle on a running job was the tray icon's "Cancel refresh" menu —
//! easy to miss, and nothing in the app itself ever said a run was happening at all. A vision pass
//! with a large backlog would sit on the GPU for hours with the GUI showing no sign of it.
//!
//! So the two processes talk through two small files in the app data directory, next to
//! `settings.json`:
//!
//! - **`auto-refresh-run.json`** — the headless run publishes what it is doing, once a second. The
//!   GUI polls it.
//! - **`auto-refresh-cancel`** — a sentinel the GUI drops to ask for a stop. The headless run's
//!   supervisor notices it and sets every pass's cancel flag.
//!
//! ## Why files, and why these two shapes
//!
//! A socket or a named pipe would need the headless side to run a server and the GUI to handle it
//! being absent, which is most of the time. The state here is a few hundred bytes, updated once a
//! second, read once every two — a file is the right size of mechanism.
//!
//! **The state file is written atomically** (temp file + rename). The GUI polls on its own clock
//! and would otherwise regularly read a half-written file: `fs::write` truncates first, so a naive
//! write has a window where the JSON on disk is invalid. `fs::rename` replaces in one step on
//! Windows, so a reader sees either the old state or the new one.
//!
//! **Cancellation is a separate file rather than a field in the state file**, because the state
//! file has one writer (the headless run) and the cancel signal has a different one (the GUI).
//! Putting both in one file would make every GUI stop a read-modify-write racing the headless
//! run's once-a-second publish. Existence-as-signal needs no read, no merge, and no lock.
//!
//! ## Staleness is not optional
//!
//! A killed run (Task Manager, a crash, a reboot mid-pass) leaves the state file behind. A banner
//! that keeps claiming a run is happening — with a Stop button that does nothing — is worse than
//! no banner, so a state is believed only while **both** hold:
//!
//! - the recorded pid is still a live process, and
//! - the heartbeat is younger than [`HEARTBEAT_STALE_SECS`].
//!
//! Either check alone has a hole. A pid is recycled by Windows, so a dead run's pid can come back
//! attached to something unrelated; and the heartbeat alone would leave a phantom banner up for its
//! full staleness window after a kill. Together they disagree only in the safe direction.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

const STATE_FILE_NAME: &str = "auto-refresh-run.json";
const STATE_TEMP_FILE_NAME: &str = "auto-refresh-run.json.tmp";
const CANCEL_FILE_NAME: &str = "auto-refresh-cancel";

/// How old a heartbeat may get before the GUI stops believing the run. The supervisor beats once a
/// second, so this is a wide margin — it exists to absorb a stalled disk or a process frozen by the
/// debugger, not to time anything.
pub const HEARTBEAT_STALE_SECS: u64 = 45;

/// Unix-epoch milliseconds. The wire format for every instant here, because the GUI compares them
/// against its own `Date.now()` and an ISO string would need parsing on both sides.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// What the headless run is doing, as the GUI needs to show it.
///
/// `Default` is what the run starts from, so a field added later reads as "not reported" rather
/// than breaking a state file written by an older build (`#[serde(default)]` on the struct).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunState {
    pub pid: u32,
    /// ISO-8601, for display only.
    pub started_at: String,
    pub started_ms: u64,
    pub heartbeat_ms: u64,
    /// Machine-readable stage: `starting`, `scanning`, `explicit`, `text`, `ocr`, `gpu-wait`,
    /// `vision`, `finishing`.
    pub phase: String,
    /// One human line for the banner, e.g. "Describing images".
    pub label: String,
    pub root: Option<String>,
    pub root_index: usize,
    pub root_total: usize,
    pub processed: usize,
    pub total: usize,
    pub current_name: Option<String>,
    /// When the GPU pass must stop, or 0 when no limit applies. Only ever set once the vision pass
    /// is actually running, so the banner's countdown measures GPU time and not the hours the GPU
    /// gate may have spent waiting for a free card.
    pub vision_deadline_ms: u64,
    pub vision_limit_minutes: u32,
    /// The run has seen the cancel sentinel and is winding down. Purely so the banner can say
    /// "Stopping…" instead of looking like the Stop button did nothing — a pass only notices a
    /// cancel between images, which for a vision pass is several seconds.
    pub cancel_requested: bool,
    /// Set by the supervisor when the run stops itself at the time limit rather than being
    /// cancelled, so the summary can tell the two apart.
    pub hit_time_limit: bool,
    /// Not persisted: the in-process flag that tells the supervisor thread to stop publishing.
    #[serde(skip)]
    pub finished: bool,
}

fn data_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok()
}

fn state_path(app: &AppHandle) -> Option<PathBuf> {
    data_dir(app).map(|dir| dir.join(STATE_FILE_NAME))
}

fn cancel_path(app: &AppHandle) -> Option<PathBuf> {
    data_dir(app).map(|dir| dir.join(CANCEL_FILE_NAME))
}

/// Publishes the current state for the GUI. Best-effort by design: a failed write costs one tick of
/// banner freshness, and a nightly job must not abort because a status file could not be written.
pub fn publish(app: &AppHandle, state: &RunState) {
    let Some(dir) = data_dir(app) else { return };
    let Ok(json) = serde_json::to_string_pretty(state) else { return };
    let _ = fs::create_dir_all(&dir);
    // Temp + rename, so a GUI poll landing mid-write reads the previous state rather than a
    // truncated file. `fs::rename` replaces the destination atomically on Windows.
    let temp = dir.join(STATE_TEMP_FILE_NAME);
    if fs::write(&temp, json).is_err() {
        return;
    }
    if fs::rename(&temp, dir.join(STATE_FILE_NAME)).is_err() {
        let _ = fs::remove_file(&temp);
    }
}

/// The raw published state, with no liveness judgement. Callers that are about to show it to
/// somebody want [`read_live`] instead.
pub fn read(app: &AppHandle) -> Option<RunState> {
    let raw = fs::read_to_string(state_path(app)?).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The state only if a run is genuinely still going, clearing the file when it is not.
///
/// Self-healing is the point of the removal: a killed run's file would otherwise sit there forever,
/// and every future read would pay the pid lookup to reach the same answer.
pub fn read_live(app: &AppHandle) -> Option<RunState> {
    let state = read(app)?;
    if is_live(&state) {
        return Some(state);
    }
    clear(app);
    None
}

/// Whether a published state describes a run that is still happening. See the module docs for why
/// this needs both halves.
pub fn is_live(state: &RunState) -> bool {
    if state.finished || state.pid == 0 {
        return false;
    }
    let age_ms = now_ms().saturating_sub(state.heartbeat_ms);
    age_ms <= HEARTBEAT_STALE_SECS * 1000 && pid_alive(state.pid)
}

pub fn clear(app: &AppHandle) {
    if let Some(path) = state_path(app) {
        let _ = fs::remove_file(path);
    }
}

/// Asks a running headless refresh to stop. Writing the file *is* the request — the headless
/// supervisor polls for it once a second.
pub fn request_cancel(app: &AppHandle) -> Result<(), String> {
    let dir = data_dir(app).ok_or_else(|| "Failed to resolve the app data directory.".to_string())?;
    fs::create_dir_all(&dir).map_err(|error| format!("Failed to create the app data directory: {error}"))?;
    fs::write(dir.join(CANCEL_FILE_NAME), now_ms().to_string())
        .map_err(|error| format!("Failed to write the stop request: {error}"))
}

pub fn cancel_requested(app: &AppHandle) -> bool {
    cancel_path(app).map(|path| path.exists()).unwrap_or(false)
}

/// Removes a leftover sentinel. Called at the start of every run: a stop requested while nothing
/// was running (or one written just as the last run exited) must not cancel the *next* night's job.
pub fn clear_cancel(app: &AppHandle) {
    if let Some(path) = cancel_path(app) {
        let _ = fs::remove_file(path);
    }
}

/// `STILL_ACTIVE`. Spelled out rather than imported because the `windows` crate types it as an
/// `NTSTATUS` while `GetExitCodeProcess` hands back a plain `u32`.
const STILL_ACTIVE_CODE: u32 = 259;

/// Whether a pid belongs to a process that is actually running.
///
/// `OpenProcess` succeeding proves nothing: a process that has exited stays openable for as long as
/// anything holds a handle to it, and it answers `GetExitCodeProcess` with its real exit code. Only
/// `STILL_ACTIVE` means running.
///
/// The known hole — a process that genuinely exits with code 259 reads as alive — is closed by the
/// heartbeat check in [`is_live`], which is why that one requires both.
fn pid_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut exit_code: u32 = 0;
        let queried = GetExitCodeProcess(handle, &mut exit_code).is_ok();
        let _ = CloseHandle(handle);
        queried && exit_code == STILL_ACTIVE_CODE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_state() -> RunState {
        RunState {
            pid: std::process::id(),
            heartbeat_ms: now_ms(),
            ..RunState::default()
        }
    }

    #[test]
    fn a_fresh_state_for_this_process_is_live() {
        assert!(is_live(&running_state()));
    }

    #[test]
    fn a_stale_heartbeat_is_not_live() {
        let mut state = running_state();
        state.heartbeat_ms = now_ms() - (HEARTBEAT_STALE_SECS + 5) * 1000;
        assert!(!is_live(&state));
    }

    #[test]
    fn a_dead_pid_is_not_live() {
        let mut state = running_state();
        // Pid 0 is never a real user process, and the guard rejects it before any syscall.
        state.pid = 0;
        assert!(!is_live(&state));
    }

    #[test]
    fn a_finished_run_is_not_live_even_with_a_fresh_heartbeat() {
        let mut state = running_state();
        state.finished = true;
        assert!(!is_live(&state));
    }

    /// The whole point of the pid half: this process is alive, so the heartbeat is what must decide.
    #[test]
    fn this_process_is_alive() {
        assert!(pid_alive(std::process::id()));
    }
}
