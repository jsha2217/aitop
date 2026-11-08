use super::{epoch, measured, read_json, request, FetchOptions};
use crate::{
    config::Paths,
    models::{Availability, Provider, QuotaSnapshot, QuotaWindow},
    process,
};
use serde_json::Value;
use std::{process::Command, sync::atomic::AtomicBool, time::Duration};

fn windows(rate: &Value, prefix: &str) -> Vec<QuotaWindow> {
    let mut windows = vec![];
    for (key, other) in [
        ("primary", "primary_window"),
        ("secondary", "secondary_window"),
    ] {
        let Some(v) = rate.get(key).or_else(|| rate.get(other)) else {
            continue;
        };
        let minutes = v
            .get("windowDurationMins")
            .or_else(|| v.get("window_minutes"))
            .and_then(Value::as_u64)
            .or_else(|| {
                v.get("limit_window_seconds")
                    .and_then(Value::as_u64)
                    .map(|v| v / 60)
            });
        let duration = match minutes {
            Some(300) => "5h".into(),
            Some(10080) => "7d".into(),
            Some(n) => format!("{n}m"),
            None => key.into(),
        };
        let label = if prefix.is_empty() {
            duration
        } else {
            format!("{prefix} {duration}")
        };
        let percent = v
            .get("used_percent")
            .or_else(|| v.get("usedPercent"))
            .and_then(Value::as_f64);
        if let Some(q) = percent.and_then(|pct| {
            QuotaWindow::new(
                &format!("{prefix}:{key}"),
                &label,
                pct,
                v.get("resets_at")
                    .or_else(|| v.get("reset_at"))
                    .or_else(|| v.get("resetsAt"))
                    .and_then(epoch),
                minutes,
            )
        }) {
            windows.push(q);
        }
    }
    windows
}

pub fn local_limits(value: &Value, observed_at: i64) -> QuotaSnapshot {
    let mut q = measured(
        Provider::Codex,
        "local rate-limit snapshot",
        windows(value, ""),
        observed_at,
    );
    q.message = "Local session snapshot; account attribution unknown".into();
    q
}
pub fn api_usage(value: &Value, now: i64) -> QuotaSnapshot {
    measured(
        Provider::Codex,
        "oauth-api (experimental)",
        windows(value.get("rate_limit").unwrap_or(&Value::Null), ""),
        now,
    )
}
pub fn rpc_usage(value: &Value, now: i64) -> QuotaSnapshot {
    let result = value.get("result").unwrap_or(value);
    let mut out = Vec::new();
    if let Some(buckets) = result.get("rateLimitsByLimitId").and_then(Value::as_object) {
        for (id, rate) in buckets {
            out.extend(windows(rate, id));
        }
    } else if let Some(rate) = result.get("rateLimits") {
        out.extend(windows(rate, ""));
    }
    measured(Provider::Codex, "app-server", out, now)
}
pub fn fetch(paths: &Paths, options: FetchOptions, cancel: &AtomicBool, now: i64) -> QuotaSnapshot {
    if options.cli {
        let input=b"{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"aitop\",\"version\":\"0.2\"}}}\n{\"method\":\"initialized\"}\n{\"id\":3,\"method\":\"account/rateLimits/read\"}\n";
        if let Ok(bytes) = process::run(
            Command::new("codex").arg("app-server"),
            input,
            Duration::from_secs(8),
            cancel,
            Some(3),
        ) {
            for line in bytes.split(|&b| b == b'\n') {
                if let Ok(v) = serde_json::from_slice::<Value>(line) {
                    if v.get("id").and_then(Value::as_u64) == Some(3) {
                        return rpc_usage(&v, now);
                    }
                }
            }
        }
        return QuotaSnapshot::unavailable(
            Provider::Codex,
            Availability::Error,
            "App Server quota probe failed; check installed Codex version and sign-in",
            now,
        );
    }
    let auth = read_json(&paths.home.join(".codex/auth.json"));
    let Some(token) = auth
        .as_ref()
        .and_then(|v| v.pointer("/tokens/access_token"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return QuotaSnapshot::unavailable(
            Provider::Codex,
            Availability::AuthRequired,
            "Sign in through Codex. API-key-only accounts do not provide subscription quotas.",
            now,
        );
    };
    let mut headers = vec![("Authorization", format!("Bearer {token}"))];
    if let Some(account) = auth
        .as_ref()
        .and_then(|v| v.pointer("/tokens/account_id"))
        .and_then(Value::as_str)
    {
        headers.push(("ChatGPT-Account-Id", account.to_string()));
    }
    match request(
        Provider::Codex,
        "https://chatgpt.com/backend-api/wham/usage",
        headers,
        now,
    ) {
        Ok(v) => api_usage(&v, now),
        Err(q) => *q,
    }
}
