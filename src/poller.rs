//! Background polling: a worker thread refreshes `AppState`. It fetches on
//! several triggers — startup, each conversation turn (log watcher), the
//! "Refresh now" item, the soonest limit reset, and a steady heartbeat — so the
//! bar stays current whether or not a conversation is active.
//!
//! The UI (main thread) only ever reads the shared state.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};

use crate::provider::{FetchError, Provider, UsageSnapshot};
use crate::watch;

/// Coalesce the burst of log writes within a single turn before fetching.
const DEBOUNCE: Duration = Duration::from_millis(800);
/// Steady refresh cadence even when nothing else triggers.
const HEARTBEAT: Duration = Duration::from_secs(60);
/// Wait this long past a reset so the server has rolled the window over.
const RESET_MARGIN: Duration = Duration::from_secs(3);
/// Floor on the computed wait, to avoid a busy loop right after a reset.
const MIN_WAIT: Duration = Duration::from_secs(2);
/// Minimum gap between trigger-driven fetches (menu opens, turn events), so
/// rapid pokes coalesce instead of hammering the endpoint.
const MIN_TRIGGER_GAP: Duration = Duration::from_secs(5);

/// Handle to poke the poller into fetching now (e.g. the "Refresh now" item).
#[derive(Clone)]
pub struct Trigger(Sender<()>);

impl Trigger {
    pub fn fire(&self) {
        let _ = self.0.send(());
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// No data yet (startup).
    Loading,
    /// Latest fetch succeeded.
    Ok,
    /// Latest fetch failed but a previous snapshot is still shown.
    Stale,
    /// Token missing/expired — re-login in Claude Code.
    AuthExpired,
    /// Failed with no prior snapshot to fall back on.
    Error,
}

pub struct AppState {
    pub snapshot: Option<UsageSnapshot>,
    pub status: Status,
    /// Human-readable detail for the error/stale case.
    pub message: Option<String>,
    pub updated_at: Option<DateTime<Local>>,
    /// When the last fetch attempt completed, whatever its outcome — drives the
    /// dropdown's "Last refresh" line so a click's result is always visible.
    pub checked_at: Option<DateTime<Local>>,
    /// True while a fetch is in flight, so the UI can show a spinner.
    pub refreshing: bool,
    /// Bumped on every state update so the UI can skip redundant redraws.
    pub version: u64,
}

pub type Shared = Arc<Mutex<AppState>>;

/// Spawn the polling thread. Returns the shared state and a `Trigger` for
/// on-demand refreshes (e.g. from the menu).
pub fn spawn<P: Provider + Send + 'static>(provider: P) -> (Shared, Trigger) {
    let shared: Shared = Arc::new(Mutex::new(AppState {
        snapshot: None,
        status: Status::Loading,
        message: None,
        updated_at: None,
        checked_at: None,
        refreshing: false,
        version: 0,
    }));

    let (tx, rx) = mpsc::channel::<()>();
    let watcher = watch::spawn(tx.clone());

    let worker = Arc::clone(&shared);
    thread::spawn(move || {
        let _watcher = watcher; // keep the FS watcher alive for the thread's life
        loop {
            let last_fetch = Instant::now();
            // Mark in-flight so the UI spins while the (possibly slow) network
            // fetch runs; apply() clears it. Read directly, not version-gated.
            worker.lock().expect("state lock").refreshing = true;
            let result = provider.fetch();
            let wait = {
                let mut state = worker.lock().expect("state lock");
                apply(&mut state, result);
                next_wait(&state)
            };
            // Wake on the soonest of: a trigger (turn / menu), reset, heartbeat.
            match rx.recv_timeout(wait) {
                Ok(()) => {
                    // Spin right away so a manual "Refresh now" gives instant
                    // feedback through the debounce/rate-limit wait below — not
                    // only once the GET starts. apply() clears it.
                    worker.lock().expect("state lock").refreshing = true;
                    thread::sleep(DEBOUNCE);
                    while rx.try_recv().is_ok() {} // drain the rest of the burst
                    // Rate-limit trigger-driven fetches.
                    let since = last_fetch.elapsed();
                    if since < MIN_TRIGGER_GAP {
                        thread::sleep(MIN_TRIGGER_GAP - since);
                        while rx.try_recv().is_ok() {} // drain pokes during the gap
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                // Sender dropped (never happens; we hold a clone) — keep going.
                Err(RecvTimeoutError::Disconnected) => thread::sleep(wait),
            }
        }
    });

    (shared, Trigger(tx))
}

/// Time until the next fetch: the soonest limit reset (plus margin) if that is
/// sooner than the heartbeat, otherwise the heartbeat. Floored at `MIN_WAIT`.
///
/// A reset that's already in the past gives no useful wake-up: the server's
/// rollover can lag its own `resets_at`, and a 429 keeps the stale (past-reset)
/// snapshot — so anchoring on it would busy-poll at the margin and hammer the
/// endpoint into a sustained 429. In that case we fall back to the heartbeat.
fn next_wait(state: &AppState) -> Duration {
    let margin = chrono::Duration::from_std(RESET_MARGIN).unwrap_or_default();
    let until_reset = state
        .snapshot
        .as_ref()
        .and_then(|s| s.windows.iter().map(|w| w.resets_at).min())
        .and_then(|reset| (reset - Local::now() + margin).to_std().ok());
    until_reset
        .map_or(HEARTBEAT, |r| r.min(HEARTBEAT))
        .max(MIN_WAIT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::UsageWindow;

    fn state_with_resets(offsets_secs: &[i64]) -> AppState {
        let windows = offsets_secs
            .iter()
            .map(|&s| UsageWindow {
                label: "w".into(),
                utilization: 0.0,
                resets_at: Local::now() + chrono::Duration::seconds(s),
            })
            .collect();
        AppState {
            snapshot: Some(UsageSnapshot { windows }),
            status: Status::Ok,
            message: None,
            updated_at: None,
            checked_at: None,
            refreshing: false,
            version: 0,
        }
    }

    #[test]
    fn no_snapshot_uses_heartbeat() {
        let state = AppState {
            snapshot: None,
            status: Status::Loading,
            message: None,
            updated_at: None,
            checked_at: None,
            refreshing: false,
            version: 0,
        };
        assert_eq!(next_wait(&state), HEARTBEAT);
    }

    #[test]
    fn distant_reset_uses_heartbeat() {
        // Both windows reset hours away -> capped at the heartbeat.
        let state = state_with_resets(&[7200, 600000]);
        assert_eq!(next_wait(&state), HEARTBEAT);
    }

    #[test]
    fn soon_reset_wakes_at_boundary() {
        // Soonest reset in ~10s -> wake ~10s + margin, before the heartbeat.
        let state = state_with_resets(&[10, 7200]);
        let wait = next_wait(&state);
        assert!(wait > Duration::from_secs(10) && wait <= Duration::from_secs(14));
    }

    #[test]
    fn passed_reset_falls_back_to_heartbeat() {
        // Reset already elapsed (the server's rollover can lag its resets_at) ->
        // don't busy-poll at the margin; wait out the heartbeat so a 429 can't
        // get sustained.
        let state = state_with_resets(&[-5]);
        assert_eq!(next_wait(&state), HEARTBEAT);
    }
}

/// Fold one fetch result into the state, preserving the last good snapshot.
fn apply(state: &mut AppState, result: Result<UsageSnapshot, FetchError>) {
    state.refreshing = false;
    state.checked_at = Some(Local::now());
    state.version = state.version.wrapping_add(1);
    match result {
        Ok(snapshot) => {
            state.snapshot = Some(snapshot);
            state.status = Status::Ok;
            state.message = None;
            state.updated_at = Some(Local::now());
        }
        Err(FetchError::AuthExpired) => {
            state.status = Status::AuthExpired;
            state.message = Some("auth expired — re-login in Claude Code".into());
        }
        Err(FetchError::Other(e)) => {
            state.status = if state.snapshot.is_some() {
                Status::Stale
            } else {
                Status::Error
            };
            state.message = Some(format!("{e}"));
        }
    }
}
