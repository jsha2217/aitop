use chrono::{DateTime, Days, Local, TimeZone};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Claude,
    Codex,
    Agy,
    Cursor,
    Copilot,
    Grok,
}

impl Provider {
    pub const ALL: [Self; 6] = [
        Self::Claude,
        Self::Codex,
        Self::Agy,
        Self::Cursor,
        Self::Copilot,
        Self::Grok,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Agy => "agy",
            Self::Cursor => "cursor",
            Self::Copilot => "copilot",
            Self::Grok => "grok",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Agy => "Antigravity",
            Self::Cursor => "Cursor",
            Self::Copilot => "GitHub Copilot",
            Self::Grok => "Grok",
        }
    }
    pub fn supports_history(self) -> bool {
        matches!(self, Self::Claude | Self::Codex | Self::Agy | Self::Grok)
    }
}
impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BarMode {
    #[default]
    Remaining,
    Used,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Period {
    #[default]
    Today,
    #[value(name = "7d")]
    SevenDays,
    #[value(name = "30d")]
    ThirtyDays,
}
impl Period {
    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::SevenDays => "7 days",
            Self::ThirtyDays => "30 days",
        }
    }
    pub fn since(self, now: DateTime<Local>) -> i64 {
        let days = match self {
            Self::Today => 0,
            Self::SevenDays => 6,
            Self::ThirtyDays => 29,
        };
        let date = now
            .date_naive()
            .checked_sub_days(Days::new(days))
            .unwrap_or(now.date_naive());
        date.and_hms_opt(0, 0, 0)
            .and_then(|d| Local.from_local_datetime(&d).earliest())
            .map(|d| d.timestamp())
            .unwrap_or(now.timestamp() - days as i64 * 86400)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    #[default]
    Project,
    Model,
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    NoData,
    AuthRequired,
    Unsupported,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaWindow {
    pub id: String,
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<i64>,
    pub window_minutes: Option<u64>,
}
impl QuotaWindow {
    pub fn new(
        id: &str,
        label: &str,
        used: f64,
        reset: Option<i64>,
        minutes: Option<u64>,
    ) -> Option<Self> {
        if !used.is_finite() || !(0.0..=100.0).contains(&used) {
            return None;
        }
        Some(Self {
            id: clean_text(id),
            label: clean_text(label),
            used_percent: used,
            resets_at: reset,
            window_minutes: minutes,
        })
    }
    pub fn expired(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|t| t <= now)
    }
    pub fn reset_label(&self, now: i64) -> String {
        if self.resets_at.is_some() {
            return countdown(self.resets_at, now);
        }
        match self.window_minutes.filter(|&m| m > 0) {
            Some(m) if m % 1440 == 0 => format!("{}d max", m / 1440),
            Some(m) if m % 60 == 0 => format!("{}h max", m / 60),
            Some(m) if m >= 60 => format!("{}h {}m max", m / 60, m % 60),
            Some(m) => format!("{m}m max"),
            None => "--".into(),
        }
    }
    pub fn percent(&self, mode: BarMode) -> f64 {
        match mode {
            BarMode::Remaining => 100.0 - self.used_percent,
            BarMode::Used => self.used_percent,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuotaSnapshot {
    pub provider: Provider,
    pub availability: Availability,
    pub source: String,
    pub observed_at: Option<i64>,
    pub fetched_at: i64,
    pub retry_after: i64,
    pub windows: Vec<QuotaWindow>,
    pub message: String,
    #[serde(default)]
    pub refresh_error: Option<String>,
}
impl Serialize for QuotaSnapshot {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let now = Local::now().timestamp();
        let mut s = serializer.serialize_struct("QuotaSnapshot", 11)?;
        s.serialize_field("provider", &self.provider)?;
        s.serialize_field("availability", &self.availability)?;
        s.serialize_field("source", &self.source)?;
        s.serialize_field("observed_at", &self.observed_at)?;
        s.serialize_field("fetched_at", &self.fetched_at)?;
        s.serialize_field("retry_after", &self.retry_after)?;
        s.serialize_field("windows", &self.windows)?;
        s.serialize_field("message", &self.message)?;
        s.serialize_field("refresh_error", &self.refresh_error)?;
        s.serialize_field(
            "freshness",
            &if self.availability != Availability::Available {
                "unknown"
            } else if self.stale(now) {
                "stale"
            } else {
                "fresh"
            },
        )?;
        s.serialize_field("status", self.status_label(now))?;
        s.end()
    }
}
impl QuotaSnapshot {
    pub fn unavailable(provider: Provider, state: Availability, message: &str, now: i64) -> Self {
        Self {
            provider,
            availability: state,
            source: "none".into(),
            observed_at: None,
            fetched_at: now,
            retry_after: now + 60,
            windows: vec![],
            message: message.into(),
            refresh_error: None,
        }
    }
    pub fn stale(&self, now: i64) -> bool {
        self.observed_at
            .is_none_or(|t| now.saturating_sub(t) > 300 || t > now + 60)
            || self.windows.iter().any(|w| w.expired(now))
    }
    pub fn status_label(&self, now: i64) -> &'static str {
        match self.availability {
            Availability::Available if self.refresh_error.is_some() => "Refresh failed / cached",
            Availability::Available if self.stale(now) => "Stale / recheck needed",
            Availability::Available
                if self.source == "statusline" || self.source == "local rate-limit snapshot" =>
            {
                "Local observation"
            }
            Availability::Available => "Observed",
            Availability::NoData => "No data",
            Availability::AuthRequired if self.message.contains("Keychain") => {
                "Credential access needed"
            }
            Availability::AuthRequired => "Sign-in required",
            Availability::Unsupported => "Unsupported source",
            Availability::Error => "Refresh failed",
        }
    }
}

/// Canonical counts: input EXCLUDES cache reads/writes; output INCLUDES reasoning.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}
impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
    pub fn valid(&self) -> bool {
        [self.input, self.output, self.cache_read, self.cache_write]
            .iter()
            .all(|&n| n <= 1_000_000_000_000)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HistoryRow {
    pub label: String,
    pub tokens: u64,
    pub sessions: u64,
    pub estimated_cost_usd: Option<f64>,
    #[serde(default)]
    pub priced_cost_usd: Option<f64>,
    pub unpriced_events: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryReport {
    #[serde(default)]
    pub provider: Option<Provider>,
    pub period: Period,
    pub group_by: GroupBy,
    pub since: i64,
    pub until: i64,
    pub time_zone: String,
    pub scope: String,
    pub partial: bool,
    pub available: bool,
    pub warnings: Vec<String>,
    pub rows: Vec<HistoryRow>,
    pub total_tokens: Option<u64>,
    pub estimated_cost_usd: Option<f64>,
    #[serde(default)]
    pub priced_cost_usd: Option<f64>,
    #[serde(default)]
    pub pricing_source: String,
    pub unpriced_events: u64,
}

/// Local usage only. Dollar values are model-rate estimates, never subscription bills.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageTotals {
    pub tokens: Tokens,
    pub events: u64,
    pub estimated_cost_usd: Option<f64>,
    pub priced_cost_usd: Option<f64>,
    pub unpriced_events: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageItem {
    pub label: String,
    pub totals: UsageTotals,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsagePeriod {
    pub period: Period,
    pub since: i64,
    pub until: i64,
    pub totals: UsageTotals,
    pub models: Vec<UsageItem>,
    pub projects: Vec<UsageItem>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageHour {
    pub since: i64,
    pub totals: UsageTotals,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageDay {
    pub date: String,
    pub tokens: Tokens,
    pub events: u64,
}

pub fn combined_days<'a>(providers: impl Iterator<Item = &'a ProviderUsage>) -> Vec<UsageDay> {
    let mut days = std::collections::BTreeMap::<String, UsageDay>::new();
    for usage in providers {
        for day in &usage.days {
            let total = days.entry(day.date.clone()).or_insert_with(|| UsageDay {
                date: day.date.clone(),
                ..Default::default()
            });
            total.tokens.input += day.tokens.input;
            total.tokens.output += day.tokens.output;
            total.tokens.cache_read += day.tokens.cache_read;
            total.tokens.cache_write += day.tokens.cache_write;
            total.events += day.events;
        }
    }
    days.into_values().collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: Provider,
    pub available: bool,
    pub partial: bool,
    pub warnings: Vec<String>,
    pub periods: Vec<UsagePeriod>,
    pub hours: Vec<UsageHour>,
    #[serde(default)]
    pub days: Vec<UsageDay>,
    #[serde(default)]
    pub pricing_source: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub generated_at: i64,
    pub demo: bool,
    pub providers: Vec<QuotaSnapshot>,
    pub history: HistoryReport,
    #[serde(default)]
    pub provider_usage: Vec<ProviderUsage>,
    pub warnings: Vec<String>,
}

pub fn clean_text(value: &str) -> String {
    value
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(120)
        .collect()
}
pub fn format_number(n: u64) -> String {
    for (unit, scale) in [("T", 1e12), ("B", 1e9), ("M", 1e6), ("K", 1e3)] {
        if n as f64 >= scale {
            return format!("{:.1}{unit}", n as f64 / scale);
        }
    }
    n.to_string()
}
pub fn countdown(reset: Option<i64>, now: i64) -> String {
    let Some(t) = reset else {
        return "reset unknown".into();
    };
    let seconds = t - now;
    if seconds <= 0 {
        return "recheck needed".into();
    }
    let m = (seconds + 59) / 60;
    if m >= 1440 {
        format!("{}d {}h", m / 1440, (m % 1440) / 60)
    } else if m >= 60 {
        format!("{}h {}m", m / 60, m % 60)
    } else {
        format!("{m}m")
    }
}
