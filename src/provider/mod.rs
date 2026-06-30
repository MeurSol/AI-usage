//! Usage providers. A `Provider` fetches a normalized `UsageSnapshot`.
//!
//! Today there is one provider (Claude, via the OAuth usage endpoint). The
//! trait exists so future sources (Anthropic API usage, Codex) can plug in
//! without touching the UI or polling layers.

use std::fmt;

use chrono::{DateTime, Local};

pub mod claude;

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
    /// Token missing or rejected (401) — user must re-login in Claude Code.
    AuthExpired,
    /// Anything else (network, parse, ...).
    Other(anyhow::Error),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::AuthExpired => write!(f, "auth expired — re-login in Claude Code"),
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
    fn fetch(&self) -> Result<UsageSnapshot, FetchError>;
}
