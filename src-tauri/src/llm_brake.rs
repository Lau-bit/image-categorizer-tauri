//! Listening for `local-llm-control`'s brake, so a pass STOPS instead of fighting it.
//!
//! `local-llm-control` (the widget on 5289) has two positions that stop local model work:
//! **halt**, which aborts everything and unloads, and **guard**, which leaves what is already
//! resident alone. Both of them *block new model loads* — LM Studio has no way to refuse a
//! load, so the widget does it by watching residency and aborting a load while it is still
//! happening.
//!
//! ## What this app looked like without it, and why that is worth a module
//!
//! To a client, a blocked load is indistinguishable from a bad day at the server: the request
//! comes back
//!
//! ```text
//! HTTP 400  Failed to load model "google/gemma-4-26b-a4b".
//!           Error: Engine protocol startup was aborted.
//! ```
//!
//! …which is true, mentions nothing about anybody's intent, and looks exactly like a model
//! that is not downloaded or a server that is unwell. A describe pass would then do the
//! obvious thing and move to the next image — which starts a fresh load, which is aborted, for
//! every one of the images left in the queue. The backlog here is thousands of images. Each
//! round trip is a few seconds of disk and GPU on both sides to arrive back where it began,
//! and the user watching the GPU sees the model appearing to "reload fast" over and over
//! against a brake that is plainly on.
//!
//! `VISION_FAIL_FAST_ATTEMPTS` does not save this. It only fires while *nothing* has been
//! described yet, so a brake pressed in the middle of a working pass — which is when a person
//! actually presses it — leaves the loop grinding to the end of its queue.
//!
//! Blocking the loads is the widget's job. **Not provoking them is this app's**, and one fact
//! is enough to manage it: that somebody meant this.
//!
//! ## The channel
//!
//! `GET http://127.0.0.1:5289/api/brake` — a read, a few hundred bytes, no snapshot behind it.
//! It carries `stop`, the position, when it was taken and a sentence naming the app that took
//! it, so this app can say *"Local LLM Control is holding a halt"* on its own screen rather
//! than passing on LM Studio's account of a symptom.
//!
//! **Three rules, and the first is the one that matters:**
//!
//! 1. **Silence means GO.** An unreachable port means the widget is not running, and a widget
//!    that is not running is not braking anything. A refused connection, a timeout, a 404 from
//!    an older build, a body that will not parse — every one of them answers `None` and this
//!    app works normally. A consumer that stopped working because a status port was closed
//!    would be a new and much worse failure than the one being fixed.
//! 2. **It never blocks a pass for long.** 300 ms of connect and read against loopback, and a
//!    result cached for a second, so a per-item check costs nothing next to a generation.
//! 3. **Reading only.** Nothing here takes or releases a position — that is two buttons on a
//!    window somebody is looking at. This app learns what is true and decides about its own
//!    work, which is the whole of what it is owed.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The widget's loopback read API. Fixed, and registered in the drawer's `ports.json`.
const BRAKE_URL: &str = "http://127.0.0.1:5289/api/brake";

/// Loopback, and a pass may ask between items — so a widget that is wedged costs a blink, not
/// a stall. Anything that cannot answer in this long is treated as not there, which is the
/// safe direction (see rule 1).
const TIMEOUT: Duration = Duration::from_millis(300);

/// How long an answer stands before it is asked for again. Long enough that a per-item check
/// is free, short enough that a position taken mid-pass is acted on within a second — which is
/// far inside the time one image takes anyway.
const CACHE: Duration = Duration::from_millis(1000);

/// A position that is being held right now, as this app needs to read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// `guard` or `halt`. Never `off` — an unheld brake is `None`, not a `Held` saying no.
    pub mode: String,
    /// The widget's own sentence, already naming the app and saying what to do about it.
    pub message: String,
    /// False when the widget could not find `lms.exe`, so nothing is actually enforcing the
    /// position. Still reported: the user should be told the difference between "your work was
    /// stopped" and "somebody meant to stop it and nothing did".
    pub enforced: bool,
}

impl Held {
    /// What a pass says when it stands down. Names the app that did it and what was kept, so a
    /// stopped run does not read as a crash. `noun` is singular — "description", "batch".
    pub fn pass_message(&self, done: usize, noun: &str) -> String {
        let kept = match done {
            0 => format!("No {noun}s were kept"),
            1 => format!("1 {noun} was kept"),
            n => format!("{n} {noun}s were kept"),
        };
        format!("{} {kept}; the rest is left for the next run.", self.message)
    }
}

#[derive(Clone)]
struct Cached {
    at: Instant,
    held: Option<Held>,
}

static CACHED: Mutex<Option<Cached>> = Mutex::new(None);

/// Is local model work being stopped right now?
///
/// `None` — and this is the important half — for every reason including "the widget is not
/// running", "it answered something unexpected" and "it is not holding anything". Callers use
/// this to decide whether to stop their own work, so the only answer that may ever be
/// synthesised is the permissive one.
pub fn held() -> Option<Held> {
    if let Ok(guard) = CACHED.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.at.elapsed() < CACHE {
                return cached.held.clone();
            }
        }
    }
    let fresh = ask();
    if let Ok(mut guard) = CACHED.lock() {
        *guard = Some(Cached { at: Instant::now(), held: fresh.clone() });
    }
    fresh
}

/// Ask the widget, ignoring the cache. For the moment a request has just failed: the position
/// may have been taken a few hundred milliseconds ago, and a cached "nothing is held" from
/// before the failure is exactly the answer that would send the pass on to the next item.
pub fn held_now() -> Option<Held> {
    let fresh = ask();
    if let Ok(mut guard) = CACHED.lock() {
        *guard = Some(Cached { at: Instant::now(), held: fresh.clone() });
    }
    fresh
}

fn ask() -> Option<Held> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(TIMEOUT)
        .timeout_read(TIMEOUT)
        .build();
    // `.ok()?` on purpose, repeatedly: a refused connection, a timeout, a 404 from a build
    // that predates this route and a body that will not parse are all "no brake", and telling
    // them apart would only produce a way for this app to stop working over a missing widget.
    let body = agent.get(BRAKE_URL).call().ok()?.into_string().ok()?;
    parse(&body)
}

/// Split from the socket so the contract with the widget can be pinned by a test.
fn parse(body: &str) -> Option<Held> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    if !value.get("stop").and_then(|v| v.as_bool()).unwrap_or(false) {
        return None;
    }
    let mode = value.get("mode").and_then(|v| v.as_str()).unwrap_or("held").to_string();
    let message = value
        .get("message")
        .and_then(|v| v.as_str())
        .filter(|m| !m.trim().is_empty())
        .unwrap_or("Local LLM Control is stopping local model work.")
        .to_string();
    Some(Held {
        mode,
        message,
        enforced: value.get("enforced").and_then(|v| v.as_bool()).unwrap_or(true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_halt_is_read_with_the_widgets_own_sentence() {
        let held = parse(
            r#"{"mode":"halt","stop":true,"since":1,"enforced":true,"blocked":3,
                "message":"Local LLM Control is holding a HALT: stop rather than retrying."}"#,
        )
        .expect("a held halt");
        assert_eq!(held.mode, "halt");
        assert!(held.enforced);
        assert!(held.message.contains("Local LLM Control"));
    }

    #[test]
    fn guard_stops_this_app_too() {
        // This app's model is rarely already resident, so a guard blocks its loads exactly as
        // a halt does. Reading only `halt` would leave the same grind under the other button.
        let held = parse(r#"{"mode":"guard","stop":true,"message":"…"}"#).expect("a held guard");
        assert_eq!(held.mode, "guard");
    }

    #[test]
    fn nothing_held_is_none() {
        assert_eq!(parse(r#"{"mode":"off","stop":false,"message":"…"}"#), None);
    }

    /// EVERY malformed answer has to be `None`. The failure being avoided is not a wrong
    /// message — it is this app refusing to work because the widget was upgraded, downgraded
    /// or closed.
    #[test]
    fn anything_unparseable_reads_as_no_brake() {
        for body in ["", "not json", "{}", "[]", "null", r#"{"error":"try /api/health"}"#] {
            assert_eq!(parse(body), None, "{body:?} must read as no brake");
        }
    }

    /// An older widget that answers `stop` and nothing else still stops the pass, with a
    /// sentence of our own rather than an empty one.
    #[test]
    fn a_sparse_answer_still_carries_a_sentence() {
        let held = parse(r#"{"stop":true}"#).expect("held");
        assert!(!held.message.trim().is_empty());
        assert!(held.enforced, "unstated enforcement is assumed real");
    }

    #[test]
    fn the_pass_message_says_what_was_kept() {
        let held = parse(r#"{"stop":true,"message":"Held."}"#).unwrap();
        assert!(held.pass_message(0, "description").contains("No descriptions were kept"));
        assert!(held.pass_message(1, "description").contains("1 description was kept"));
        assert!(held.pass_message(7, "description").contains("7 descriptions were kept"));
    }
}
