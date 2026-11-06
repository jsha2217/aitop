pub mod claude;
pub mod codex;
pub mod agy;
pub mod extras;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use chrono::{DateTime, Local, Utc};
use serde_json::Value;
use crate::models::{AgentSummary, AllUsageStats, AppConfig};

/// Detect which AI coding agents have active local authentication on this Mac
pub fn detect_authenticated_agents(home: &PathBuf) -> AppConfig {
    // 1. Claude Code: macOS Keychain "Claude Code-credentials" or ~/.claude/.credentials.json
    let has_claude = Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        || home.join(".claude/.credentials.json").exists();

    // 2. Codex CLI: ~/.codex/auth.json with access_token or OPENAI_API_KEY
    let codex_auth_path = home.join(".codex/auth.json");
    let has_codex = if codex_auth_path.exists() {
        fs::read_to_string(&codex_auth_path)
            .ok()
            .and_then(|c| serde_json::from_str::<Value>(&c).ok())
            .map(|v| {
                v.get("tokens")
                    .and_then(|t| t.get("access_token"))
                    .and_then(|t| t.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
                    || v.get("OPENAI_API_KEY")
                        .and_then(|t| t.as_str())
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
            })
            .unwrap_or(false)
    } else {
        false
    };

    // 3. Antigravity (AGY): ~/.gemini/antigravity or ~/.gemini/antigravity-cli or ~/.gemini/oauth_creds.json
    let has_agy = home.join(".gemini/antigravity").exists()
        || home.join(".gemini/antigravity-cli").exists()
        || home.join(".gemini/oauth_creds.json").exists();

    // 4. Cursor: ~/Library/Application Support/Cursor/User/globalStorage/state.vscdb -> cursorAuth/accessToken
    let cursor_db = home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb");
    let has_cursor = if cursor_db.exists() {
        Command::new("sqlite3")
            .arg(&cursor_db)
            .arg("SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken' LIMIT 1;")
            .stdin(std::process::Stdio::null())
            .output()
            .ok()
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false)
    } else {
        false
    };

    // 5. GitHub Copilot: ~/.config/github-copilot/hosts.json or apps.json -> oauth_token
    let mut has_copilot = false;
    for p in [
        home.join(".config/github-copilot/hosts.json"),
        home.join(".config/github-copilot/apps.json"),
    ] {
        if p.exists() {
            if let Ok(content) = fs::read_to_string(&p) {
                if content.contains("oauth_token") {
                    has_copilot = true;
                    break;
                }
            }
        }
    }

    // 6. Grok (xAI): XAI_API_KEY env var or ~/.grok/config.json or ~/.config/grok
    let has_grok = std::env::var("XAI_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        || home.join(".grok/config.json").exists()
        || home.join(".config/grok/config.json").exists();

    AppConfig {
        initialized: true,
        refresh_interval_secs: 300,
        bar_mode: crate::models::BarMode::Remaining,
        show_claude: has_claude,
        show_codex: has_codex,
        show_agy: has_agy,
        show_cursor: has_cursor,
        show_copilot: has_copilot,
        show_grok: has_grok,
    }
}

pub fn recompute_totals(stats: &mut AllUsageStats, cfg: &AppConfig) {
    let list = [
        (cfg.show_claude, &stats.claude),
        (cfg.show_codex, &stats.codex),
        (cfg.show_agy, &stats.agy),
        (cfg.show_cursor, &stats.cursor),
        (cfg.show_copilot, &stats.copilot),
        (cfg.show_grok, &stats.grok),
    ];
    stats.total_cost_usd = list
        .iter()
        .filter(|(en, _)| *en)
        .map(|(_, a)| a.tokens.estimated_cost_usd)
        .sum();
    stats.total_tokens = list
        .iter()
        .filter(|(en, _)| *en)
        .map(|(_, a)| a.tokens.total_tokens)
        .sum();
}

pub fn placeholder_agent(agent_idx: usize) -> AgentSummary {
    let (name, provider) = match agent_idx {
        0 => ("Claude Code", "Anthropic"),
        1 => ("Codex CLI", "OpenAI"),
        2 => ("Antigravity (AGY)", "Google"),
        3 => ("Cursor", "Anysphere"),
        4 => ("GitHub Copilot", "GitHub"),
        5 => ("Grok", "xAI"),
        _ => ("Unknown", "-"),
    };
    AgentSummary {
        name: name.to_string(),
        provider: provider.to_string(),
        ..Default::default()
    }
}

pub fn fetch_agent_by_index(home: &PathBuf, agent_idx: usize) -> AgentSummary {
    match agent_idx {
        0 => claude::parse_claude_usage(home),
        1 => codex::parse_codex_usage(home),
        2 => agy::parse_agy_usage(home),
        3 => extras::parse_cursor_usage(home),
        4 => extras::parse_copilot_usage(home),
        5 => extras::parse_grok_usage(home),
        _ => AgentSummary::default(),
    }
}

/// Format remaining seconds into a clean "Xd Yh Zm" or "Yh Zm" countdown string
pub fn format_countdown(diff_secs: i64) -> String {
    if diff_secs <= 0 {
        return "soon".to_string();
    }
    let days = diff_secs / 86400;
    let hours = (diff_secs % 86400) / 3600;
    let mins = (diff_secs % 3600) / 60;

    if days > 0 {
        format!("{}d {}h {}m", days, hours, mins)
    } else if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else {
        format!("{}m", mins.max(1))
    }
}

/// Format an ISO-8601 UTC timestamp string into a clean countdown string (only d/h/m)
pub fn format_reset_iso(iso_str: &str) -> String {
    if let Ok(dt) = DateTime::parse_from_rfc3339(iso_str) {
        let local_dt: DateTime<Local> = DateTime::from(dt);
        let now = Local::now();
        let diff_secs = (local_dt.timestamp() - now.timestamp()).max(0);
        format_countdown(diff_secs)
    } else {
        "-".to_string()
    }
}

/// Format a UNIX epoch timestamp (seconds) into a clean countdown string (only d/h/m)
pub fn format_reset_epoch_secs(epoch_secs: i64) -> String {
    if let Some(dt) = DateTime::<Utc>::from_timestamp(epoch_secs, 0) {
        let local_dt: DateTime<Local> = DateTime::from(dt);
        let now = Local::now();
        let diff_secs = (local_dt.timestamp() - now.timestamp()).max(0);
        format_countdown(diff_secs)
    } else {
        "-".to_string()
    }
}

pub fn collect_all_stats(home: &PathBuf, cfg: &AppConfig) -> AllUsageStats {
    let claude = if cfg.show_claude {
        claude::parse_claude_usage(home)
    } else {
        AgentSummary::default()
    };
    let codex = if cfg.show_codex {
        codex::parse_codex_usage(home)
    } else {
        AgentSummary::default()
    };
    let agy = if cfg.show_agy {
        agy::parse_agy_usage(home)
    } else {
        AgentSummary::default()
    };
    let cursor = if cfg.show_cursor {
        extras::parse_cursor_usage(home)
    } else {
        AgentSummary::default()
    };
    let copilot = if cfg.show_copilot {
        extras::parse_copilot_usage(home)
    } else {
        AgentSummary::default()
    };
    let grok = if cfg.show_grok {
        extras::parse_grok_usage(home)
    } else {
        AgentSummary::default()
    };

    let agents = [&claude, &codex, &agy, &cursor, &copilot, &grok];
    let total_cost: f64 = agents.iter().map(|a| a.tokens.estimated_cost_usd).sum();
    let total_tokens: u64 = agents.iter().map(|a| a.tokens.total_tokens).sum();

    let now_str = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    AllUsageStats {
        claude,
        codex,
        agy,
        cursor,
        copilot,
        grok,
        last_updated: now_str,
        total_cost_usd: total_cost,
        total_tokens,
    }
}
