use super::{epoch, measured, read_json, request};
use crate::{
    config::Paths,
    models::{Availability, Provider, QuotaSnapshot, QuotaWindow},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

pub fn cursor_usage(value: &Value, now: i64) -> QuotaSnapshot {
    let mut windows = Vec::new();
    if let (Some(used), Some(limit)) = (
        value.get("numRequests").and_then(Value::as_f64),
        value.get("maxRequestUsage").and_then(Value::as_f64),
    ) {
        if limit.is_finite() && limit > 0.0 && used.is_finite() && used >= 0.0 {
            if let Some(q) = QuotaWindow::new(
                "requests",
                "Requests",
                (used / limit * 100.0).min(100.0),
                value.get("resetAt").and_then(epoch),
                None,
            ) {
                windows.push(q);
            }
        }
    }
    measured(
        Provider::Cursor,
        "cursor profile (experimental)",
        windows,
        now,
    )
}
pub fn cursor(paths: &Paths, now: i64) -> QuotaSnapshot {
    let mut candidates = vec![paths
        .home
        .join("Library/Application Support/Cursor/User/globalStorage/state.vscdb")];
    candidates.push(
        paths
            .home
            .join(".config/Cursor/User/globalStorage/state.vscdb"),
    );
    let token = candidates.iter().find_map(|path| {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        db.busy_timeout(std::time::Duration::from_secs(1)).ok()?;
        db.query_row(
            "SELECT value FROM ItemTable WHERE key='cursorAuth/accessToken' LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
    });
    let Some(token) = token else {
        return QuotaSnapshot::unavailable(
            Provider::Cursor,
            Availability::AuthRequired,
            "No readable Cursor login. Existing database is opened read-only.",
            now,
        );
    };
    match request(
        Provider::Cursor,
        "https://api2.cursor.sh/auth/full_stripe_profile",
        vec![("Authorization", format!("Bearer {token}"))],
        now,
    ) {
        Ok(v) => cursor_usage(&v, now),
        Err(q) => *q,
    }
}
pub fn copilot_usage(value: &Value, now: i64) -> QuotaSnapshot {
    let mut windows = Vec::new();
    if let Some(pct) = value
        .pointer("/quota_snapshots/premium_interactions/percent_remaining")
        .and_then(Value::as_f64)
    {
        if let Some(q) = QuotaWindow::new(
            "premium",
            "Premium",
            100.0 - pct,
            value.get("quota_reset_date_utc").and_then(epoch),
            None,
        ) {
            windows.push(q);
        }
    }
    measured(
        Provider::Copilot,
        "copilot internal API (experimental)",
        windows,
        now,
    )
}
pub fn copilot(paths: &Paths, now: i64) -> QuotaSnapshot {
    let token = ["hosts.json", "apps.json"].iter().find_map(|name| {
        read_json(&paths.home.join(".config/github-copilot").join(name)).and_then(|v| {
            v.as_object().and_then(|o| {
                o.values().find_map(|v| {
                    v.get("oauth_token")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
            })
        })
    });
    let Some(token) = token else {
        return QuotaSnapshot::unavailable(
            Provider::Copilot,
            Availability::AuthRequired,
            "No readable Copilot CLI login found",
            now,
        );
    };
    match request(
        Provider::Copilot,
        "https://api.github.com/copilot_internal/user",
        vec![("Authorization", format!("token {token}"))],
        now,
    ) {
        Ok(v) => copilot_usage(&v, now),
        Err(q) => *q,
    }
}
