//! Access-token management: hand out the token Claude Code has stored in the
//! Keychain, cached until it nears expiry.
//!
//! We deliberately do not refresh. Claude Code owns the refresh cycle, and the
//! OAuth refresh_token rotates on every use — refreshing here too would race
//! it, and the write-back needed to keep it in sync would evict Claude Code
//! from the Keychain item's partition list (see `keychain`). When the stored
//! token has expired we instead run Claude Code (see `keepalive`) and re-read
//! what it stores.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::{keepalive, keychain};

/// Treat a token as expired this long before its real expiry, to avoid races.
const SKEW_MS: i64 = 60_000;

struct Cached {
    access_token: String,
    expires_at_ms: i64,
}

/// Why we couldn't produce an access token. `Stale` means the Keychain token
/// has expired and asking Claude Code to renew it did not help — the OAuth
/// grant itself is gone, and only signing in again fixes it.
pub enum TokenError {
    Stale,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for TokenError {
    fn from(e: anyhow::Error) -> Self {
        TokenError::Other(e)
    }
}

#[derive(Default)]
pub struct TokenManager {
    cached: Mutex<Option<Cached>>,
}

impl TokenManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// A usable access token, re-reading the Keychain once the cached one
    /// nears expiry.
    pub fn access_token(&self) -> Result<String, TokenError> {
        if let Some(token) = self.cached_valid() {
            return Ok(token);
        }
        self.reload()
    }

    /// Re-read the Keychain, bypassing the cache. For a 401 on a token we
    /// still believed valid: Claude Code may have rotated it since we cached
    /// it, in which case the retry succeeds with the new one.
    ///
    /// A token that has genuinely expired means nothing has run Claude Code
    /// for hours, so we run it ourselves and read again.
    pub fn reload(&self) -> Result<String, TokenError> {
        match self.read_stored() {
            Err(TokenError::Stale) if keepalive::nudge() => self.read_stored(),
            other => other,
        }
    }

    /// The stored token, if Claude Code has left us a live one.
    fn read_stored(&self) -> Result<String, TokenError> {
        let creds = keychain::load().context("load credentials")?;
        if creds.expires_at_ms - now_ms() <= SKEW_MS {
            return Err(TokenError::Stale);
        }
        self.set_cache(&creds.access_token, creds.expires_at_ms);
        Ok(creds.access_token)
    }

    fn cached_valid(&self) -> Option<String> {
        let guard = self.cached.lock().ok()?;
        let cached = guard.as_ref()?;
        (cached.expires_at_ms - now_ms() > SKEW_MS).then(|| cached.access_token.clone())
    }

    fn set_cache(&self, access_token: &str, expires_at_ms: i64) {
        if let Ok(mut guard) = self.cached.lock() {
            *guard = Some(Cached {
                access_token: access_token.to_owned(),
                expires_at_ms,
            });
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
