//! Background polling: a worker thread refreshes `AppState` on an interval.
//! The UI (main thread) only ever reads the shared state.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Local};

use crate::provider::{FetchError, Provider, UsageSnapshot};

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
}

pub type Shared = Arc<Mutex<AppState>>;

/// Spawn the polling thread and return the shared state. The first fetch runs
/// immediately, then repeats every `interval`.
pub fn spawn<P: Provider + Send + 'static>(provider: P, interval: Duration) -> Shared {
    let shared: Shared = Arc::new(Mutex::new(AppState {
        snapshot: None,
        status: Status::Loading,
        message: None,
        updated_at: None,
    }));

    let worker = Arc::clone(&shared);
    thread::spawn(move || loop {
        let result = provider.fetch();
        if let Ok(mut state) = worker.lock() {
            apply(&mut state, result);
        }
        thread::sleep(interval);
    });

    shared
}

/// Fold one fetch result into the state, preserving the last good snapshot.
fn apply(state: &mut AppState, result: Result<UsageSnapshot, FetchError>) {
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
