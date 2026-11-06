use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Command;
use std::time::SystemTime;
use walkdir::WalkDir;
use serde_json::Value;
use crate::models::{AgentSummary, TokenCounts, UsageGauges};
use crate::parser::format_reset_epoch_secs;

fn fetch_live_codex_usage(home: &PathBuf) -> Option<UsageGauges> {
    let auth_path = home.join(".codex").join("auth.json");
    let file = File::open(auth_path).ok()?;
    let auth_json: Value = serde_json::from_reader(BufReader::new(file)).ok()?;
    let tokens = auth_json.get("tokens")?;
    let access_token = tokens.get("access_token").and_then(|v| v.as_str())?;
    let account_id = tokens.get("account_id").and_then(|v| v.as_str()).unwrap_or("");

    let curl_out = Command::new("curl")
        .args([
            "-s",
            "-m",
            "4",
            "-H",
            &format!("Authorization: Bearer {}", access_token),
            "-H",
            &format!("ChatGPT-Account-Id: {}", account_id),
            "-H",
            "User-Agent: codex-cli",
            "-H",
            "Accept: application/json",
            "https://chatgpt.com/backend-api/wham/usage",
        ])
        .output()
        .ok()?;

    if !curl_out.status.success() {
        return None;
    }

    let usage_json: Value = serde_json::from_slice(&curl_out.stdout).ok()?;
    let rl = usage_json.get("rate_limit")?;
    let primary = rl.get("primary_window")?;
    let secondary = rl.get("secondary_window")?;

    let p_pct = primary
        .get("used_percent")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .round()
        .clamp(0.0, 100.0) as u16;
    let p_reset = primary
        .get("reset_at")
        .and_then(|v| v.as_i64())
        .map(format_reset_epoch_secs)
        .unwrap_or_else(|| "active".to_string());

    let s_pct = secondary
        .get("used_percent")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .round()
        .clamp(0.0, 100.0) as u16;
    let s_reset = secondary
        .get("reset_at")
        .and_then(|v| v.as_i64())
        .map(format_reset_epoch_secs)
        .unwrap_or_else(|| "active".to_string());

    Some(UsageGauges {
        session_usage_pct: p_pct,
        session_usage_label: format!("Session (5h): {}% left · {}", 100u16.saturating_sub(p_pct), p_reset),
        weekly_usage_pct: s_pct,
        weekly_usage_label: format!("Weekly (7d): {}% left · {}", 100u16.saturating_sub(s_pct), s_reset),
        extra_bars: Vec::new(),
    })
}

pub fn parse_codex_usage(home: &PathBuf) -> AgentSummary {
    let mut summary = AgentSummary {
        name: "Codex CLI".to_string(),
        provider: "OpenAI".to_string(),
        active: false,
        ..Default::default()
    };

    let sessions_dir = home.join(".codex").join("sessions");
    if !sessions_dir.exists() {
        return summary;
    }

    summary.active = true;

    let now = SystemTime::now();
    let seven_days_secs = 7 * 24 * 3600;

    let mut session_count = 0;
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cached = 0u64;

    let mut week_input = 0u64;
    let mut week_output = 0u64;
    let mut week_cached = 0u64;

    let mut latest_file_mtime = SystemTime::UNIX_EPOCH;
    let mut latest_file_path: Option<PathBuf> = None;

    for entry in WalkDir::new(&sessions_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("jsonl"))
    {
        session_count += 1;
        let file_age_secs = entry.metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|mtime| now.duration_since(mtime).ok())
            .map(|d| d.as_secs())
            .unwrap_or(u64::MAX);

        let is_recent_week = file_age_secs <= seven_days_secs;

        if let Ok(meta) = entry.metadata() {
            if let Ok(mtime) = meta.modified() {
                if mtime > latest_file_mtime {
                    latest_file_mtime = mtime;
                    latest_file_path = Some(entry.path().to_path_buf());
                }
            }
        }

        if let Ok(file) = File::open(entry.path()) {
            let reader = BufReader::new(file);
            let mut file_input = 0u64;
            let mut file_output = 0u64;
            let mut file_cached = 0u64;

            for line in reader.lines().filter_map(|l| l.ok()) {
                if !line.contains("token_usage_record") && !line.contains("\"token_count\"") {
                    continue;
                }
                if let Ok(val) = serde_json::from_str::<Value>(&line) {
                    let row_type = val.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if row_type == "token_usage_record" {
                        if let Some(usage) = val.get("payload").and_then(|p| p.get("usage")) {
                            let input = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let output = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let cached = usage.get("cached_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);

                            file_input += input;
                            file_output += output;
                            file_cached += cached;
                        }
                    } else if row_type == "event_msg" {
                        let payload = val.get("payload");
                        if payload.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("token_count") {
                            if let Some(tot) = payload.and_then(|p| p.get("info")).and_then(|i| i.get("total_token_usage")) {
                                file_input = tot.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(file_input);
                                file_output = tot.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(file_output);
                                file_cached = tot.get("cached_input_tokens").and_then(|v| v.as_u64()).unwrap_or(file_cached);
                            }
                        }
                    }
                }
            }

            total_input += file_input;
            total_output += file_output;
            total_cached += file_cached;

            if is_recent_week {
                week_input += file_input;
                week_output += file_output;
                week_cached += file_cached;
            }
        }
    }

    let total = total_input + total_output + total_cached;
    let est_cost = (total_input as f64 * 2.5 + total_output as f64 * 10.0 + total_cached as f64 * 0.25) / 1_000_000.0;
    let week_cost = (week_input as f64 * 2.5 + week_output as f64 * 10.0 + week_cached as f64 * 0.25) / 1_000_000.0;
    let week_tokens = week_input + week_output + week_cached;

    summary.session_count = session_count;
    summary.tokens = TokenCounts {
        input_tokens: total_input,
        output_tokens: total_output,
        cache_read_tokens: total_cached,
        cache_creation_tokens: 0,
        total_tokens: total,
        estimated_cost_usd: est_cost,
    };

    // 1순위: 실시간 /wham/usage API 조회
    if let Some(live_gauges) = fetch_live_codex_usage(home) {
        summary.usage = live_gauges;
        return summary;
    }

    // 2순위: 가장 최근 세션 파일의 rate_limits 스냅샷 조회
    let mut last_rate_limits: Option<Value> = None;
    let mut current_session_tokens = 0u64;
    if let Some(path) = latest_file_path {
        if let Ok(file) = File::open(path) {
            let reader = BufReader::new(file);
            for line in reader.lines().filter_map(|l| l.ok()) {
                if line.contains("rate_limits") || line.contains("token_usage_record") {
                    if let Ok(val) = serde_json::from_str::<Value>(&line) {
                        if let Some(rl) = val.get("payload").and_then(|p| p.get("rate_limits")).filter(|v| !v.is_null()) {
                            last_rate_limits = Some(rl.clone());
                        }
                        if let Some(tot) = val.get("payload").and_then(|p| p.get("info")).and_then(|i| i.get("total_token_usage")) {
                            current_session_tokens = tot.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        }
                    }
                }
            }
        }
    }

    if let Some(rl) = last_rate_limits {
        let p_pct = rl.get("primary").and_then(|p| p.get("used_percent")).and_then(|v| v.as_f64()).unwrap_or(0.0).round().clamp(0.0, 100.0) as u16;
        let p_reset = rl.get("primary").and_then(|p| p.get("resets_at")).and_then(|v| v.as_i64()).map(format_reset_epoch_secs).unwrap_or_else(|| "active".to_string());
        let s_pct = rl.get("secondary").and_then(|s| s.get("used_percent")).and_then(|v| v.as_f64()).unwrap_or(0.0).round().clamp(0.0, 100.0) as u16;
        let s_reset = rl.get("secondary").and_then(|s| s.get("resets_at")).and_then(|v| v.as_i64()).map(format_reset_epoch_secs).unwrap_or_else(|| "active".to_string());

        summary.usage = UsageGauges {
            session_usage_pct: p_pct,
            session_usage_label: format!("Session (5h): {}% left · {}", 100u16.saturating_sub(p_pct), p_reset),
            weekly_usage_pct: s_pct,
            weekly_usage_label: format!("Weekly (7d): {}% left · {}", 100u16.saturating_sub(s_pct), s_reset),
            extra_bars: Vec::new(),
        };
        return summary;
    }

    let session_cap = 200_000f64;
    let session_pct = ((current_session_tokens as f64 / session_cap) * 100.0).clamp(0.0, 100.0) as u16;
    let week_pct = ((week_cost / 50.0) * 100.0).clamp(0.0, 100.0) as u16;

    summary.usage = UsageGauges {
        session_usage_pct: session_pct,
        session_usage_label: format!("Session: {}% used · {:.1}k tokens", session_pct, current_session_tokens as f64 / 1000.0),
        weekly_usage_pct: week_pct,
        weekly_usage_label: format!("Weekly: {}% used · ${:.2} ({:.1}M tok)", week_pct, week_cost, week_tokens as f64 / 1_000_000.0),
        extra_bars: Vec::new(),
    };

    summary
}
