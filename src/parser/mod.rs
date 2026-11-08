pub mod agy;
pub mod claude;
pub mod codex;
pub mod extras;

use crate::{
    config::{Config, Paths},
    models::{Availability, Provider, QuotaSnapshot, QuotaWindow},
};
use reqwest::blocking::Client;
use serde_json::Value;
use std::{fs::File, io::Read, path::Path, sync::atomic::AtomicBool, time::Duration};

#[derive(Debug, Clone, Copy, Default)]
pub struct FetchOptions {
    pub offline: bool,
    pub keychain: bool,
    pub cli: bool,
    pub agy_cli: bool,
    /// A user's refresh request bypasses aitop's cached retry window.
    pub force_refresh: bool,
}
impl FetchOptions {
    pub fn configured(mut self, cfg: &Config) -> Self {
        self.keychain |= cfg.claude_keychain;
        self
    }
}

pub fn fetch(
    p: Provider,
    paths: &Paths,
    options: FetchOptions,
    cancel: &AtomicBool,
    now: i64,
) -> QuotaSnapshot {
    if options.offline {
        return QuotaSnapshot::unavailable(
            p,
            Availability::NoData,
            "Offline: no live query requested",
            now,
        );
    }
    match p {
        Provider::Claude => claude::fetch(paths, options, cancel, now),
        Provider::Codex => codex::fetch(paths, options, cancel, now),
        Provider::Agy => agy::fetch(paths, options, cancel, now),
        Provider::Cursor => extras::cursor(paths, now),
        Provider::Copilot => extras::copilot(paths, now),
        Provider::Grok => QuotaSnapshot::unavailable(
            p,
            Availability::Unsupported,
            "No verified Grok quota source. Structured local token history remains available.",
            now,
        ),
    }
}

pub fn read_json(path: &Path) -> Option<Value> {
    let file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > 1024 * 1024 {
        return None;
    }
    serde_json::from_reader(file).ok()
}
pub fn epoch(value: &Value) -> Option<i64> {
    value.as_i64().filter(|&v| v > 0).or_else(|| {
        value
            .as_str()
            .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
            .map(|v| v.timestamp())
    })
}
pub fn measured(p: Provider, source: &str, windows: Vec<QuotaWindow>, now: i64) -> QuotaSnapshot {
    if windows.is_empty() {
        return QuotaSnapshot::unavailable(
            p,
            Availability::Error,
            "Source returned no recognized quota fields",
            now,
        );
    }
    QuotaSnapshot {
        provider: p,
        availability: Availability::Available,
        source: source.into(),
        observed_at: Some(now),
        fetched_at: now,
        retry_after: now + 60,
        windows,
        message: String::new(),
        refresh_error: None,
    }
}
pub fn request(
    p: Provider,
    url: &str,
    headers: Vec<(&str, String)>,
    now: i64,
) -> std::result::Result<Value, Box<QuotaSnapshot>> {
    let failure = |message: &str| {
        let mut q = QuotaSnapshot::unavailable(p, Availability::Error, message, now);
        q.source = "https".into();
        Box::new(q)
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .user_agent(concat!("aitop/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| failure("Cannot initialize HTTPS client"))?;
    let mut request = client.get(url).header("Accept", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.send().map_err(|e| {
        failure(if e.is_timeout() {
            "Quota request timed out"
        } else {
            "Quota request failed; check connectivity"
        })
    })?;
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        let mut q = QuotaSnapshot::unavailable(
            p,
            Availability::AuthRequired,
            "Provider rejected access; check sign-in and eligibility in its own application",
            now,
        );
        q.source = "https".into();
        return Err(Box::new(q));
    }
    if !status.is_success() {
        let mut q = failure(if status.as_u16() == 429 {
            "Rate limited; waiting before retry"
        } else {
            "Provider returned an HTTP error"
        });
        let delay = response
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.parse::<i64>().ok().or_else(|| {
                    chrono::DateTime::parse_from_rfc2822(v)
                        .ok()
                        .map(|d| d.timestamp() - now)
                })
            })
            .unwrap_or(60)
            .clamp(60, 86400);
        q.retry_after = now + delay;
        return Err(q);
    }
    let mut bytes = Vec::new();
    response
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("Cannot read quota response"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(failure("Quota response exceeded 1 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(|_| failure("Provider returned an unsupported response"))
}
