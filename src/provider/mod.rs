//! Usage providers. A `Provider` fetches a normalized `UsageSnapshot`.
//!
//! Providers normalize Claude and GPT/Codex usage into the same window model.

use std::fmt;

use chrono::{DateTime, Local};

pub mod claude;
pub mod gpt;

/// One rate-limit window (e.g. the 5-hour session or the 7-day weekly limit).
#[derive(Debug, Clone)]
pub struct UsageWindow {
    pub label: String,
    /// Percent used, 0.0 – 100.0.
    pub utilization: f64,
    /// When this window rolls over. The endpoint returns `null` for a window
    /// that isn't currently counting down, so this is optional.
    pub resets_at: Option<DateTime<Local>>,
}

/// A point-in-time view of all windows for one provider.
#[derive(Debug, Clone)]
pub struct UsageSnapshot {
    /// Ordered: session first, then weekly.
    pub windows: Vec<UsageWindow>,
}

/// Why a fetch failed, kept distinct so the UI can prompt re-auth specifically.
#[derive(Debug)]
pub enum FetchError {
    /// Token missing, stale, or rejected (401). Claude Code owns the refresh
    /// cycle and we already ran it (see `keepalive`) without getting a live
    /// token back, so the OAuth grant is gone and only signing in clears this.
    AuthExpired,
    /// Provider explicitly rejected the request for polling too quickly.
    RateLimited,
    /// Anything else (network, parse, ...).
    Other(anyhow::Error),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::AuthExpired => write!(f, "credential expired — sign in to Claude Code"),
            FetchError::RateLimited => write!(f, "rate-limited (429)"),
            FetchError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FetchError {}

impl From<anyhow::Error> for FetchError {
    fn from(e: anyhow::Error) -> Self {
        FetchError::Other(e)
    }
}

pub trait Provider {
    /// Short product name shown in the menu bar and dropdown.
    fn name(&self) -> &'static str;

    fn fetch(&self) -> Result<UsageSnapshot, FetchError>;
}
