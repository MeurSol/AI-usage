//! Background polling: a worker thread refreshes `AppState`. It fetches only
//! when there's a reason to — startup, each conversation turn (log watcher), the
//! "Refresh now" item, and the soonest limit reset — with no steady heartbeat.
//! A failed fetch retries on a bounded interval so the bar recovers on its own.
//! Every fetch is throttled to `MIN_FETCH_GAP` apart so overlapping triggers
//! can't hammer the endpoint into a 429.
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
/// Wait this long past a reset so the server has rolled the window over.
const RESET_MARGIN: Duration = Duration::from_secs(3);
/// Floor on the computed wait, to avoid a busy loop right after a reset.
const MIN_WAIT: Duration = Duration::from_secs(2);
/// Minimum gap between *any* two fetches (turn, reset, click, retry), so
/// overlapping triggers coalesce instead of hammering the endpoint into a 429.
const MIN_FETCH_GAP: Duration = Duration::from_secs(5);
/// Bounded retry after a failed fetch (or a server rollover that lags its
/// `resets_at`), so the bar recovers on its own without a turn or a click. This
/// is recovery only — there is no steady heartbeat when fetches succeed.
const RETRY: Duration = Duration::from_secs(30);

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
        let mut last_fetch: Option<Instant> = None;
        loop {
            // Throttle: never fetch more often than MIN_FETCH_GAP, whatever woke
            // us (turn, reset, Refresh now, retry), so overlapping triggers can't
            // trip the endpoint's 429.
            if let Some(prev) = last_fetch {
                let since = prev.elapsed();
                if since < MIN_FETCH_GAP {
                    thread::sleep(MIN_FETCH_GAP - since);
                    while rx.try_recv().is_ok() {} // coalesce pokes during the gap
                }
            }
            last_fetch = Some(Instant::now());
            // Mark in-flight so the UI spins while the (possibly slow) network
            // fetch runs; apply() clears it. Read directly, not version-gated.
            worker.lock().expect("state lock").refreshing = true;
            let result = provider.fetch();
            let wait = {
                let mut state = worker.lock().expect("state lock");
                apply(&mut state, result);
                next_wait(&state)
            };
            // Wake on the soonest of: a trigger (turn / Refresh now), the next
            // reset boundary, or the recovery retry. No steady heartbeat.
            match rx.recv_timeout(wait) {
                Ok(()) => {
                    // Spin right away so a manual "Refresh now" gives instant
                    // feedback through the wait below, and let a turn's burst of
                    // log writes settle before looping back to fetch.
                    worker.lock().expect("state lock").refreshing = true;
                    thread::sleep(DEBOUNCE);
                    while rx.try_recv().is_ok() {} // drain the rest of the burst
                }
                Err(RecvTimeoutError::Timeout) => {}
                // Sender dropped (never happens; we hold a clone) — stop the loop.
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });

    (shared, Trigger(tx))
}

/// Time until the next fetch. On success, sleep until the soonest limit reset
/// (plus margin) to catch the rollover — there is no steady heartbeat. On
/// failure (or no data yet) retry on a bounded interval so the bar recovers.
///
/// A reset that's already in the past gives no useful wake-up: the server's
/// rollover can lag its own `resets_at`, and a 429 keeps the stale (past-reset)
/// snapshot — so anchoring on it would busy-poll at the margin. In that case we
/// fall back to the bounded retry instead.
fn next_wait(state: &AppState) -> Duration {
    let margin = chrono::Duration::from_std(RESET_MARGIN).unwrap_or_default();
    let until_reset = state
        .snapshot
        .as_ref()
        .and_then(|s| s.windows.iter().filter_map(|w| w.resets_at).min())
        .and_then(|reset| (reset - Local::now() + margin).to_std().ok());
    match state.status {
        // Healthy: wait out the soonest reset; an elapsed reset retries instead.
        Status::Ok => until_reset.unwrap_or(RETRY).max(MIN_WAIT),
        // Failed / no data yet: bounded recovery retry (not a heartbeat).
        _ => RETRY,
    }
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
                resets_at: Some(Local::now() + chrono::Duration::seconds(s)),
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
    fn no_data_uses_retry() {
        // No snapshot yet (a failed startup fetch) -> bounded recovery retry.
        let state = AppState {
            snapshot: None,
            status: Status::Error,
            message: None,
            updated_at: None,
            checked_at: None,
            refreshing: false,
            version: 0,
        };
        assert_eq!(next_wait(&state), RETRY);
    }

    #[test]
    fn healthy_waits_until_reset() {
        // Healthy with the soonest reset hours away -> wait that long, no cap
        // (no heartbeat).
        let state = state_with_resets(&[7200, 600000]);
        let wait = next_wait(&state);
        assert!(wait > Duration::from_secs(7100) && wait <= Duration::from_secs(7205));
    }

    #[test]
    fn soon_reset_wakes_at_boundary() {
        // Soonest reset in ~10s -> wake ~10s + margin.
        let state = state_with_resets(&[10, 7200]);
        let wait = next_wait(&state);
        assert!(wait > Duration::from_secs(10) && wait <= Duration::from_secs(14));
    }

    #[test]
    fn passed_reset_falls_back_to_retry() {
        // Reset already elapsed (the server's rollover can lag its resets_at) ->
        // don't busy-poll at the margin; use the bounded retry instead.
        let state = state_with_resets(&[-5]);
        assert_eq!(next_wait(&state), RETRY);
    }

    #[test]
    fn stale_uses_retry() {
        // A failed fetch that kept a prior snapshot still retries on the bounded
        // interval, not at the (stale) reset boundary.
        let mut state = state_with_resets(&[10]);
        state.status = Status::Stale;
        assert_eq!(next_wait(&state), RETRY);
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
            // `{e:#}` includes the cause chain (e.g. "usage request failed: http
            // status: 429") so the dropdown can show why a refresh failed.
            state.message = Some(format!("{e:#}"));
        }
    }
}
