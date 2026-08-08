//! Idle lease over the shared local model.
//!
//! LM Studio keeps exactly ONE model resident, so "is the model loaded" is machine state several
//! apps share — this app, Local LLM Chat, the asset forges, the GUI. A 26B model is ~18 GB of VRAM,
//! and leaving it there after a describe run finished costs every other app on the box. This module
//! is how the app takes responsibility for **the load it caused**, and only that one:
//!
//! - Every chat request carries `ttl` (see [`crate::vision::set_idle_ttl_secs`]). LM Studio binds
//!   that at load time, so it shortens the idle life of a model this app JIT-loaded and is ignored
//!   for one that was already resident. The server resets the countdown on traffic from *any*
//!   client, so another app's work keeps the model alive with nothing to coordinate here — which is
//!   the whole reason the unload is left to the server rather than done from this side. Nothing in
//!   this process can see another app's requests; LM Studio sees all of them.
//! - That countdown knows nothing about a user who is busy in this app but not hitting the model.
//!   The watchdog closes that gap: while the app holds the lease and the window has seen recent
//!   interaction, it sends one cheap 1-token poke every half-window, which re-arms the timer.
//!
//! Net effect: no LM Studio traffic from anyone AND no in-app activity for the idle window → the
//! model unloads. Either one → it stays. And if this app never loaded the model, it never expires
//! it — the lease is simply never claimed.
//!
//! The lease also survives the app closing in the right direction: the deadline lives in the
//! server, so quitting mid-lease still unloads the model on schedule instead of stranding it.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::vision::{build_probe_agent, model_state, warm_model, ModelState};

/// How long "quiet" has to last before the model is allowed to go. Also the `ttl` handed to the
/// server, so the two can never disagree.
pub const DEFAULT_IDLE_MINUTES: u32 = 5;
/// Two minutes is the floor because the keep-alive has to fit inside the window with a poll
/// interval of slack — see `keepalive_gap_leaves_room_for_poll_jitter`.
pub const MIN_IDLE_MINUTES: u32 = 2;
pub const MAX_IDLE_MINUTES: u32 = 240;

/// How often the watchdog looks. Deliberately small next to the keep-alive gap, so poll jitter can
/// never push a keep-alive past the server's deadline.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Never poke more often than this, however short the idle window is configured.
const MIN_KEEPALIVE_GAP: Duration = Duration::from_secs(60);

/// The keep-alive cadence for a given idle window: half of it, so a poke plus a full poll interval
/// of slack still lands comfortably inside the server's countdown.
fn keepalive_gap(idle: Duration) -> Duration {
    (idle / 2).max(MIN_KEEPALIVE_GAP)
}

/// What the app knows about the load it is holding.
#[derive(Default)]
pub struct ModelLease {
    inner: Mutex<LeaseState>,
}

#[derive(Default)]
struct LeaseState {
    /// The model this app JIT-loaded and is therefore allowed to let expire. `None` = the app is
    /// borrowing somebody else's load, or nothing is loaded.
    owned_model: Option<String>,
    /// Last deliberate user interaction with the window (reported by the frontend).
    last_app_activity: Option<Instant>,
    /// Last request this process sent the endpoint — every one of them re-arms the server timer,
    /// so this is what the keep-alive rate-limits against.
    last_request: Option<Instant>,
}

/// Lease state for the Settings panel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdleLeaseStatus {
    pub enabled: bool,
    pub idle_minutes: u32,
    /// The model this app loaded and holds the lease on, if any.
    pub owned_model: Option<String>,
    pub seconds_since_activity: Option<u64>,
    pub seconds_since_request: Option<u64>,
}

impl ModelLease {
    fn lock(&self) -> std::sync::MutexGuard<'_, LeaseState> {
        // A panic in a lease update must not take the whole feature down with it; the state is
        // three timestamps and is self-correcting on the next tick.
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records deliberate user interaction with the window.
    pub fn note_app_activity(&self) {
        self.lock().last_app_activity = Some(Instant::now());
    }

    /// Records that a request just went to the endpoint (which reset its idle countdown).
    pub fn note_request(&self) {
        self.lock().last_request = Some(Instant::now());
    }

    /// Claims the lease when `before` — the residency read taken immediately *before* one of this
    /// app's requests — says the model was cold, i.e. that request is what loaded it.
    ///
    /// `Loaded` and `Unknown` both claim nothing: somebody else owns that load, or the app can't
    /// tell, and either way it must not shorten a life it didn't create.
    pub fn claim(&self, model: &str, before: ModelState) {
        if before != ModelState::NotLoaded {
            return;
        }
        let mut state = self.lock();
        state.owned_model = Some(model.to_string());
        state.last_request = Some(Instant::now());
    }

    pub fn owned_model(&self) -> Option<String> {
        self.lock().owned_model.clone()
    }

    /// Gives up the lease, returning the model that was held. Called when the watchdog sees the
    /// model gone — whoever loads it next owns it, not this app.
    fn release(&self) -> Option<String> {
        self.lock().owned_model.take()
    }

    fn spans(&self) -> (Option<Duration>, Option<Duration>) {
        let state = self.lock();
        (
            state.last_app_activity.map(|at| at.elapsed()),
            state.last_request.map(|at| at.elapsed()),
        )
    }

    pub fn status(&self, enabled: bool, idle_minutes: u32) -> IdleLeaseStatus {
        let (since_activity, since_request) = self.spans();
        IdleLeaseStatus {
            enabled,
            idle_minutes,
            owned_model: self.owned_model(),
            seconds_since_activity: since_activity.map(|span| span.as_secs()),
            seconds_since_request: since_request.map(|span| span.as_secs()),
        }
    }
}

/// Reads whether `model` is resident right now, for a caller about to send a request that could
/// load it. Best-effort: an unreachable or non-LM-Studio endpoint answers `Unknown`, which claims
/// nothing and leaves the watchdog inert.
pub fn probe(app: &AppHandle, model: &str) -> ModelState {
    let settings = crate::load_app_settings(app);
    if !crate::vision_idle_unload(&settings) {
        return ModelState::Unknown;
    }
    let Some(models_url) = crate::vision_rest_models_url(&settings) else {
        return ModelState::Unknown;
    };
    let api_key = crate::vision_api_key(&settings);
    model_state(&build_probe_agent(), &models_url, model, api_key.as_deref())
}

/// Probes, runs `request`, and claims the lease if the probe said the model was cold — the shape
/// every load-capable call site wants, so none of them has to remember the ordering.
pub fn with_claim<T>(app: &AppHandle, model: &str, request: impl FnOnce() -> T) -> T {
    let before = probe(app, model);
    let result = request();
    let lease = app.state::<ModelLease>();
    lease.claim(model, before);
    lease.note_request();
    result
}

/// Starts the background watchdog. One thread, one cheap local GET every 30 s, and a 1-token poke
/// only while the app both holds the lease and is being used.
pub fn spawn_watchdog(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(POLL_INTERVAL);
        tick(&app);
    });
}

fn tick(app: &AppHandle) {
    let lease = app.state::<ModelLease>();
    // Nothing claimed: this app didn't load whatever is resident, so it has no business poking it.
    let Some(model) = lease.owned_model() else {
        return;
    };

    let settings = crate::load_app_settings(app);
    let Some(idle) = crate::vision_idle_window(&settings) else {
        // Switched off while holding a lease: stop managing this load. The TTL already bound to it
        // server-side still stands — LM Studio fixes `ttl` at load time and no later request can
        // change it — so turning the setting off takes effect from the *next* load, not this one.
        lease.release();
        return;
    };

    match probe(app, &model) {
        // The model went — timed out, or another app's load evicted it. Either way the lease is
        // over and the next load belongs to whoever causes it.
        ModelState::NotLoaded => {
            if lease.release().is_some() {
                let _ = app.emit("model-lease-released", &model);
            }
            return;
        }
        // Can't tell. Leave everything as it is rather than guessing in either direction.
        ModelState::Unknown => return,
        ModelState::Loaded => {}
    }

    let (since_activity, since_request) = lease.spans();
    // No in-app activity inside the window: let the server's countdown run out. This is the whole
    // point of the feature — the app going quiet is what releases the model.
    let Some(since_activity) = since_activity else {
        return;
    };
    if since_activity >= idle {
        return;
    }
    // Requests are still flowing (a describe or classify pass): they already re-arm the timer.
    if since_request.is_some_and(|span| span < keepalive_gap(idle)) {
        return;
    }

    let endpoint = crate::vision_endpoint(&settings);
    let api_key = crate::vision_api_key(&settings);
    lease.note_request();
    if let Err(error) = warm_model(&build_probe_agent(), &endpoint, &model, api_key.as_deref()) {
        // Non-fatal by design: the only consequence of a failed poke is that the model expires on
        // the server's schedule, which is the behaviour being asked for anyway.
        eprintln!("Model keep-alive poke failed for \"{model}\": {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_cold_probe_claims_the_lease() {
        let lease = ModelLease::default();

        // Already resident: somebody else's load, so the app must not take it over.
        lease.claim("m", ModelState::Loaded);
        assert_eq!(lease.owned_model(), None);

        // Couldn't tell: same answer, for the same reason.
        lease.claim("m", ModelState::Unknown);
        assert_eq!(lease.owned_model(), None);

        // Cold: this app's request is what brings it up, so the lease is its own.
        lease.claim("m", ModelState::NotLoaded);
        assert_eq!(lease.owned_model(), Some("m".to_string()));

        assert_eq!(lease.release(), Some("m".to_string()));
        assert_eq!(lease.owned_model(), None);
    }

    // The poke must land inside the server's countdown with room for a whole poll interval of
    // jitter, or the keep-alive races the very deadline it exists to postpone.
    #[test]
    fn keepalive_gap_leaves_room_for_poll_jitter() {
        for minutes in [MIN_IDLE_MINUTES, DEFAULT_IDLE_MINUTES, 30, MAX_IDLE_MINUTES] {
            let idle = Duration::from_secs(u64::from(minutes) * 60);
            let gap = keepalive_gap(idle);
            assert!(
                gap + POLL_INTERVAL < idle,
                "a {minutes}-minute window pokes every {gap:?}, which can slip past the deadline"
            );
        }
    }

    // A one-minute window is the floor precisely because the gap can't shrink below the rate limit;
    // pin that the two constants stay compatible.
    #[test]
    fn shortest_window_still_respects_the_rate_limit() {
        let idle = Duration::from_secs(u64::from(MIN_IDLE_MINUTES) * 60);
        assert_eq!(keepalive_gap(idle), MIN_KEEPALIVE_GAP);
    }
}
