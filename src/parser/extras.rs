use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use chrono::{Datelike, Local, TimeZone};
use serde_json::Value;
use walkdir::WalkDir;

use crate::models::{AgentSummary, TokenCounts, UsageGauges};
use crate::parser::{format_countdown, format_reset_iso};

fn monthly_reset_countdown() -> String {
    let now = Local::now();
    let (next_year, next_month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    if let Some(next_dt) = Local
        .with_ymd_and_hms(next_year, next_month, 1, 0, 0, 0)
        .single()
    {
        let diff = (next_dt.timestamp() - now.timestamp()).max(0);
        format_countdown(diff)
    } else {
        "30d 0h 0m".to_string()
    }
}

pub fn parse_cursor_usage(home: &PathBuf) -> AgentSummary {
    let ws_dir = home.join("Library/Application Support/Cursor/User/workspaceStorage");
    let mut session_count = 0usize;
    if ws_dir.exists() {
        if let Ok(entries) = fs::read_dir(&ws_dir) {
            session_count = entries.filter_map(Result::ok).count();
        }
    }

    let db_path = home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb");
    let mut used_pct = 0u16;
    let mut reset_str = monthly_reset_countdown();
    let mut status_suffix = String::new();

    if db_path.exists() {
        if let Ok(out) = Command::new("sqlite3")
            .arg(&db_path)
            .arg("SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken' LIMIT 1;")
            .stdin(std::process::Stdio::null())
            .output()
        {
            let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !token.is_empty() {
                if let Ok(curl_out) = Command::new("curl")
                    .args([
                        "-s",
                        "-m",
                        "3",
                        "-H",
                        &format!("Authorization: Bearer {}", token),
                        "-H",
                        "Accept: application/json",
                        "https://api2.cursor.sh/auth/full_stripe_profile",
                    ])
                    .stdin(std::process::Stdio::null())
                    .output()
                {
                    let raw = String::from_utf8_lossy(&curl_out.stdout);
                    if let Ok(json) = serde_json::from_str::<Value>(&raw) {
                        if let Some(used) = json.get("numRequests").and_then(|v| v.as_f64()) {
                            let max_req = json
                                .get("maxRequestUsage")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(500.0)
                                .max(1.0);
                            used_pct = ((used / max_req) * 100.0).round().clamp(0.0, 100.0) as u16;
                        }
                    }
                }
            } else {
                status_suffix = " (No Auth)".to_string();
            }
        }
    } else {
        reset_str = "-".to_string();
        status_suffix = " (Not Installed)".to_string();
    }

    let rem_pct = 100u16.saturating_sub(used_pct);
    let usage = UsageGauges {
        session_usage_pct: used_pct,
        session_usage_label: format!(
            "Fast Requests{}: {}% left · {}",
            status_suffix, rem_pct, reset_str
        ),
        weekly_usage_pct: 0,
        weekly_usage_label: String::new(),
        extra_bars: Vec::new(),
    };

    AgentSummary {
        name: "Cursor".to_string(),
        provider: "Anysphere".to_string(),
        active: session_count > 0,
        tokens: TokenCounts::default(),
        session_count,
        usage,
        model_breakdowns: HashMap::new(),
    }
}

pub fn parse_copilot_usage(home: &PathBuf) -> AgentSummary {
    let hosts_path = home.join(".config/github-copilot/hosts.json");
    let apps_path = home.join(".config/github-copilot/apps.json");

    let mut oauth_token: Option<String> = None;
    for path in [&hosts_path, &apps_path] {
        if path.exists() {
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(json) = serde_json::from_str::<Value>(&content) {
                    if let Some(obj) = json.as_object() {
                        for (_k, v) in obj {
                            if let Some(tok) = v.get("oauth_token").and_then(|t| t.as_str()) {
                                oauth_token = Some(tok.to_string());
                                break;
                            }
                        }
                    }
                }
            }
        }
        if oauth_token.is_some() {
            break;
        }
    }

    let mut used_pct = 0u16;
    let mut reset_str = monthly_reset_countdown();
    let mut active = false;

    if let Some(token) = oauth_token {
        active = true;
        if let Ok(curl_out) = Command::new("curl")
            .args([
                "-s",
                "-m",
                "3",
                "-H",
                &format!("Authorization: token {}", token),
                "-H",
                "Accept: application/json",
                "-H",
                "User-Agent: GitHubCopilotChat/0.24.0",
                "https://api.github.com/copilot_internal/user",
            ])
            .stdin(std::process::Stdio::null())
            .output()
        {
            let raw = String::from_utf8_lossy(&curl_out.stdout);
            if let Ok(json) = serde_json::from_str::<Value>(&raw) {
                if let Some(reset_iso) = json.get("quota_reset_date_utc").and_then(|v| v.as_str()) {
                    let formatted = format_reset_iso(reset_iso);
                    if formatted != "-" {
                        reset_str = formatted;
                    }
                }
                if let Some(prem) = json
                    .get("quota_snapshots")
                    .and_then(|q| q.get("premium_interactions"))
                {
                    if let Some(rem_f) = prem.get("percent_remaining").and_then(|v| v.as_f64()) {
                        let rem = rem_f.round().clamp(0.0, 100.0) as u16;
                        used_pct = 100u16.saturating_sub(rem);
                    }
                }
            }
        }
    } else {
        reset_str = "-".to_string();
    }

    let rem_pct = 100u16.saturating_sub(used_pct);
    let label_prefix = if active {
        "Premium Quota"
    } else {
        "Premium Quota (No Auth)"
    };

    let usage = UsageGauges {
        session_usage_pct: used_pct,
        session_usage_label: format!("{}: {}% left · {}", label_prefix, rem_pct, reset_str),
        weekly_usage_pct: 0,
        weekly_usage_label: String::new(),
        extra_bars: Vec::new(),
    };

    AgentSummary {
        name: "GitHub Copilot".to_string(),
        provider: "GitHub".to_string(),
        active,
        tokens: TokenCounts::default(),
        session_count: if active { 1 } else { 0 },
        usage,
        model_breakdowns: HashMap::new(),
    }
}

pub fn parse_grok_usage(home: &PathBuf) -> AgentSummary {
    let grok_dir = home.join(".grok");
    let mut session_count = 0usize;
    let mut total_tokens = 0u64;

    if grok_dir.exists() {
        for entry in WalkDir::new(&grok_dir).into_iter().filter_map(Result::ok) {
            if entry.path().extension().and_then(|s| s.to_str()) == Some("jsonl") {
                session_count += 1;
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    total_tokens += (content.len() as u64) / 4;
                }
            }
        }
    }

    let has_env = std::env::var("XAI_API_KEY").is_ok() || grok_dir.exists();
    let reset_str = if has_env { "2h 0m" } else { "-" };
    let label_prefix = if has_env {
        "Rate Limit (2h)"
    } else {
        "Rate Limit (No Auth)"
    };

    let usage = UsageGauges {
        session_usage_pct: 0,
        session_usage_label: format!("{}: 100% left · {}", label_prefix, reset_str),
        weekly_usage_pct: 0,
        weekly_usage_label: String::new(),
        extra_bars: Vec::new(),
    };

    AgentSummary {
        name: "Grok".to_string(),
        provider: "xAI".to_string(),
        active: has_env,
        tokens: TokenCounts {
            total_tokens,
            estimated_cost_usd: (total_tokens as f64 / 1_000_000.0) * 5.0,
            ..Default::default()
        },
        session_count,
        usage,
        model_breakdowns: HashMap::new(),
    }
}
