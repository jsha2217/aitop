use super::{epoch, measured, read_json, request, FetchOptions};
use crate::{
    config::Paths,
    models::{Availability, Provider, QuotaSnapshot, QuotaWindow},
    process,
};
use serde_json::Value;
use std::{process::Command, sync::atomic::AtomicBool, time::Duration};

fn access_token(value: &Value) -> Option<&str> {
    value
        .pointer("/claudeAiOauth/accessToken")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

pub fn usage(value: &Value, now: i64, source: &str) -> QuotaSnapshot {
    let mut windows = Vec::new();
    for (key, label, minutes) in [
        ("five_hour", "5h", 300),
        ("seven_day", "7d", 10080),
        ("seven_day_sonnet", "Sonnet 7d", 10080),
        ("seven_day_opus", "Opus 7d", 10080),
    ] {
        if let Some(v) = value.get(key) {
            if let Some(q) = v
                .get("utilization")
                .and_then(Value::as_f64)
                .and_then(|pct| {
                    QuotaWindow::new(
                        key,
                        label,
                        pct,
                        v.get("resets_at").and_then(epoch),
                        Some(minutes),
                    )
                })
            {
                windows.push(q);
            }
        }
    }
    measured(Provider::Claude, source, windows, now)
}

pub fn statusline(value: &Value, now: i64) -> QuotaSnapshot {
    let mut windows = Vec::new();
    for (key, label, minutes) in [("five_hour", "5h", 300), ("seven_day", "7d", 10080)] {
        if let Some(v) = value.get("rate_limits").and_then(|v| v.get(key)) {
            if let Some(q) = v
                .get("used_percentage")
                .and_then(Value::as_f64)
                .and_then(|pct| {
                    QuotaWindow::new(
                        key,
                        label,
                        pct,
                        v.get("resets_at").and_then(epoch),
                        Some(minutes),
                    )
                })
            {
                windows.push(q);
            }
        }
    }
    let mut q = measured(Provider::Claude, "statusline", windows, now);
    q.message = "Captured from Claude statusline; account attribution unknown".into();
    q
}

pub fn fetch(paths: &Paths, options: FetchOptions, cancel: &AtomicBool, now: i64) -> QuotaSnapshot {
    let mut credentials = read_json(&paths.home.join(".claude/.credentials.json"));
    if credentials.as_ref().and_then(access_token).is_none() && options.keychain {
        match process::run(
            Command::new("security").args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ]),
            &[],
            Duration::from_secs(4),
            cancel,
            None,
        ) {
            Ok(bytes) => credentials = serde_json::from_slice(&bytes).ok(),
            Err(error) => {
                return QuotaSnapshot::unavailable(
                    Provider::Claude,
                    Availability::AuthRequired,
                    if error.to_string() == "Provider command timed out" {
                        "Claude Keychain access timed out; check the macOS permission dialog and retry"
                    } else {
                        "Cannot read Claude credentials from Keychain; check macOS access or Claude Code sign-in"
                    },
                    now,
                )
            }
        }
    }
    let Some(token) = credentials.as_ref().and_then(access_token) else {
        return QuotaSnapshot::unavailable(
            Provider::Claude,
            Availability::AuthRequired,
            if options.keychain {
                "No usable Claude OAuth credential; sign in through Claude Code or capture a statusline snapshot"
            } else {
                "Claude credentials unavailable: enable Claude Keychain in Settings or run aitop --keychain; a local credential file or captured statusline can also be used"
            },
            now,
        );
    };
    match request(
        Provider::Claude,
        "https://api.anthropic.com/api/oauth/usage",
        vec![
            ("Authorization", format!("Bearer {token}")),
            ("anthropic-beta", "oauth-2025-04-20".into()),
        ],
        now,
    ) {
        Ok(v) => usage(&v, now, "oauth-api (experimental)"),
        Err(q) => *q,
    }
}
