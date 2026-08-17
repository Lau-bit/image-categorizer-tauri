//! Waits for the GPU to be free before the nightly job starts a local-LLM pass.
//!
//! The vision-description pass is the only part of the refresh that runs on the GPU, and it is the
//! one part that can ruin somebody's evening: a 26B vision model saturates the card for as long as
//! the backlog lasts. The scheduled task deliberately does **not** wake the machine
//! (`WakeToRun=false`) and *does* catch up a missed run (`StartWhenAvailable=true`), so the nightly
//! pass frequently fires not at 04:00 but at the moment the user sits down and starts using the
//! machine — which is exactly when it must not grab the GPU.
//!
//! So this module answers one question, repeatedly: *is somebody else using the card right now?*
//!
//! **It reads aggregate GPU state, not per-process state, and that is forced.** `nvidia-smi
//! --query-compute-apps` looks like the right tool and is useless here: on a consumer WDDM driver it
//! enumerates ~50 processes that merely hold a GPU context (explorer.exe, the Start menu, every
//! WebView2) and reports `[N/A]` for every single `used_gpu_memory`. Measured on this box
//! 2026-08-16. There is no per-process VRAM number to read, so "who is using the GPU" cannot be
//! answered directly — only "how busy is the GPU", which `--query-gpu` does report correctly.
//!
//! ## Why this cannot deadlock against its own pass
//!
//! Two rules, and both matter:
//!
//! 1. **The gate runs once, before the pass — never between images.** Once the vision pass is
//!    running, *this app* is the thing pinning the card at 100%, so a mid-pass check would see a
//!    busy GPU forever and stall a job that is working perfectly.
//! 2. **The VRAM requirement is waived when the model is already resident.** LM Studio holding
//!    `gemma-4-26b-a4b` leaves ~12 GB free on a 32 GB card, which is less than the ~20 GB the model
//!    needs to *load*. Demanding load-sized headroom unconditionally would block precisely when the
//!    model is already up and ready. The caller passes `needs_vram: false` in that case.
//!
//! A machine with no `nvidia-smi` (AMD-only, or a broken driver install) is not an error: the gate
//! reports [`GateOutcome::Proceed`] and the pass runs as it did before this module existed. Refusing
//! to run the nightly job because a diagnostic tool is missing would be the worse failure.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::CREATE_NO_WINDOW;

/// At or above this utilization the card counts as in use. The idle Windows desktop on this machine
/// reads 5-10% (compositor + a few WebView2s); a game or a render job sits near 100%. 25% clears the
/// idle noise by a wide margin without needing the GPU to be perfectly quiet.
const BUSY_UTILIZATION_PCT: u32 = 25;

/// How many consecutive calm samples release the gate. One dip proves nothing — a game sits at 0%
/// during a loading screen, and a video call idles between frames — so the gate wants the quiet to
/// last. Three samples at 30 s is ~1 minute of sustained calm.
const CALM_SAMPLES_REQUIRED: u32 = 3;

/// Gap between samples. Long enough that polling costs nothing, short enough that the job starts
/// promptly once a game exits.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(30);

/// Cancellation granularity. The gate can sit for hours, and the tray's "Cancel refresh" must not
/// appear frozen, so the long waits are slept in slices this size.
const CANCEL_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// How long the gate is willing to wait before giving up and skipping the pass. The scheduled task
/// sets `ExecutionTimeLimit=PT0S` (no limit), so nothing else would ever stop it — an all-day gaming
/// session would otherwise leave a headless process polling until the machine sleeps. Skipping is
/// the right answer at that point: the next night's run picks the backlog up.
const MAX_WAIT: Duration = Duration::from_secs(4 * 60 * 60);

/// Free VRAM required before *loading* the model. Measured 2026-08-12 on the 32 GB RTX 5090:
/// `google/gemma-4-26b-a4b` at 65,536 context occupies 20,076 MiB. A load that does not fit spills
/// into system RAM and runs roughly 10x slower — the documented failure that made a whole benchmark
/// suite wrong — so starting one without the headroom is worse than waiting for it.
const REQUIRED_FREE_MIB: u64 = 20_480;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuSample {
    pub utilization_pct: u32,
    pub free_mib: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    /// The GPU is free (or unreadable, or the gate is switched off) — run the pass.
    Proceed,
    /// The user cancelled from the tray while the gate was waiting.
    Cancelled,
    /// The GPU stayed busy for [`MAX_WAIT`]; skip the pass this run.
    TimedOut,
}

/// Whether one reading counts as calm. Split out from the polling so the thresholds are testable
/// without a GPU.
fn sample_is_calm(sample: &GpuSample, needs_vram: bool) -> bool {
    if sample.utilization_pct >= BUSY_UTILIZATION_PCT {
        return false;
    }
    // Only a cold model needs room to land. See the module docs: applying this to an already-loaded
    // model is the deadlock.
    !needs_vram || sample.free_mib >= REQUIRED_FREE_MIB
}

/// Parses `nvidia-smi --format=csv,noheader,nounits` output for `utilization.gpu,memory.free`.
/// Multi-GPU boxes print one row per card; the first row is the one `--id=0` would report, which on
/// this machine is the discrete RTX 5090 (the AMD iGPU is invisible to nvidia-smi entirely).
fn parse_sample(stdout: &str) -> Option<GpuSample> {
    let line = stdout.lines().map(str::trim).find(|line| !line.is_empty())?;
    let (utilization, free) = line.split_once(',')?;
    Some(GpuSample {
        utilization_pct: utilization.trim().parse().ok()?,
        free_mib: free.trim().parse().ok()?,
    })
}

/// One reading of the card, or `None` when nvidia-smi is absent or answers something unparseable.
pub fn sample() -> Option<GpuSample> {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=utilization.gpu,memory.free", "--format=csv,noheader,nounits"])
        .creation_flags(CREATE_NO_WINDOW)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_sample(&String::from_utf8_lossy(&output.stdout))
}

/// Sleeps `total`, waking every [`CANCEL_CHECK_INTERVAL`] to notice a cancel. Returns `true` if it
/// was cancelled part-way.
fn sleep_watching_cancel(total: Duration, is_cancelled: &dyn Fn() -> bool) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if is_cancelled() {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(CANCEL_CHECK_INTERVAL));
    }
    is_cancelled()
}

/// Blocks until the GPU has been calm for [`CALM_SAMPLES_REQUIRED`] consecutive samples, the user
/// cancels, or [`MAX_WAIT`] elapses.
///
/// `needs_vram` must be `false` when the model is already resident — see the module docs for why
/// that is the difference between a gate and a deadlock. `on_wait` is called once, with the first
/// busy reading, if the gate is actually going to wait; it is how the caller reports "waiting for
/// the GPU" without this module knowing about toasts or settings.
pub fn wait_until_free(
    needs_vram: bool,
    is_cancelled: &dyn Fn() -> bool,
    on_wait: &dyn Fn(GpuSample),
) -> GateOutcome {
    if is_cancelled() {
        return GateOutcome::Cancelled;
    }

    // No readable GPU: proceed rather than refuse to work. A missing diagnostic is not a busy card.
    let Some(first) = sample() else {
        eprintln!("GPU gate: nvidia-smi unavailable; starting the vision pass without waiting.");
        return GateOutcome::Proceed;
    };

    let mut calm_streak = if sample_is_calm(&first, needs_vram) { 1 } else { 0 };
    if calm_streak >= CALM_SAMPLES_REQUIRED {
        return GateOutcome::Proceed;
    }
    if calm_streak == 0 {
        eprintln!(
            "GPU gate: card busy ({}% used, {} MiB free); waiting for it to free up.",
            first.utilization_pct, first.free_mib
        );
        on_wait(first);
    }

    let started = Instant::now();
    loop {
        if started.elapsed() >= MAX_WAIT {
            eprintln!("GPU gate: still busy after {} h; skipping the vision pass.", MAX_WAIT.as_secs() / 3600);
            return GateOutcome::TimedOut;
        }
        if sleep_watching_cancel(SAMPLE_INTERVAL, is_cancelled) {
            return GateOutcome::Cancelled;
        }

        match sample() {
            // The tool vanished mid-wait (driver restart, etc.). Same rule as above: proceed.
            None => {
                eprintln!("GPU gate: nvidia-smi stopped answering; starting the vision pass.");
                return GateOutcome::Proceed;
            }
            Some(reading) => {
                if sample_is_calm(&reading, needs_vram) {
                    calm_streak += 1;
                    if calm_streak >= CALM_SAMPLES_REQUIRED {
                        eprintln!(
                            "GPU gate: card free ({}% used, {} MiB free) after {} s; starting the vision pass.",
                            reading.utilization_pct,
                            reading.free_mib,
                            started.elapsed().as_secs()
                        );
                        return GateOutcome::Proceed;
                    }
                } else {
                    // Any busy reading resets the streak — the point is *sustained* quiet.
                    calm_streak = 0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_gpu_row() {
        let parsed = parse_sample("7, 27366\n").expect("row should parse");
        assert_eq!(parsed, GpuSample { utilization_pct: 7, free_mib: 27366 });
    }

    // Multi-GPU boxes print one row per card; the gate reads the first, which is card 0.
    #[test]
    fn takes_the_first_row_when_several_cards_report() {
        let parsed = parse_sample("3, 30000\n96, 512\n").expect("row should parse");
        assert_eq!(parsed.utilization_pct, 3);
    }

    #[test]
    fn rejects_output_that_is_not_two_numbers() {
        assert!(parse_sample("").is_none());
        assert!(parse_sample("\n\n").is_none());
        assert!(parse_sample("No devices were found\n").is_none());
        assert!(parse_sample("[N/A], [N/A]\n").is_none());
    }

    // The idle desktop measured on this machine (7-8%) must read as calm, or the nightly job would
    // never start at all.
    #[test]
    fn an_idle_desktop_is_calm() {
        let idle = GpuSample { utilization_pct: 8, free_mib: 27366 };
        assert!(sample_is_calm(&idle, true));
        assert!(sample_is_calm(&idle, false));
    }

    #[test]
    fn a_loaded_card_is_busy_whatever_the_vram_says() {
        let gaming = GpuSample { utilization_pct: 97, free_mib: 30000 };
        assert!(!sample_is_calm(&gaming, false));
        assert!(!sample_is_calm(&gaming, true));
    }

    // The deadlock this whole module is arranged to avoid: LM Studio already holding the ~20 GB
    // model leaves too little free to *load* one, but there is nothing left to load. A quiet card in
    // that state must read as calm, or the pass could never start once the model was up.
    #[test]
    fn a_resident_model_does_not_block_the_pass_it_is_there_for() {
        let model_resident = GpuSample { utilization_pct: 2, free_mib: 11_800 };
        assert!(!sample_is_calm(&model_resident, true), "a cold load genuinely does not fit here");
        assert!(sample_is_calm(&model_resident, false), "but nothing needs loading, so it must proceed");
    }

    // A quiet card with someone else's big model on it: nothing to preempt, but no room to load
    // into either, so a cold start must wait rather than spill into system RAM.
    #[test]
    fn a_cold_load_waits_for_headroom_even_on_a_quiet_card() {
        let full = GpuSample { utilization_pct: 1, free_mib: 2_048 };
        assert!(!sample_is_calm(&full, true));
    }

    #[test]
    fn cancelling_is_noticed_without_waiting_out_the_interval() {
        let started = Instant::now();
        let cancelled = sleep_watching_cancel(Duration::from_secs(3600), &|| true);
        assert!(cancelled);
        assert!(started.elapsed() < Duration::from_secs(5), "must not sleep the full interval");
    }

    #[test]
    fn an_already_cancelled_run_never_samples_the_gpu() {
        let outcome = wait_until_free(true, &|| true, &|_| panic!("must not report a wait"));
        assert_eq!(outcome, GateOutcome::Cancelled);
    }
}
