//! OAuth access-token management: hand out a valid access token, refreshing via
//! the refresh_token when the Keychain token has expired (e.g. Claude Code has
//! been idle). Rotated tokens are written back so Claude Code stays in sync.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::keychain::{self, Credentials};

const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Treat a token as expired this long before its real expiry, to avoid races.
const SKEW_MS: i64 = 60_000;

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64, // seconds
}

struct Cached {
    access_token: String,
    expires_at_ms: i64,
}

pub struct TokenManager {
    agent: ureq::Agent,
    cached: Mutex<Option<Cached>>,
}

impl TokenManager {
    pub fn new(agent: ureq::Agent) -> Self {
        Self {
            agent,
            cached: Mutex::new(None),
        }
    }

    /// A valid access token, refreshing if the current one is near expiry.
    pub fn access_token(&self) -> Result<String> {
        if let Some(token) = self.cached_valid() {
            return Ok(token);
        }
        let creds = keychain::load().context("load credentials")?;
        if creds.expires_at_ms - now_ms() > SKEW_MS {
            self.set_cache(&creds.access_token, creds.expires_at_ms);
            return Ok(creds.access_token);
        }
        self.refresh(&creds)
    }

    /// Force a refresh (e.g. after a 401 despite a seemingly-valid token).
    pub fn refresh_now(&self) -> Result<String> {
        let creds = keychain::load().context("load credentials")?;
        self.refresh(&creds)
    }

    fn refresh(&self, creds: &Credentials) -> Result<String> {
        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": creds.refresh_token,
            "client_id": CLIENT_ID,
        });
        let resp: RefreshResponse = self
            .agent
            .post(TOKEN_URL)
            .header("Content-Type", "application/json")
            .send_json(body)
            .context("oauth refresh request")?
            .body_mut()
            .read_json()
            .context("parse oauth refresh response")?;

        let expires_at_ms = now_ms() + resp.expires_in * 1000;
        // Persist rotated tokens so Claude Code keeps working too.
        keychain::store_refreshed(creds, &resp.access_token, &resp.refresh_token, expires_at_ms)
            .context("write back refreshed credentials")?;
        self.set_cache(&resp.access_token, expires_at_ms);
        Ok(resp.access_token)
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
