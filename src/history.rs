use crate::{
    config::{private_dir, Config, Paths},
    models::{
        clean_text, GroupBy, HistoryReport, HistoryRow, Period, Provider, ProviderUsage,
        QuotaSnapshot, Result, Tokens, UsageDay, UsageHour, UsageItem, UsagePeriod, UsageTotals,
    },
};
use chrono::{DateTime, Local, Timelike};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use walkdir::WalkDir;

pub fn fingerprint(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct ParseState {
    session: String,
    project: String,
    model: String,
    cumulative: Tokens,
}

#[derive(Debug)]
pub struct UsageEvent {
    pub id: String,
    pub provider: Provider,
    pub timestamp: i64,
    pub session: String,
    pub project: String,
    pub model: String,
    pub tokens: Tokens,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanReport {
    pub files: u64,
    pub bytes_read: u64,
    pub events: u64,
    pub incomplete_files: u64,
    pub errors: u64,
    #[serde(default)]
    pub by_provider: BTreeMap<String, ScanIssues>,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ScanIssues {
    pub errors: u64,
    pub incomplete_files: u64,
}
impl ScanReport {
    pub fn issues(&self, provider: Option<Provider>) -> ScanIssues {
        if let Some(p) = provider {
            self.by_provider.get(p.id()).copied().unwrap_or_default()
        } else {
            ScanIssues {
                errors: self.errors,
                incomplete_files: self.incomplete_files,
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub prompt_threshold: Option<u64>,
    #[serde(default)]
    pub long_prompt: Option<Box<Rates>>,
}
impl Rates {
    fn valid(&self) -> bool {
        [
            Some(self.input),
            Some(self.output),
            Some(self.cache_read),
            self.cache_write,
        ]
        .into_iter()
        .flatten()
        .all(|n| n.is_finite() && (0.0..=1_000_000.0).contains(&n))
            && self.prompt_threshold.is_some() == self.long_prompt.is_some()
            && self.long_prompt.as_ref().is_none_or(|r| r.valid())
    }
}
#[derive(Debug, Default, Deserialize)]
pub struct Pricing {
    #[serde(default)]
    pub models: BTreeMap<String, Rates>,
    #[serde(default)]
    pub verified_at: Option<String>,
    #[serde(skip)]
    overrides: usize,
}
impl Pricing {
    pub fn load(paths: &Paths) -> Result<Self> {
        let mut bundled: Self = serde_json::from_str(include_str!("../assets/pricing.json"))?;
        let path = paths.config.with_file_name("prices.json");
        match fs::read(path) {
            Ok(bytes) => {
                let prices: Self = serde_json::from_slice(&bytes)
                    .map_err(|_| "Invalid prices.json; costs unavailable until repaired")?;
                if prices.models.values().any(|r| !r.valid()) {
                    return Err("Invalid prices.json rate".into());
                }
                bundled.overrides = prices.models.len();
                bundled.models.extend(prices.models);
                Ok(bundled)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(bundled),
            Err(_) => Err("Cannot read prices.json".into()),
        }
    }
    pub fn source(&self) -> String {
        match &self.verified_at {
            Some(date) => format!(
                "Official API rates {date}{}",
                if self.overrides > 0 {
                    " + local overrides"
                } else {
                    ""
                }
            ),
            None if self.models.is_empty() => "Rates unavailable".into(),
            None => "Configured API rates".into(),
        }
    }
    pub fn cost(&self, model: &str, t: &Tokens) -> Option<f64> {
        if t.total() == 0 {
            return Some(0.0);
        }
        self.models.get(model).and_then(|r| {
            let prompt = t.input + t.cache_read + t.cache_write;
            let r = if r.prompt_threshold.is_some_and(|limit| prompt > limit) {
                r.long_prompt.as_deref()?
            } else {
                r
            };
            let cache_write = if t.cache_write == 0 {
                0.0
            } else {
                r.cache_write?
            };
            Some(
                (t.input as f64 * r.input
                    + t.output as f64 * r.output
                    + t.cache_read as f64 * r.cache_read
                    + t.cache_write as f64 * cache_write)
                    / 1_000_000.0,
            )
        })
    }
}

#[derive(Default)]
struct UsageSum {
    tokens: Tokens,
    events: u64,
    priced_events: u64,
    cost: f64,
    unpriced_events: u64,
}
impl UsageSum {
    fn add(&mut self, tokens: &Tokens, events: u64, cost: Option<f64>) {
        self.tokens.input += tokens.input;
        self.tokens.output += tokens.output;
        self.tokens.cache_read += tokens.cache_read;
        self.tokens.cache_write += tokens.cache_write;
        self.events += events;
        if let Some(cost) = cost {
            self.cost += cost;
            self.priced_events += events;
        } else {
            self.unpriced_events += events;
        }
    }
    fn finish(self, available: bool) -> UsageTotals {
        UsageTotals {
            tokens: self.tokens,
            events: self.events,
            estimated_cost_usd: (available && self.unpriced_events == 0).then_some(self.cost),
            priced_cost_usd: (available && (self.events == 0 || self.priced_events > 0))
                .then_some(self.cost),
            unpriced_events: self.unpriced_events,
        }
    }
}
fn top_usage(items: BTreeMap<String, UsageSum>) -> Vec<UsageItem> {
    let mut items: Vec<_> = items
        .into_iter()
        .map(|(label, sum)| UsageItem {
            label,
            totals: sum.finish(true),
        })
        .collect();
    items.sort_by(|a, b| {
        b.totals
            .tokens
            .total()
            .cmp(&a.totals.tokens.total())
            .then(a.label.cmp(&b.label))
    });
    items.truncate(3);
    items
}

pub struct Store {
    connection: Connection,
}
static OPEN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
impl Store {
    pub fn open(paths: &Paths) -> Result<Self> {
        // Concurrent quota/history workers must not race the first WAL/schema setup.
        let _lock = OPEN_LOCK
            .lock()
            .map_err(|_| "History initialization lock failed")?;
        private_dir(&paths.data)?;
        let mut connection = Connection::open(paths.data.join("history.sqlite3"))?;
        connection.busy_timeout(Duration::from_secs(3))?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 2 {
            return Err("History database belongs to a newer aitop version".into());
        }
        if version == 0 {
            connection.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS files (path TEXT PRIMARY KEY, provider TEXT NOT NULL, identity TEXT NOT NULL, modified TEXT NOT NULL, size INTEGER NOT NULL, offset INTEGER NOT NULL, state TEXT NOT NULL, errors INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS events (id TEXT PRIMARY KEY, provider TEXT NOT NULL, timestamp INTEGER NOT NULL, session TEXT NOT NULL, project TEXT NOT NULL, model TEXT NOT NULL, input INTEGER NOT NULL, output INTEGER NOT NULL, cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS events_time ON events(timestamp,provider);
            CREATE TABLE IF NOT EXISTS quotas (provider TEXT PRIMARY KEY, observed_at INTEGER NOT NULL, value TEXT NOT NULL);
            PRAGMA user_version=2;")?;
        } else if version == 1 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current: u32 =
                transaction.pragma_query_value(None, "user_version", |r| r.get(0))?;
            if current == 1 {
                // Preserve the old derived AGY index before rebuilding from its canonical transcript.
                // Earlier builds mixed mirror logs and interpreted exclusive cache counts as inclusive.
                transaction.execute_batch("CREATE TABLE IF NOT EXISTS agy_legacy_events AS SELECT * FROM events WHERE provider='agy';
                    CREATE TABLE IF NOT EXISTS agy_legacy_files AS SELECT * FROM files WHERE provider='agy';
                    DELETE FROM events WHERE provider='agy'; DELETE FROM files WHERE provider='agy';
                    PRAGMA user_version=2;")?;
            }
            transaction.commit()?;
        }
        Ok(Self { connection })
    }
    pub fn save_quota(&self, q: &QuotaSnapshot) -> Result<()> {
        let observed = q.observed_at.unwrap_or(0);
        self.connection.execute("INSERT INTO quotas VALUES (?1,?2,?3) ON CONFLICT(provider) DO UPDATE SET observed_at=excluded.observed_at,value=excluded.value WHERE excluded.observed_at>=quotas.observed_at", params![q.provider.id(),observed,serde_json::to_string(q)?])?;
        Ok(())
    }
    pub fn local_quota(&self, p: Provider) -> Result<Option<QuotaSnapshot>> {
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM quotas WHERE provider=?1",
                [p.id()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.and_then(|s| serde_json::from_str(&s).ok()))
    }
    pub fn scan(&mut self, paths: &Paths, cfg: &Config, cancel: &AtomicBool) -> Result<ScanReport> {
        let mut report = ScanReport::default();
        for provider in Provider::ALL {
            if !cfg.enabled(provider) || !provider.supports_history() {
                continue;
            }
            let errors = report.errors;
            let incomplete = report.incomplete_files;
            for root in roots(paths, provider) {
                if !root.exists() {
                    continue;
                }
                for entry in WalkDir::new(root).follow_links(false) {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("Collection cancelled".into());
                    }
                    match entry {
                        Ok(entry)
                            if entry.file_type().is_file()
                                && entry.path().extension().is_some_and(|s| s == "jsonl")
                                && (provider != Provider::Agy
                                    || entry.file_name() == "transcript.jsonl") =>
                        {
                            report.files += 1;
                            if self
                                .scan_file(entry.path(), provider, &mut report, cancel)
                                .is_err()
                            {
                                report.errors += 1;
                            }
                        }
                        Err(_) => report.errors += 1,
                        _ => {}
                    }
                }
            }
            report.by_provider.insert(
                provider.id().into(),
                ScanIssues {
                    errors: report.errors - errors,
                    incomplete_files: report.incomplete_files - incomplete,
                },
            );
        }
        Ok(report)
    }
    fn scan_file(
        &mut self,
        path: &Path,
        provider: Provider,
        report: &mut ScanReport,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        let path_id = fingerprint(path.to_string_lossy().as_bytes());
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            format!("{}:{}", meta.dev(), meta.ino())
        };
        #[cfg(not(unix))]
        let identity = format!("{:?}", meta.created());
        let modified = format!("{:?}", meta.modified()?);
        type Cursor = (String, String, u64, u64, String, u64);
        let previous: Option<Cursor> = self
            .connection
            .query_row(
                "SELECT identity,modified,size,offset,state,errors FROM files WHERE path=?1",
                [&path_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?;
        let mut offset = 0;
        let mut errors = 0;
        let mut state = ParseState {
            session: path_id.clone(),
            project: "Unattributed".into(),
            model: "Unknown".into(),
            ..Default::default()
        };
        if let Some((old_id, old_modified, old_size, old_offset, old_state, old_errors)) = previous
        {
            if old_id == identity
                && old_size == meta.len()
                && old_modified == modified
                && old_offset == old_size
            {
                report.errors += old_errors;
                return Ok(());
            }
            if old_id == identity
                && meta.len() >= old_size
                && (meta.len() > old_size || old_modified == modified)
            {
                offset = old_offset;
                errors = old_errors;
                state = serde_json::from_str(&old_state).unwrap_or(state);
            }
        }
        let mut file = file;
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(file.take(meta.len() - offset));
        let transaction = self.connection.transaction()?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("Collection cancelled".into());
            }
            let (line, consumed, complete, oversized) = read_line(&mut reader)?;
            if consumed == 0 {
                break;
            }
            report.bytes_read += consumed;
            if !complete
                && !oversized
                && !line.iter().all(u8::is_ascii_whitespace)
                && serde_json::from_slice::<Value>(&line).is_err()
            {
                report.incomplete_files += 1;
                break;
            }
            offset += consumed;
            if oversized {
                errors += 1;
                continue;
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let value: Value = match serde_json::from_slice(&line) {
                Ok(v) => v,
                Err(_) => {
                    errors += 1;
                    continue;
                }
            };
            let timestamp = timestamp(&value);
            if let Some(t) = timestamp {
                if provider == Provider::Codex {
                    if let Some(rl) = value
                        .pointer("/payload/rate_limits")
                        .filter(|v| !v.is_null())
                    {
                        let q = crate::parser::codex::local_limits(rl, t);
                        if !q.windows.is_empty() {
                            transaction.execute("INSERT INTO quotas VALUES (?1,?2,?3) ON CONFLICT(provider) DO UPDATE SET observed_at=excluded.observed_at,value=excluded.value WHERE excluded.observed_at>=quotas.observed_at",params![provider.id(),t,serde_json::to_string(&q)?])?;
                        }
                    }
                }
            }
            match parse_event(provider, &value, &mut state) {
                Ok(Some(event)) => {
                    transaction.execute("INSERT INTO events VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(id) DO UPDATE SET input=excluded.input,output=excluded.output,cache_read=excluded.cache_read,cache_write=excluded.cache_write,model=excluded.model WHERE excluded.input+excluded.output+excluded.cache_read+excluded.cache_write >= events.input+events.output+events.cache_read+events.cache_write",params![event.id,event.provider.id(),event.timestamp,event.session,event.project,event.model,event.tokens.input,event.tokens.output,event.tokens.cache_read,event.tokens.cache_write])?;
                    report.events += 1;
                }
                Err(()) => errors += 1,
                _ => {}
            }
        }
        transaction.execute("INSERT INTO files VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(path) DO UPDATE SET identity=excluded.identity,modified=excluded.modified,size=excluded.size,offset=excluded.offset,state=excluded.state,errors=excluded.errors",params![path_id,provider.id(),identity,modified,meta.len(),offset,serde_json::to_string(&state)?,errors])?;
        transaction.commit()?;
        report.errors += errors;
        Ok(())
    }
    /// All detail periods are read from the same SQLite snapshot. No transcript re-scan on Enter.
    pub fn provider_usage(
        &mut self,
        provider: Provider,
        now: DateTime<Local>,
        pricing: &Pricing,
    ) -> Result<ProviderUsage> {
        let transaction = self.connection.transaction()?;
        let available = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE provider=?1)",
            [provider.id()],
            |r| r.get::<_, bool>(0),
        )?;
        let errors: u64 = transaction.query_row(
            "SELECT COALESCE(SUM(errors),0) FROM files WHERE provider=?1",
            [provider.id()],
            |r| r.get(0),
        )?;
        let until = now.timestamp();
        let mut periods = Vec::new();
        for period in [Period::Today, Period::SevenDays, Period::ThirtyDays] {
            let since = period.since(now);
            let mut sum = UsageSum::default();
            let mut models: BTreeMap<String, UsageSum> = BTreeMap::new();
            let mut projects: BTreeMap<String, UsageSum> = BTreeMap::new();
            let mut statement = transaction.prepare("SELECT project,model,input,output,cache_read,cache_write,1 FROM events WHERE provider=?1 AND timestamp>=?2 AND timestamp<=?3")?;
            let records = statement.query_map(params![provider.id(), since, until], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    Tokens {
                        input: r.get(2)?,
                        output: r.get(3)?,
                        cache_read: r.get(4)?,
                        cache_write: r.get(5)?,
                    },
                    r.get::<_, u64>(6)?,
                ))
            })?;
            for record in records {
                let (project, model, tokens, events) = record?;
                let cost = pricing.cost(&model, &tokens);
                sum.add(&tokens, events, cost);
                models.entry(model).or_default().add(&tokens, events, cost);
                projects
                    .entry(project)
                    .or_default()
                    .add(&tokens, events, cost);
            }
            periods.push(UsagePeriod {
                period,
                since,
                until,
                totals: sum.finish(available),
                models: top_usage(models),
                projects: top_usage(projects),
            });
        }
        // 24 consecutive elapsed-hour buckets, including the current partial hour.
        // A timestamp boundary remains unambiguous across a DST fold/gap.
        let start = now
            .with_minute(0)
            .and_then(|d| d.with_second(0))
            .and_then(|d| d.with_nanosecond(0))
            .unwrap_or(now)
            .timestamp()
            - 23 * 3600;
        let mut hours: Vec<UsageSum> = (0..24).map(|_| UsageSum::default()).collect();
        {
            let mut statement = transaction.prepare("SELECT (timestamp-?2)/3600,model,input,output,cache_read,cache_write,1 FROM events WHERE provider=?1 AND timestamp>=?2 AND timestamp<=?3")?;
            let records = statement.query_map(params![provider.id(), start, until], |r| {
                Ok((
                    r.get::<_, usize>(0)?,
                    r.get::<_, String>(1)?,
                    Tokens {
                        input: r.get(2)?,
                        output: r.get(3)?,
                        cache_read: r.get(4)?,
                        cache_write: r.get(5)?,
                    },
                    r.get::<_, u64>(6)?,
                ))
            })?;
            for record in records {
                let (hour, model, tokens, events) = record?;
                if let Some(sum) = hours.get_mut(hour) {
                    sum.add(&tokens, events, pricing.cost(&model, &tokens));
                }
            }
        }
        let first_date = now.date_naive() - chrono::Duration::days(364);
        let mut daily = BTreeMap::new();
        {
            // Use the same local calendar as period totals. Include all disjoint token categories.
            let mut statement = transaction.prepare("SELECT date(timestamp,'unixepoch','localtime'),SUM(input),SUM(output),SUM(cache_read),SUM(cache_write),COUNT(*) FROM events WHERE provider=?1 AND timestamp>=?2 AND timestamp<=?3 GROUP BY date(timestamp,'unixepoch','localtime')")?;
            let records =
                statement.query_map(params![provider.id(), until - 366 * 86400, until], |r| {
                    Ok(UsageDay {
                        date: r.get(0)?,
                        tokens: Tokens {
                            input: r.get(1)?,
                            output: r.get(2)?,
                            cache_read: r.get(3)?,
                            cache_write: r.get(4)?,
                        },
                        events: r.get(5)?,
                    })
                })?;
            for record in records {
                let day = record?;
                daily.insert(day.date.clone(), day);
            }
        }
        let days = (0..365)
            .map(|i| {
                let date = (first_date + chrono::Duration::days(i)).to_string();
                daily.remove(&date).unwrap_or_else(|| UsageDay {
                    date,
                    ..Default::default()
                })
            })
            .collect();
        transaction.commit()?;
        Ok(ProviderUsage {
            provider,
            available,
            partial: errors > 0,
            warnings: if errors > 0 {
                vec![format!("{errors} unsupported or damaged usage records")]
            } else {
                vec![]
            },
            periods,
            days,
            pricing_source: pricing.source(),
            hours: hours
                .into_iter()
                .enumerate()
                .map(|(i, sum)| UsageHour {
                    since: start + i as i64 * 3600,
                    totals: sum.finish(available),
                })
                .collect(),
        })
    }

    pub fn report(
        &self,
        cfg: &Config,
        period: Period,
        group: GroupBy,
        now: DateTime<Local>,
        pricing: &Pricing,
    ) -> Result<HistoryReport> {
        let since = period.since(now);
        let until = now.timestamp();
        let mut rows: BTreeMap<String, HistoryRow> = BTreeMap::new();
        let mut total = 0u64;
        let mut cost = 0.0;
        let mut unpriced = 0;
        let mut priced_events = 0;
        let mut statement=self.connection.prepare("SELECT provider,project,model,input,output,cache_read,cache_write,1,1 FROM events WHERE timestamp>=?1 AND timestamp<=?2")?;
        let records = statement.query_map(params![since, until], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                Tokens {
                    input: r.get(3)?,
                    output: r.get(4)?,
                    cache_read: r.get(5)?,
                    cache_write: r.get(6)?,
                },
                r.get::<_, u64>(7)?,
                r.get::<_, u64>(8)?,
            ))
        })?;
        for record in records {
            let (p, project, model, tokens, events, sessions) = record?;
            if !Provider::ALL
                .iter()
                .any(|&provider| provider.id() == p && cfg.enabled(provider))
            {
                continue;
            }
            let label = match group {
                GroupBy::Project => project,
                GroupBy::Model => model.clone(),
                GroupBy::Provider => p,
            };
            let row = rows.entry(label.clone()).or_insert_with(|| HistoryRow {
                label,
                estimated_cost_usd: Some(0.0),
                ..Default::default()
            });
            row.tokens += tokens.total();
            row.sessions += sessions;
            total += tokens.total();
            if let Some(value) = pricing.cost(&model, &tokens) {
                cost += value;
                priced_events += events;
                *row.priced_cost_usd.get_or_insert(0.0) += value;
                if let Some(sum) = &mut row.estimated_cost_usd {
                    *sum += value;
                }
            } else {
                unpriced += events;
                row.unpriced_events += events;
                row.estimated_cost_usd = None;
            }
        }
        let enabled = Provider::ALL
            .into_iter()
            .filter(|&p| cfg.enabled(p))
            .map(|p| format!("'{}'", p.id()))
            .collect::<Vec<_>>()
            .join(",");
        let enabled = if enabled.is_empty() {
            "''".to_string()
        } else {
            enabled
        };
        let event_count: u64 = self.connection.query_row(
            &format!("SELECT COUNT(*) FROM events WHERE provider IN ({enabled})"),
            [],
            |r| r.get(0),
        )?;
        let errors: u64 = self.connection.query_row(
            &format!("SELECT COALESCE(SUM(errors),0) FROM files WHERE provider IN ({enabled})"),
            [],
            |r| r.get(0),
        )?;
        // Count sessions at the requested grouping level, not once per model/project.
        let column = match group {
            GroupBy::Project => "project",
            GroupBy::Model => "model",
            GroupBy::Provider => "provider",
        };
        let mut session_query=self.connection.prepare(&format!("SELECT {column},COUNT(DISTINCT provider||':'||session) FROM events WHERE timestamp>=?1 AND timestamp<=?2 AND provider IN ({enabled}) GROUP BY {column}"))?;
        let sessions = session_query.query_map(params![since, until], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
        })?;
        for session in sessions {
            let (label, count) = session?;
            if let Some(row) = rows.get_mut(&label) {
                row.sessions = count;
            }
        }
        let warnings = if errors > 0 {
            vec![format!(
                "{errors} unsupported or damaged usage records; totals cover accepted records only"
            )]
        } else {
            vec![]
        };
        let available = event_count > 0;
        let mut rows: Vec<_> = rows.into_values().collect();
        rows.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.label.cmp(&b.label)));
        Ok(HistoryReport {provider:None,period,group_by:group,since,until,time_zone:now.offset().to_string(),scope:"Observed local history; account attribution unknown; other devices and deleted logs excluded".into(),partial:errors>0,available,warnings,rows,total_tokens:available.then_some(total),estimated_cost_usd:if available && unpriced==0 {Some(cost)} else {None},priced_cost_usd:if available&&(priced_events>0||total==0) {Some(cost)} else {None},pricing_source:pricing.source(),unpriced_events:unpriced})
    }
}

pub fn roots(paths: &Paths, p: Provider) -> Vec<PathBuf> {
    let dirs: &[&str] = match p {
        Provider::Claude => &[".claude/projects"],
        Provider::Codex => &[".codex/sessions", ".codex/archived_sessions"],
        Provider::Agy => &[".gemini/antigravity-cli/brain"],
        Provider::Grok => &[".grok"],
        _ => &[],
    };
    dirs.iter().map(|d| paths.home.join(d)).collect()
}

// Bound memory even for a corrupt, multi-megabyte JSONL record.
fn read_line(reader: &mut impl BufRead) -> std::io::Result<(Vec<u8>, u64, bool, bool)> {
    let mut line = Vec::new();
    let mut count = 0;
    let mut oversized = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok((line, count, false, oversized));
        }
        let end = buf.iter().position(|&b| b == b'\n').map(|i| i + 1);
        let n = end.unwrap_or(buf.len());
        count += n as u64;
        if count <= 1024 * 1024 {
            line.extend_from_slice(&buf[..n]);
        } else {
            oversized = true;
            line.clear();
        }
        reader.consume(n);
        if end.is_some() {
            return Ok((line, count, true, oversized));
        }
    }
}

pub fn timestamp(v: &Value) -> Option<i64> {
    let t = v.get("timestamp").or_else(|| v.get("created_at"))?;
    let t = if let Some(s) = t.as_str() {
        DateTime::parse_from_rfc3339(s).ok()?.timestamp()
    } else {
        let n = t.as_i64()?;
        if n > 100_000_000_000 {
            n / 1000
        } else {
            n
        }
    };
    (t > 0 && t < 32_503_680_000).then_some(t)
}
fn number(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}
fn counts(v: &Value, includes_cache: bool) -> std::result::Result<Tokens, ()> {
    let input = number(v, "input_tokens").ok_or(())?;
    let output = number(v, "output_tokens").ok_or(())?;
    let cache_read = number(v, "cached_input_tokens")
        .or_else(|| number(v, "cache_read_input_tokens"))
        .or_else(|| number(v, "cache_read_tokens"))
        .unwrap_or(0);
    let cache_write = number(v, "cache_creation_input_tokens")
        .or_else(|| number(v, "cache_creation_tokens"))
        .unwrap_or(0);
    let input = if includes_cache {
        input.checked_sub(cache_read).ok_or(())?
    } else {
        input
    };
    let t = Tokens {
        input,
        output,
        cache_read,
        cache_write,
    };
    if t.valid() {
        Ok(t)
    } else {
        Err(())
    }
}
fn project(cwd: &str) -> String {
    let name = Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .map(clean_text)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Unattributed".into());
    format!("{name} [{}]", &fingerprint(cwd)[..6])
}

fn parse_event(
    p: Provider,
    v: &Value,
    state: &mut ParseState,
) -> std::result::Result<Option<UsageEvent>, ()> {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    if let Some(s) = v.get("sessionId").and_then(Value::as_str) {
        state.session = fingerprint(s);
    }
    if let Some(cwd) = v.get("cwd").and_then(Value::as_str) {
        state.project = project(cwd);
    }
    if p == Provider::Codex && kind == "session_meta" {
        if let Some(s) = v.pointer("/payload/id").and_then(Value::as_str) {
            state.session = fingerprint(s);
        }
        if let Some(cwd) = v.pointer("/payload/cwd").and_then(Value::as_str) {
            state.project = project(cwd);
        }
        return Ok(None);
    }
    if p == Provider::Codex && kind == "turn_context" {
        if let Some(m) = v.pointer("/payload/model").and_then(Value::as_str) {
            state.model = clean_text(m);
        }
        return Ok(None);
    }
    let mut cumulative = false;
    let usage = match p {
        Provider::Claude if kind == "assistant" => v.pointer("/message/usage"),
        Provider::Codex
            if kind == "event_msg"
                && v.pointer("/payload/type").and_then(Value::as_str) == Some("token_count") =>
        {
            cumulative = true;
            v.pointer("/payload/info/total_token_usage")
        }
        Provider::Codex if kind == "token_usage_record" => v.pointer("/payload/usage"),
        Provider::Agy | Provider::Grok => v
            .get("usage")
            .or_else(|| v.pointer("/message/usage"))
            .or_else(|| v.get("input_tokens").map(|_| v)),
        _ => return Ok(None),
    };
    let Some(usage) = usage.filter(|u| !u.is_null()) else {
        return Ok(None);
    };
    let timestamp = timestamp(v).ok_or(())?;
    let agy_transcript = p == Provider::Agy
        && v.get("input_tokens").is_some()
        && v.get("cache_read_tokens").is_some();
    let totals = counts(
        usage,
        matches!(p, Provider::Codex | Provider::Agy) && !agy_transcript,
    )?;
    let tokens = if cumulative {
        let old = &state.cumulative;
        let diff = Tokens {
            input: totals.input.checked_sub(old.input).ok_or(())?,
            output: totals.output.checked_sub(old.output).ok_or(())?,
            cache_read: totals.cache_read.checked_sub(old.cache_read).ok_or(())?,
            cache_write: totals.cache_write.checked_sub(old.cache_write).ok_or(())?,
        };
        state.cumulative = totals.clone();
        diff
    } else {
        if p == Provider::Codex {
            state.cumulative.input += totals.input;
            state.cumulative.output += totals.output;
            state.cumulative.cache_read += totals.cache_read;
            state.cumulative.cache_write += totals.cache_write;
        }
        totals.clone()
    };
    let model = v
        .pointer("/message/model")
        .or_else(|| v.get("model"))
        .and_then(Value::as_str)
        .map(clean_text)
        .unwrap_or_else(|| state.model.clone());
    let raw_id = if cumulative {
        serde_json::to_string(&totals).map_err(|_| ())?
    } else {
        v.pointer("/message/id")
            .or_else(|| v.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| fingerprint(serde_json::to_vec(v).unwrap_or_default()))
    };
    let id = fingerprint(format!("{}:{}:{raw_id}", p.id(), state.session));
    Ok(Some(UsageEvent {
        id,
        provider: p,
        timestamp,
        session: state.session.clone(),
        project: state.project.clone(),
        model,
        tokens,
    }))
}
