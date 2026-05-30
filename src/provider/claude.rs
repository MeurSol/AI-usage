//! Claude provider: reads the OAuth token from the Keychain and queries the
//! same endpoint Claude Code's `/usage` uses.

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::Deserialize;

use super::{FetchError, Provider, UsageSnapshot, UsageWindow};
use crate::keychain;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";

#[derive(Deserialize)]
struct RawWindow {
    utilization: f64,
    resets_at: DateTime<Local>,
}

#[derive(Deserialize)]
struct RawUsage {
    five_hour: RawWindow,
    seven_day: RawWindow,
}

/// First usable `http://` proxy from the standard env vars, if any.
fn env_http_proxy() -> Option<ureq::Proxy> {
    ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .and_then(|url| ureq::Proxy::new(&url).ok())
}

pub struct ClaudeProvider {
    agent: ureq::Agent,
}

impl ClaudeProvider {
    pub fn new() -> Self {
        // api.anthropic.com is reached via an HTTP proxy in some regions. Use
        // the HTTP-scheme proxy from the environment; we intentionally ignore
        // ALL_PROXY (often socks5, which ureq can't use without a feature).
        let config = ureq::Agent::config_builder()
            .proxy(env_http_proxy())
            .build();
        ClaudeProvider {
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// Parse the `/api/oauth/usage` JSON body into a normalized snapshot.
    fn parse(body: &str) -> Result<UsageSnapshot> {
        let raw: RawUsage = serde_json::from_str(body).context("parse usage json")?;
        Ok(UsageSnapshot {
            windows: vec![
                UsageWindow {
                    label: "Session (5h)".into(),
                    utilization: raw.five_hour.utilization,
                    resets_at: raw.five_hour.resets_at,
                },
                UsageWindow {
                    label: "Weekly (7d)".into(),
                    utilization: raw.seven_day.utilization,
                    resets_at: raw.seven_day.resets_at,
                },
            ],
        })
    }
}

impl Provider for ClaudeProvider {
    fn name(&self) -> &str {
        "claude"
    }

    fn fetch(&self) -> Result<UsageSnapshot, FetchError> {
        let token = keychain::claude_access_token().map_err(FetchError::Other)?;
        match self
            .agent
            .get(USAGE_URL)
            .header("Authorization", &format!("Bearer {token}"))
            .header("anthropic-beta", OAUTH_BETA)
            .call()
        {
            Ok(mut resp) => {
                let body = resp
                    .body_mut()
                    .read_to_string()
                    .context("read usage response body")?;
                Self::parse(&body).map_err(FetchError::Other)
            }
            Err(ureq::Error::StatusCode(401)) => Err(FetchError::AuthExpired),
            Err(e) => Err(FetchError::Other(
                anyhow::Error::new(e).context("usage request failed"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real payload shape returned by /api/oauth/usage.
    const SAMPLE: &str = r#"{
        "five_hour": {"utilization": 5.0, "resets_at": "2026-05-30T23:09:59.915338+00:00"},
        "seven_day": {"utilization": 16.0, "resets_at": "2026-06-04T10:00:00.915368+00:00"},
        "seven_day_opus": null,
        "seven_day_sonnet": null,
        "extra_usage": {"is_enabled": false, "monthly_limit": null}
    }"#;

    #[test]
    fn parses_session_and_weekly() {
        let snap = ClaudeProvider::parse(SAMPLE).expect("parse");
        assert_eq!(snap.windows.len(), 2);

        let session = &snap.windows[0];
        assert_eq!(session.label, "Session (5h)");
        assert_eq!(session.utilization, 5.0);
        assert_eq!(session.resets_at.to_utc().to_string(), "2026-05-30 23:09:59.915338 UTC");

        let weekly = &snap.windows[1];
        assert_eq!(weekly.label, "Weekly (7d)");
        assert_eq!(weekly.utilization, 16.0);
    }
}
