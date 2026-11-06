use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Command;
use chrono::{DateTime, Local, Utc};
use serde_json::Value;
use walkdir::WalkDir;
use crate::models::{AgentSummary, QuotaBar, TokenCounts, UsageGauges};
use crate::parser::format_reset_iso;

fn fetch_live_claude_oauth_usage() -> Option<UsageGauges> {
    let sec_out = Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
        .output()
        .ok()?;
    if !sec_out.status.success() {
        return None;
    }

    let raw_creds = String::from_utf8_lossy(&sec_out.stdout);
    let creds_json: Value = serde_json::from_str(raw_creds.trim()).ok()?;
    let access_token = creds_json
        .get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|t| t.as_str())?;

    let curl_out = Command::new("curl")
        .args([
            "-s",
            "-m",
            "4",
            "-H",
            &format!("Authorization: Bearer {}", access_token),
            "-H",
            "anthropic-beta: oauth-2025-04-20",
            "-H",
            "User-Agent: claude-code/2.0.0",
            "-H",
            "Accept: application/json",
            "https://api.anthropic.com/api/oauth/usage",
        ])
        .output()
        .ok()?;

    if !curl_out.status.success() {
        return None;
    }

    let usage_json: Value = serde_json::from_slice(&curl_out.stdout).ok()?;
    let five_h = usage_json.get("five_hour")?;
    let seven_d = usage_json.get("seven_day")?;

    let five_pct = five_h
        .get("utilization")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .round()
        .clamp(0.0, 100.0) as u16;
    let five_reset = five_h
        .get("resets_at")
        .and_then(|v| v.as_str())
        .map(format_reset_iso)
        .unwrap_or_else(|| "active".to_string());

    let week_pct = seven_d
        .get("utilization")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .round()
        .clamp(0.0, 100.0) as u16;
    let week_reset = seven_d
        .get("resets_at")
        .and_then(|v| v.as_str())
        .map(format_reset_iso)
        .unwrap_or_else(|| "active".to_string());

    let mut extra_bars = Vec::new();
    if let Some(sonnet) = usage_json.get("seven_day_sonnet").filter(|v| !v.is_null()) {
        if let Some(util) = sonnet.get("utilization").and_then(|v| v.as_f64()) {
            let pct = util.round().clamp(0.0, 100.0) as u16;
            let rst = sonnet
                .get("resets_at")
                .and_then(|v| v.as_str())
                .map(format_reset_iso)
                .unwrap_or_default();
            extra_bars.push(QuotaBar {
                pct,
                label: format!("Sonnet (7d): {}% left · {}", 100u16.saturating_sub(pct), rst),
            });
        }
    }
    if let Some(opus) = usage_json.get("seven_day_opus").filter(|v| !v.is_null()) {
        if let Some(util) = opus.get("utilization").and_then(|v| v.as_f64()) {
            let pct = util.round().clamp(0.0, 100.0) as u16;
            let rst = opus
                .get("resets_at")
                .and_then(|v| v.as_str())
                .map(format_reset_iso)
                .unwrap_or_default();
            extra_bars.push(QuotaBar {
                pct,
                label: format!("Opus (7d): {}% left · {}", 100u16.saturating_sub(pct), rst),
            });
        }
    }

    Some(UsageGauges {
        session_usage_pct: five_pct,
        session_usage_label: format!("Session (5h): {}% left · {}", 100u16.saturating_sub(five_pct), five_reset),
        weekly_usage_pct: week_pct,
        weekly_usage_label: format!("Weekly (All): {}% left · {}", 100u16.saturating_sub(week_pct), week_reset),
        extra_bars,
    })
}

pub fn parse_claude_usage(home: &PathBuf) -> AgentSummary {
    let mut summary = AgentSummary {
        name: "Claude Code".to_string(),
        provider: "Anthropic".to_string(),
        active: false,
        ..Default::default()
    };

    let claude_dir = home.join(".claude");
    if !claude_dir.exists() {
        return summary;
    }

    summary.active = true;
    let stats_path = claude_dir.join("stats-cache.json");

    // 1. 전체 통계
    if let Ok(file) = File::open(&stats_path) {
        let reader = BufReader::new(file);
        if let Ok(json) = serde_json::from_reader::<_, Value>(reader) {
            if let Some(sessions) = json.get("totalSessions").and_then(|v| v.as_u64()) {
                summary.session_count = sessions as usize;
            }

            if let Some(model_usage) = json.get("modelUsage").and_then(|v| v.as_object()) {
                for (model_name, usage_val) in model_usage {
                    let input = usage_val.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let output = usage_val.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cache_read = usage_val.get("cacheReadInputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cache_create = usage_val.get("cacheCreationInputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let mut cost = usage_val.get("costUSD").and_then(|v| v.as_f64()).unwrap_or(0.0);

                    if cost == 0.0 && (input > 0 || output > 0 || cache_read > 0) {
                        if model_name.contains("haiku") {
                            cost = (input as f64 * 0.8 + output as f64 * 4.0 + cache_read as f64 * 0.08 + cache_create as f64 * 1.0) / 1_000_000.0;
                        } else if model_name.contains("opus") {
                            cost = (input as f64 * 15.0 + output as f64 * 75.0 + cache_read as f64 * 1.5 + cache_create as f64 * 18.75) / 1_000_000.0;
                        } else {
                            cost = (input as f64 * 3.0 + output as f64 * 15.0 + cache_read as f64 * 0.3 + cache_create as f64 * 3.75) / 1_000_000.0;
                        }
                    }

                    let total = input + output + cache_read + cache_create;
                    let counts = TokenCounts {
                        input_tokens: input,
                        output_tokens: output,
                        cache_read_tokens: cache_read,
                        cache_creation_tokens: cache_create,
                        total_tokens: total,
                        estimated_cost_usd: cost,
                    };

                    summary.tokens.input_tokens += input;
                    summary.tokens.output_tokens += output;
                    summary.tokens.cache_read_tokens += cache_read;
                    summary.tokens.cache_creation_tokens += cache_create;
                    summary.tokens.total_tokens += total;
                    summary.tokens.estimated_cost_usd += cost;

                    summary.model_breakdowns.insert(model_name.clone(), counts);
                }
            }
        }
    }

    // 2. Claude 실제 /usage 한도 조회 (OAuth API 우선, 실패 시 로컬 폴백)
    if let Some(live_gauges) = fetch_live_claude_oauth_usage() {
        summary.usage = live_gauges;
        return summary;
    }

    let projects_dir = claude_dir.join("projects");
    if projects_dir.exists() {
        let now_ms = Utc::now().timestamp_millis();
        let five_hours_ago = now_ms - (5 * 3600 * 1000);
        let seven_days_ago = now_ms - (7 * 24 * 3600 * 1000);

        let mut five_h_cost = 0.0;
        let mut weekly_cost = 0.0;
        let mut oldest_5h_ts: Option<i64> = None;

        for entry in WalkDir::new(&projects_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("jsonl"))
        {
            if let Ok(file) = File::open(entry.path()) {
                let reader = BufReader::new(file);
                for line in reader.lines().filter_map(|l| l.ok()) {
                    if !line.contains("\"cost-state\"") {
                        continue;
                    }
                    if let Ok(val) = serde_json::from_str::<Value>(&line) {
                        let start_time = val.get("startTime").and_then(|v| v.as_i64()).unwrap_or(0);
                        let cost = val.get("totalCostUSD").and_then(|v| v.as_f64()).unwrap_or(0.0);

                        if start_time >= seven_days_ago {
                            weekly_cost += cost;
                        }

                        if start_time >= five_hours_ago {
                            five_h_cost += cost;
                            oldest_5h_ts = match oldest_5h_ts {
                                Some(prev) => Some(prev.min(start_time)),
                                None => Some(start_time),
                            };
                        }
                    }
                }
            }
        }

        let five_cap = 8.0;
        let week_cap = 50.0;
        let five_pct = ((five_h_cost / five_cap) * 100.0).clamp(0.0, 100.0) as u16;
        let week_pct = ((weekly_cost / week_cap) * 100.0).clamp(0.0, 100.0) as u16;

        let reset_str = if let Some(first_ts) = oldest_5h_ts {
            let reset_time_ms = first_ts + (5 * 3600 * 1000);
            if let Some(dt) = DateTime::from_timestamp_millis(reset_time_ms) {
                let local_dt: DateTime<Local> = DateTime::from(dt);
                format!("resets {}", local_dt.format("%H:%M"))
            } else {
                "active".to_string()
            }
        } else {
            "full available".to_string()
        };

        summary.usage = UsageGauges {
            session_usage_pct: five_pct,
            session_usage_label: format!("Session (5h): {}% used · {}", five_pct, reset_str),
            weekly_usage_pct: week_pct,
            weekly_usage_label: format!("Weekly: {}% used · ${:.2}", week_pct, weekly_cost),
            extra_bars: Vec::new(),
        };
    }

    summary
}
