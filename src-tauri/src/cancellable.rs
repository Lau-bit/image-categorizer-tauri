//! Abandon-on-stop: what makes a Stop button mean *now*.
//!
//! Every long pass in this app is a loop over items, and each loop polls its `control.cancel` flag
//! **between** items. That makes Stop cost "however long the current item takes" — a couple of
//! hundred milliseconds for an OCR or an ONNX score, but a whole model generation for a Describe,
//! a Topics bucket or a Kinds batch, which on a local vision model is tens of seconds and can be
//! minutes. A button that does nothing for a minute reads as a button that does nothing.
//!
//! There is no way to interrupt a blocking `ureq` call from another thread — ureq 2 exposes
//! neither the socket nor an abort handle — so this module does the next best thing: it runs the
//! item's work on a throwaway thread and **stops waiting for it**. The abandoned thread runs to
//! its natural end (the server answers, or the 300 s read timeout fires) and its result is dropped
//! on the floor.
//!
//! **Discarding the half-finished item is not a compromise here, it is the specified behaviour.**
//! Every pass commits a result only *after* the call it abandoned would have returned one, so an
//! abandoned item leaves no sidecar entry, no description file and no marker. The next run's
//! `pending_*` selector sees it as untouched and does it again from the top — the newest item is
//! simply redone, never half-recorded.
//!
//! Cost of abandoning: one orphaned thread per Stop, holding its request buffer and its connection
//! until the endpoint answers or times out. Bounded, small, and it cannot outlive the read timeout.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// How often the waiting side re-reads the cancel flag. Small enough that Stop feels immediate,
/// large enough that waiting costs nothing while a pass grinds.
const POLL: Duration = Duration::from_millis(50);

/// What came back from [`until_cancelled`].
pub enum Interrupted<T> {
    /// The work finished on its own; here is its value.
    Done(T),
    /// Somebody asked to stop while the work was still running. The worker is abandoned and its
    /// value — if it ever produces one — is dropped.
    Cancelled,
    /// The worker died without producing a value (it panicked, or the thread could not be
    /// spawned). Kept apart from `Cancelled` so a crash is never reported to the user as "you
    /// stopped it".
    Lost,
}

/// Runs `work` on a throwaway thread and returns the moment either the work finishes or
/// `is_cancelled()` goes true — whichever happens first.
///
/// `work` must own everything it touches (`Send + 'static`): the whole point is that this function
/// can return while the closure is still running, so it cannot borrow from the caller's frame.
/// Cloning a `ureq::Agent`, an `AppHandle` and a few `String`s per item is cheap — an `Agent` is an
/// `Arc` around the connection pool, so the clone shares it rather than opening new sockets.
pub fn until_cancelled<T, F>(is_cancelled: &dyn Fn() -> bool, work: F) -> Interrupted<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // Asked to stop before it began: don't start the work at all.
    if is_cancelled() {
        return Interrupted::Cancelled;
    }

    let (tx, rx) = mpsc::sync_channel::<T>(1);
    // The `JoinHandle` is deliberately dropped: nothing ever joins this thread. `sync_channel(1)`
    // means the send never blocks even when nobody is listening any more, so an abandoned worker
    // still exits cleanly instead of parking forever on a full channel.
    if thread::Builder::new()
        .name("cancellable-work".to_string())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .is_err()
    {
        return Interrupted::Lost;
    }

    loop {
        match rx.recv_timeout(POLL) {
            Ok(value) => return Interrupted::Done(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if is_cancelled() {
                    return Interrupted::Cancelled;
                }
            }
            // The sender was dropped without a send: the worker panicked. No value is coming.
            Err(mpsc::RecvTimeoutError::Disconnected) => return Interrupted::Lost,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn returns_the_value_when_nothing_cancels() {
        let outcome = until_cancelled(&|| false, || 7u32);
        assert!(matches!(outcome, Interrupted::Done(7)));
    }

    #[test]
    fn stops_waiting_without_waiting_out_the_work() {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let flag = Arc::clone(&flag);
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(120));
                flag.store(true, Ordering::SeqCst);
            });
        }
        let started = Instant::now();
        let outcome = until_cancelled(&|| flag.load(Ordering::SeqCst), || {
            // Stands in for a model generation that will not be interrupted from outside.
            thread::sleep(Duration::from_secs(30));
            "described"
        });
        assert!(matches!(outcome, Interrupted::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "Stop must not wait out the item it interrupts (took {:?})",
            started.elapsed()
        );
    }

    #[test]
    fn a_flag_already_set_never_starts_the_work() {
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let outcome = until_cancelled(&|| true, move || flag.store(true, Ordering::SeqCst));
        assert!(matches!(outcome, Interrupted::Cancelled));
        assert!(!ran.load(Ordering::SeqCst), "work must not start once Stop is already pending");
    }

    #[test]
    fn a_panicking_worker_is_lost_not_cancelled() {
        let outcome = until_cancelled::<(), _>(&|| false, || panic!("worker died"));
        assert!(matches!(outcome, Interrupted::Lost));
    }
}
