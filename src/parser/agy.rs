use super::{epoch, measured, FetchOptions};
use crate::{
    config::{private_dir, Paths},
    models::{Availability, Provider, QuotaSnapshot, QuotaWindow},
    process,
};
use serde_json::Value;
use std::{process::Command, sync::atomic::AtomicBool, time::Duration};

/// Read-only slash reports were introduced in agy 1.1.11. Older versions can
/// interpret /usage as a model prompt, so never probe them or an unknown version.
pub fn supported_version(text: &str) -> bool {
    text.split_whitespace()
        .filter_map(|word| {
            let mut parts = word.trim_start_matches('v').split('.');
            let version = (
                parts.next()?.parse::<u64>().ok()?,
                parts.next()?.parse::<u64>().ok()?,
                parts.next()?.parse::<u64>().ok()?,
            );
            parts.next().is_none().then_some(version)
        })
        .any(|v| v >= (1, 1, 11))
}

pub fn usage(value: &Value, now: i64) -> QuotaSnapshot {
    let mut windows = Vec::new();
    let command = value.get("command");
    let valid = command.and_then(|c| c.get("name")).and_then(Value::as_str) == Some("usage")
        && value
            .get("num_turns")
            .and_then(Value::as_u64)
            .is_none_or(|n| n == 0)
        && value
            .get("status")
            .and_then(Value::as_str)
            .is_none_or(|s| s == "SUCCESS");
    if valid {
        if let Some(groups) = value
            .pointer("/command/data/groups")
            .and_then(Value::as_array)
        {
            for group in groups {
                let Some(name) = group.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let short = if name.contains("Argon") {
                    "Argon"
                } else if name.contains("Claude") && name.contains("GPT") {
                    "Claude/GPT"
                } else if name.contains("Gemini") {
                    "Gemini"
                } else {
                    name
                };
                if let Some(buckets) = group.get("buckets").and_then(Value::as_array) {
                    for bucket in buckets {
                        if bucket.get("enabled").and_then(Value::as_bool) == Some(false) {
                            continue;
                        }
                        let (Some(id), Some(fraction)) = (
                            bucket.get("id").and_then(Value::as_str),
                            bucket.get("remaining_fraction").and_then(Value::as_f64),
                        ) else {
                            continue;
                        };
                        if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
                            continue;
                        }
                        let window = bucket
                            .get("window")
                            .and_then(Value::as_str)
                            .unwrap_or("quota");
                        let (label, minutes) = match window {
                            "weekly" => ("7d", Some(10080)),
                            "5h" => ("5h", Some(300)),
                            other => (other, None),
                        };
                        if let Some(q) = QuotaWindow::new(
                            id,
                            &format!("{short} {label}"),
                            100.0 * (1.0 - fraction),
                            bucket.get("reset_time").and_then(epoch),
                            minutes,
                        ) {
                            windows.push(q);
                        }
                    }
                }
            }
        }
    }
    let mut q = measured(
        Provider::Agy,
        "agy command report (experimental)",
        windows,
        now,
    );
    if q.availability == Availability::Available {
        q.message = "Ambient CLI account only; not attributed to another app or account".into();
    }
    q
}

pub fn fetch(paths: &Paths, options: FetchOptions, cancel: &AtomicBool, now: i64) -> QuotaSnapshot {
    // The provider toggle is the opt-in. Keep --agy-cli accepted for old launch commands.
    if options.offline {
        return QuotaSnapshot::unavailable(
            Provider::Agy,
            Availability::NoData,
            "Offline: no AGY usage query requested",
            now,
        );
    }
    let local = paths.home.join(".local/bin/agy");
    let bin = if local.exists() { local } else { "agy".into() };
    let version = process::run(
        Command::new(&bin)
            .arg("--version")
            .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true"),
        &[],
        Duration::from_secs(5),
        cancel,
        None,
    );
    let version =
        match version {
            Ok(v) => v,
            Err(error) => return QuotaSnapshot::unavailable(
                Provider::Agy,
                Availability::Error,
                match error.to_string().as_str() {
                    "Provider command timed out" => {
                        "AGY version check timed out after 5s; retry or check the CLI installation"
                    }
                    "Collection cancelled" => "AGY version check cancelled",
                    _ => "Cannot run AGY version check; check the CLI installation and permissions",
                },
                now,
            ),
        };
    if !supported_version(&String::from_utf8_lossy(&version)) {
        return QuotaSnapshot::unavailable(Provider::Agy,Availability::Unsupported,"agy 1.1.11+ is required for a read-only usage command; unknown versions are not probed",now);
    }
    let probe = paths
        .data
        .join(format!("agy-probe-{}-{now}", std::process::id()));
    if private_dir(&probe).is_err() {
        return QuotaSnapshot::unavailable(
            Provider::Agy,
            Availability::Error,
            "Cannot create private CLI probe directory",
            now,
        );
    }
    let result = process::run(
        Command::new(bin)
            .args([
                "-p",
                "/usage",
                "--output-format",
                "json",
                "--print-timeout",
                "20s",
            ])
            .current_dir(&probe)
            .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
            .env("BROWSER", "false"),
        &[],
        // Allow startup/shutdown outside the CLI's own 20-second deadline.
        Duration::from_secs(25),
        cancel,
        None,
    );
    let _ = std::fs::remove_dir_all(probe);
    match result {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(v) => usage(&v, now),
            Err(_) => QuotaSnapshot::unavailable(
                Provider::Agy,
                Availability::Unsupported,
                "Installed agy report schema is not supported; no quota inferred",
                now,
            ),
        },
        Err(error) => QuotaSnapshot::unavailable(
            Provider::Agy,
            Availability::Error,
            match error.to_string().as_str() {
                "Provider command timed out" => "AGY usage query timed out after 25s; retry or check connectivity",
                "Collection cancelled" => "AGY usage query cancelled",
                "Provider command is not installed or cannot start" => "Cannot start AGY CLI; check installation and permissions",
                "Provider command failed; run its CLI to check sign-in" => "AGY usage command exited unsuccessfully; run agy -p /usage to check sign-in or connectivity",
                _ => "Cannot read AGY usage command output; retry or check the CLI installation",
            },
            now,
        ),
    }
}
