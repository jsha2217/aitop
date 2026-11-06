use std::collections::HashMap;
use serde::{Deserialize, Serialize};

pub fn format_number(n: u64) -> String {
    if n >= 1_000_000_000_000 {
        format!("{:.1}T", n as f64 / 1_000_000_000_000.0)
    } else if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BarMode {
    Remaining,
    Used,
}

impl Default for BarMode {
    fn default() -> Self {
        BarMode::Remaining
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub initialized: bool,
    pub refresh_interval_secs: u64,
    pub bar_mode: BarMode,
    pub show_claude: bool,
    pub show_codex: bool,
    pub show_agy: bool,
    pub show_cursor: bool,
    pub show_copilot: bool,
    pub show_grok: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            initialized: false,
            refresh_interval_secs: 300,
            bar_mode: BarMode::Remaining,
            show_claude: false,
            show_codex: false,
            show_agy: false,
            show_cursor: false,
            show_copilot: false,
            show_grok: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct QuotaBar {
    pub pct: u16,
    pub label: String,
}

#[derive(Debug, Clone, Default)]
pub struct UsageGauges {
    // 오직 사용량(Usage)과 리셋(Resets)만 관리
    pub session_usage_pct: u16,
    pub session_usage_label: String,

    pub weekly_usage_pct: u16,
    pub weekly_usage_label: String,

    // 추가 한도 바 (예: AGY의 3P Weekly Limit 등 3개 이상의 버킷 표시용)
    pub extra_bars: Vec<QuotaBar>,
}

#[derive(Debug, Clone, Default)]
pub struct TokenCounts {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Clone, Default)]
pub struct AgentSummary {
    pub name: String,
    pub provider: String,
    pub active: bool,
    pub tokens: TokenCounts,
    pub session_count: usize,
    pub usage: UsageGauges,
    pub model_breakdowns: HashMap<String, TokenCounts>,
}

#[derive(Debug, Clone, Default)]
pub struct AllUsageStats {
    pub claude: AgentSummary,
    pub codex: AgentSummary,
    pub agy: AgentSummary,
    pub cursor: AgentSummary,
    pub copilot: AgentSummary,
    pub grok: AgentSummary,
    pub last_updated: String,
    pub total_cost_usd: f64,
    pub total_tokens: u64,
}
