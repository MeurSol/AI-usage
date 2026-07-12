//! Independent provider workers keep usage current without cross-triggering.
//!
//! GPT/Codex is local-only and refreshes from Codex JSONL events with a short
//! debounce. Claude is network-backed, so it has a longer debounce, a stricter
//! minimum request gap, and exponential 429 backoff. Manual refresh broadcasts
//! to both workers, while reset and recovery timers remain provider-specific.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use notify::RecommendedWatcher;

use crate::provider::{FetchError, Provider, UsageSnapshot};
use crate::watch;

/// Wait this long past a reset so the server has rolled the window over.
const RESET_MARGIN: Duration = Duration::from_secs(3);
/// Floor on computed reset waits, avoiding a busy loop around rollover.
const MIN_WAIT: Duration = Duration::from_secs(2);
/// Authentication cannot recover until the user logs in again. A bounded
/// fallback still lets the app recover if the credential changes silently.
const AUTH_RETRY: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy)]
struct Policy {
    turn_debounce: Duration,
    min_fetch_gap: Duration,
    idle_recheck: Duration,
}

fn policy_for(name: &str) -> Policy {
    if name != "GPT" {
        Policy {
            // Claude writes several records per turn and its usage endpoint is
            // rate-limited. A trailing debounce waits for the completed turn.
            turn_debounce: Duration::from_millis(1_200),
            min_fetch_gap: Duration::from_secs(15),
            // Normally the reset or a turn wakes us. This is only a safety net
            // for missed filesystem events or a null reset timestamp.
            idle_recheck: Duration::from_secs(6 * 60 * 60),
        }
    } else {
        Policy {
            // GPT is a local file read: update quickly after Codex finishes
            // appending the token_count/rate_limits event.
            turn_debounce: Duration::from_millis(250),
            min_fetch_gap: Duration::from_millis(500),
            idle_recheck: Duration::from_secs(6 * 60 * 60),
        }
    }
}

/// Messages sent to one provider worker.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    Turn,
    Manual,
}

/// Handle used by the menu's “Refresh now” row. The filesystem watcher is held
/// here too, keeping it alive for exactly as long as the UI trigger exists.
#[derive(Clone)]
pub struct Trigger {
    senders: Vec<Sender<Wake>>,
    _watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
}

impl Trigger {
    pub fn fire(&self) {
        for sender in &self.senders {
            let _ = sender.send(Wake::Manual);
        }
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
    /// Token missing/expired — re-login in the corresponding app.
    AuthExpired,
    /// Failed with no prior snapshot to fall back on.
    Error,
}

pub struct ProviderState {
    pub name: String,
    pub snapshot: Option<UsageSnapshot>,
    pub status: Status,
    /// Human-readable detail for the error/stale case.
    pub message: Option<String>,
    pub updated_at: Option<DateTime<Local>>,
    /// Last completed attempt for this provider, regardless of outcome.
    pub checked_at: Option<DateTime<Local>>,
    /// True only while this provider is actively doing I/O.
    pub refreshing: bool,
}

pub struct AppState {
    pub providers: Vec<ProviderState>,
    /// Bumped on every state update so the UI can skip redundant redraws.
    pub version: u64,
}

pub type Shared = Arc<Mutex<AppState>>;

/// Spawn one worker per provider. Local GPT refreshes never wait for Claude's
/// network request, and only the provider whose conversation log changed wakes.
pub fn spawn(providers: Vec<Box<dyn Provider + Send>>) -> (Shared, Trigger) {
    let provider_states = providers
        .iter()
        .map(|provider| ProviderState {
            name: provider.name().into(),
            snapshot: None,
            status: Status::Loading,
            message: None,
            updated_at: None,
            checked_at: None,
            refreshing: false,
        })
        .collect();
    let shared: Shared = Arc::new(Mutex::new(AppState {
        providers: provider_states,
        version: 0,
    }));

    let mut routes = Vec::new();
    for (index, provider) in providers.into_iter().enumerate() {
        let name = provider.name().to_owned();
        let (tx, rx) = mpsc::channel();
        routes.push((name.clone(), tx));
        let worker = Arc::clone(&shared);
        thread::spawn(move || worker_loop(index, provider, worker, rx, policy_for(&name)));
    }

    let route = |name: &str| {
        routes
            .iter()
            .find(|(provider, _)| provider == name)
            .map(|(_, sender)| sender.clone())
    };
    let watcher = watch::spawn(route("Claude"), route("GPT"));
    let senders = routes.into_iter().map(|(_, sender)| sender).collect();
    (
        shared,
        Trigger {
            senders,
            _watcher: Arc::new(Mutex::new(watcher)),
        },
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reason {
    StartupOrTimer,
    Turn,
    Manual,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Success,
    RateLimited,
    AuthExpired,
    Error,
}

fn worker_loop(
    index: usize,
    provider: Box<dyn Provider + Send>,
    shared: Shared,
    rx: Receiver<Wake>,
    policy: Policy,
) {
    let mut reason = Reason::StartupOrTimer;
    let mut last_fetch: Option<Instant> = None;
    let mut rate_limit_until: Option<Instant> = None;
    let mut rate_limit_failures = 0_u32;
    let mut transient_failures = 0_u32;

    loop {
        // Filesystem events are noisy. Debounce turns, but never delay a manual
        // refresh behind the turn debounce.
        if reason == Reason::Turn && debounce_turn(&rx, policy.turn_debounce).is_none() {
            return;
        }

        // Every provider has its own request floor. A 429 adds a stronger
        // cooldown that turn/manual triggers may queue behind but cannot bypass.
        let gap_until = last_fetch.map(|last| last + policy.min_fetch_gap);
        let not_before = match (gap_until, rate_limit_until) {
            (Some(gap), Some(limit)) => Some(gap.max(limit)),
            (Some(gap), None) => Some(gap),
            (None, Some(limit)) => Some(limit),
            (None, None) => None,
        };
        if let Some(deadline) = not_before {
            if !wait_until(deadline, &rx) {
                return;
            }
        }

        mark_refreshing(&shared, index);
        last_fetch = Some(Instant::now());
        let result = provider.fetch();
        let outcome = classify(&result);

        match outcome {
            Outcome::Success => {
                rate_limit_failures = 0;
                transient_failures = 0;
                rate_limit_until = None;
            }
            Outcome::RateLimited => {
                rate_limit_failures = rate_limit_failures.saturating_add(1);
                transient_failures = 0;
                let delay = exponential_backoff(
                    Duration::from_secs(60),
                    rate_limit_failures,
                    Duration::from_secs(15 * 60),
                );
                rate_limit_until = Some(Instant::now() + delay);
            }
            Outcome::AuthExpired => {
                transient_failures = 0;
                rate_limit_until = None;
            }
            Outcome::Error => {
                transient_failures = transient_failures.saturating_add(1);
                rate_limit_until = None;
            }
        }

        let wait = {
            let mut state = shared.lock().expect("state lock");
            apply_provider(&mut state, index, result);
            next_wait(
                &state.providers[index],
                outcome,
                rate_limit_failures,
                transient_failures,
                policy,
            )
        };

        reason = match rx.recv_timeout(wait) {
            Ok(Wake::Turn) => Reason::Turn,
            Ok(Wake::Manual) => Reason::Manual,
            Err(RecvTimeoutError::Timeout) => Reason::StartupOrTimer,
            Err(RecvTimeoutError::Disconnected) => return,
        };
    }
}

fn classify(result: &Result<UsageSnapshot, FetchError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(FetchError::RateLimited) => Outcome::RateLimited,
        Err(FetchError::AuthExpired) => Outcome::AuthExpired,
        Err(FetchError::Other(_)) => Outcome::Error,
    }
}

/// Trailing debounce for a burst of writes. A manual refresh wins immediately.
fn debounce_turn(rx: &Receiver<Wake>, debounce: Duration) -> Option<Reason> {
    loop {
        match rx.recv_timeout(debounce) {
            Ok(Wake::Turn) => continue,
            Ok(Wake::Manual) => return Some(Reason::Manual),
            Err(RecvTimeoutError::Timeout) => return Some(Reason::Turn),
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// Wait out a per-provider gap/cooldown while coalescing queued triggers.
fn wait_until(deadline: Instant, rx: &Receiver<Wake>) -> bool {
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return true;
        };
        match rx.recv_timeout(remaining) {
            Ok(_) => continue,
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

fn mark_refreshing(shared: &Shared, index: usize) {
    let mut state = shared.lock().expect("state lock");
    state.providers[index].refreshing = true;
    state.version = state.version.wrapping_add(1);
}

fn apply_provider(state: &mut AppState, index: usize, result: Result<UsageSnapshot, FetchError>) {
    let provider = &mut state.providers[index];
    provider.refreshing = false;
    provider.checked_at = Some(Local::now());
    match result {
        Ok(snapshot) => {
            provider.snapshot = Some(snapshot);
            provider.status = Status::Ok;
            provider.message = None;
            provider.updated_at = Some(Local::now());
        }
        Err(FetchError::AuthExpired) => {
            provider.status = Status::AuthExpired;
            provider.message = Some(format!("auth expired — re-login in {}", provider.name));
        }
        Err(FetchError::RateLimited) => {
            provider.status = if provider.snapshot.is_some() {
                Status::Stale
            } else {
                Status::Error
            };
            provider.message = Some("rate-limited (429); automatic backoff active".into());
        }
        Err(FetchError::Other(error)) => {
            provider.status = if provider.snapshot.is_some() {
                Status::Stale
            } else {
                Status::Error
            };
            provider.message = Some(format!("{error:#}"));
        }
    }
    state.version = state.version.wrapping_add(1);
}

fn next_wait(
    state: &ProviderState,
    outcome: Outcome,
    rate_limit_failures: u32,
    transient_failures: u32,
    policy: Policy,
) -> Duration {
    match outcome {
        Outcome::Success => {
            let margin = chrono::Duration::from_std(RESET_MARGIN).unwrap_or_default();
            state
                .snapshot
                .as_ref()
                .and_then(|snapshot| {
                    snapshot
                        .windows
                        .iter()
                        .filter_map(|window| window.resets_at)
                        .min()
                })
                .and_then(|reset| (reset - Local::now() + margin).to_std().ok())
                .unwrap_or(policy.idle_recheck)
                .max(MIN_WAIT)
        }
        Outcome::RateLimited => exponential_backoff(
            Duration::from_secs(60),
            rate_limit_failures,
            Duration::from_secs(15 * 60),
        ),
        Outcome::AuthExpired => AUTH_RETRY,
        Outcome::Error => exponential_backoff(
            Duration::from_secs(30),
            transient_failures,
            Duration::from_secs(5 * 60),
        ),
    }
}

fn exponential_backoff(base: Duration, failures: u32, cap: Duration) -> Duration {
    let shift = failures.saturating_sub(1).min(10);
    let factor = 1_u64 << shift;
    Duration::from_secs(base.as_secs().saturating_mul(factor).min(cap.as_secs()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::UsageWindow;

    fn provider_state(offsets_secs: &[i64]) -> ProviderState {
        let windows = offsets_secs
            .iter()
            .map(|&seconds| UsageWindow {
                label: "w".into(),
                utilization: 0.0,
                resets_at: Some(Local::now() + chrono::Duration::seconds(seconds)),
            })
            .collect();
        ProviderState {
            name: "test".into(),
            snapshot: Some(UsageSnapshot { windows }),
            status: Status::Ok,
            message: None,
            updated_at: None,
            checked_at: None,
            refreshing: false,
        }
    }

    #[test]
    fn provider_policies_keep_network_and_local_refreshes_independent() {
        assert_eq!(policy_for("Claude").min_fetch_gap, Duration::from_secs(15));
        assert_eq!(policy_for("GPT").min_fetch_gap, Duration::from_millis(500));
    }

    #[test]
    fn successful_provider_waits_for_its_own_reset() {
        let state = provider_state(&[10, 7_200]);
        let wait = next_wait(&state, Outcome::Success, 0, 0, policy_for("Claude"));
        assert!(wait > Duration::from_secs(10) && wait <= Duration::from_secs(14));
    }

    #[test]
    fn successful_provider_without_reset_uses_sparse_safety_recheck() {
        let mut state = provider_state(&[]);
        state.snapshot = Some(UsageSnapshot {
            windows: Vec::new(),
        });
        let policy = policy_for("Claude");
        assert_eq!(
            next_wait(&state, Outcome::Success, 0, 0, policy),
            policy.idle_recheck
        );
    }

    #[test]
    fn rate_limit_backoff_doubles_and_caps() {
        let base = Duration::from_secs(60);
        let cap = Duration::from_secs(900);
        assert_eq!(exponential_backoff(base, 1, cap), Duration::from_secs(60));
        assert_eq!(exponential_backoff(base, 2, cap), Duration::from_secs(120));
        assert_eq!(exponential_backoff(base, 5, cap), cap);
        assert_eq!(exponential_backoff(base, 20, cap), cap);
    }

    #[test]
    fn transient_backoff_is_shorter_but_bounded() {
        let base = Duration::from_secs(30);
        let cap = Duration::from_secs(300);
        assert_eq!(exponential_backoff(base, 1, cap), Duration::from_secs(30));
        assert_eq!(exponential_backoff(base, 3, cap), Duration::from_secs(120));
        assert_eq!(exponential_backoff(base, 8, cap), cap);
    }
}
