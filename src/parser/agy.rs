use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::SystemTime;
use chrono::{Local, NaiveDateTime, TimeZone};
use walkdir::WalkDir;
use serde_json::Value;
use crate::models::{AgentSummary, QuotaBar, TokenCounts, UsageGauges};
use crate::parser::{format_countdown, format_reset_iso};

fn format_agy_reset(raw_time: &str) -> String {
    let trimmed = raw_time.trim();
    if trimmed.contains('T') {
        let rfc = format_reset_iso(trimmed);
        if rfc != "-" {
            return rfc;
        }
    }
    let cleaned = trimmed
        .trim_end_matches("KST")
        .trim_end_matches("UTC")
        .trim();
    if let Ok(ndt) = NaiveDateTime::parse_from_str(cleaned, "%Y-%m-%d %H:%M") {
        if let Some(local_dt) = Local.from_local_datetime(&ndt).single() {
            let now = Local::now();
            let diff_secs = (local_dt.timestamp() - now.timestamp()).max(0);
            return format_countdown(diff_secs);
        }
    }
    if cleaned.is_empty() {
        "-".to_string()
    } else {
        cleaned.to_string()
    }
}

fn fetch_live_agy_usage(home: &PathBuf) -> Option<UsageGauges> {
    let local_bin = home.join(".local/bin/agy");
    let agy_bin = if local_bin.exists() {
        local_bin
    } else {
        PathBuf::from("agy")
    };

    let out = Command::new(agy_bin)
        .args(["-p", "/usage"])
        .stdin(std::process::Stdio::null())
        .process_group(0)
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut parsed_rows: Vec<(String, u16, String)> = Vec::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("Quota:") {
            continue;
        }

        // Find percentage token like "90%", "35%", "100%"
        let mut pct_val: Option<u16> = None;
        let mut pct_byte_idx: Option<(usize, usize)> = None;

        for (idx, _part) in trimmed.match_indices('%') {
            let prefix = &trimmed[..idx];
            if let Some(num_str) = prefix.split_whitespace().last() {
                if let Ok(n) = num_str.parse::<u16>() {
                    let start = idx - num_str.len();
                    pct_val = Some(n.min(100));
                    pct_byte_idx = Some((start, idx + 1));
                    break;
                }
            }
        }

        if let (Some(rem_pct), Some((p_start, p_end))) = (pct_val, pct_byte_idx) {
            let left_part = trimmed[..p_start].trim();
            let right_part = trimmed[p_end..].trim();

            let short_name = if left_part.contains("Gemini 4 Argon") {
                "Gemini 4 Argon (7d)".to_string()
            } else if left_part.contains("Gemini Models") && left_part.contains("Five Hour") {
                "Gemini Models (5h)".to_string()
            } else if left_part.contains("Gemini Models") && left_part.contains("Weekly") {
                "Gemini Models (7d)".to_string()
            } else if left_part.contains("Claude and GPT") && left_part.contains("Five Hour") {
                "Claude/GPT (5h)".to_string()
            } else if left_part.contains("Claude and GPT") && left_part.contains("Weekly") {
                "Claude/GPT (7d)".to_string()
            } else {
                left_part.split_whitespace().collect::<Vec<_>>().join(" ")
            };

            let reset_str = format_agy_reset(right_part);
            let used_pct = 100u16.saturating_sub(rem_pct);
            let label = format!("{}: {}% left · {}", short_name, rem_pct, reset_str);
            parsed_rows.push((short_name, used_pct, label));
        }
    }

    if parsed_rows.is_empty() {
        return None;
    }

    // Put Gemini 4 Argon first if present, then Gemini (5h/7d), then Claude/GPT (5h/7d)
    parsed_rows.sort_by_key(|(name, _, _)| {
        if name.contains("Argon") {
            0
        } else if name.contains("Gemini Models (5h)") {
            1
        } else if name.contains("Gemini Models (7d)") {
            2
        } else if name.contains("Claude/GPT (5h)") {
            3
        } else if name.contains("Claude/GPT (7d)") {
            4
        } else {
            5
        }
    });

    let first = parsed_rows.remove(0);
    let second = if !parsed_rows.is_empty() {
        parsed_rows.remove(0)
    } else {
        ("".to_string(), 0, "".to_string())
    };

    let extra_bars = parsed_rows
        .into_iter()
        .map(|(_, used_pct, label)| QuotaBar {
            pct: used_pct,
            label,
        })
        .collect();

    Some(UsageGauges {
        session_usage_pct: first.1,
        session_usage_label: first.2,
        weekly_usage_pct: second.1,
        weekly_usage_label: second.2,
        extra_bars,
    })
}

pub fn parse_agy_usage(home: &PathBuf) -> AgentSummary {
    let mut summary = AgentSummary {
        name: "Antigravity (AGY)".to_string(),
        provider: "Google".to_string(),
        active: false,
        ..Default::default()
    };

    let brain_dir = home.join(".gemini").join("antigravity-cli").join("brain");
    if !brain_dir.exists() {
        return summary;
    }

    summary.active = true;

    let now = SystemTime::now();
    let seven_days_secs = 7 * 24 * 3600;

    let mut session_count = 0;
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cache = 0u64;

    let mut week_input = 0u64;
    let mut week_output = 0u64;
    let mut week_cache = 0u64;

    let mut latest_file_mtime = SystemTime::UNIX_EPOCH;
    let mut latest_file_path: Option<PathBuf> = None;

    for entry in WalkDir::new(&brain_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() == "transcript.jsonl")
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
            for line in reader.lines().filter_map(|l| l.ok()) {
                if !line.contains("input_tokens") {
                    continue;
                }
                if let Ok(val) = serde_json::from_str::<Value>(&line) {
                    let input = val.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let output = val.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cache = val.get("cache_read_tokens").and_then(|v| v.as_u64()).unwrap_or(0);

                    total_input += input;
                    total_output += output;
                    total_cache += cache;

                    if is_recent_week {
                        week_input += input;
                        week_output += output;
                        week_cache += cache;
                    }
                }
            }
        }
    }

    let total = total_input + total_output + total_cache;
    let est_cost = (total_input as f64 * 0.15 + total_output as f64 * 0.60 + total_cache as f64 * 0.04) / 1_000_000.0;
    let week_cost = (week_input as f64 * 0.15 + week_output as f64 * 0.60 + week_cache as f64 * 0.04) / 1_000_000.0;
    let week_tokens = week_input + week_output + week_cache;

    summary.session_count = session_count;
    summary.tokens = TokenCounts {
        input_tokens: total_input,
        output_tokens: total_output,
        cache_read_tokens: total_cache,
        cache_creation_tokens: 0,
        total_tokens: total,
        estimated_cost_usd: est_cost,
    };

    // 1순위: 실시간 AGY `agy -p /usage` 쿼터 조회 (5개 전체 버킷)
    if let Some(live_gauges) = fetch_live_agy_usage(home) {
        summary.usage = live_gauges;
        return summary;
    }

    // 폴백: 로컬 트랜스크립트 기반 추정
    let mut session_tokens = 0u64;
    let mut step_count = 0u64;

    if let Some(path) = latest_file_path {
        if let Ok(file) = File::open(path) {
            let reader = BufReader::new(file);
            for line in reader.lines().filter_map(|l| l.ok()) {
                if line.contains("input_tokens") {
                    step_count += 1;
                    if let Ok(val) = serde_json::from_str::<Value>(&line) {
                        let inp = val.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        let out = val.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        session_tokens += inp + out;
                    }
                }
            }
        }
    }

    let session_cap = 100_000f64;
    let session_pct = ((session_tokens as f64 / session_cap) * 100.0).clamp(0.0, 100.0) as u16;
    let week_pct = ((week_cost / 15.0) * 100.0).clamp(0.0, 100.0) as u16;

    summary.usage = UsageGauges {
        session_usage_pct: session_pct,
        session_usage_label: format!("Current Session: {}% used · {:.1}k ({} steps)", session_pct, session_tokens as f64 / 1000.0, step_count),
        weekly_usage_pct: week_pct,
        weekly_usage_label: format!("Current Week: {}% used · ${:.2} ({:.1}M tok)", week_pct, week_cost, week_tokens as f64 / 1_000_000.0),
        extra_bars: Vec::new(),
    };

    summary
}
