use crate::{
    config::{atomic_write, Config, Paths},
    history::{fingerprint, Pricing, Store},
    models::{
        Availability, GroupBy, HistoryReport, HistoryRow, Period, Provider, ProviderUsage,
        QuotaSnapshot, QuotaWindow, Result, Snapshot, Tokens, UsageDay, UsageHour, UsageItem,
        UsagePeriod, UsageTotals,
    },
    parser::{self, FetchOptions},
};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    sync::{atomic::AtomicBool, mpsc, Arc},
    thread,
};

#[derive(Debug)]
pub enum Update {
    Quota(QuotaSnapshot),
    LocalQuota(QuotaSnapshot),
    History(HistoryReport),
    ProviderUsage(Vec<ProviderUsage>),
    Warning(String),
}

#[derive(Serialize, Deserialize)]
struct Cache {
    marker: String,
    quota: QuotaSnapshot,
    #[serde(default)]
    failures: u32,
}

fn marker(paths: &Paths, p: Provider, _options: FetchOptions) -> String {
    let files: &[&str] = match p {
        Provider::Claude => &[".claude/.credentials.json"],
        Provider::Codex => &[".codex/auth.json"],
        Provider::Copilot => &[
            ".config/github-copilot/hosts.json",
            ".config/github-copilot/apps.json",
        ],
        Provider::Cursor => &[
            "Library/Application Support/Cursor/User/globalStorage/state.vscdb",
            ".config/Cursor/User/globalStorage/state.vscdb",
        ],
        _ => &[],
    };
    let mut value = p.id().to_string();
    for file in files {
        if let Ok(m) = fs::metadata(paths.home.join(file)) {
            value.push_str(&format!(":{}:{:?}", m.len(), m.modified()));
        }
    }
    fingerprint(value)
}
fn cache_path(paths: &Paths, p: Provider) -> std::path::PathBuf {
    paths.data.join(format!("quota-{}.json", p.id()))
}

fn retry_at(p: Provider, q: &QuotaSnapshot) -> i64 {
    if p == Provider::Agy && (q.refresh_error.is_some() || q.availability == Availability::Error) {
        // Older builds persisted 16/32-minute delays after a short CLI timeout.
        q.retry_after.min(q.fetched_at.saturating_add(60))
    } else {
        q.retry_after
    }
}

fn obsolete_agy_cache(p: Provider, q: &QuotaSnapshot) -> bool {
    p == Provider::Agy
        && (q.availability == Availability::NoData
            || q.refresh_error
                .as_deref()
                .is_some_and(|s| s.contains("Use --cli") || s.contains("Use --agy-cli")))
}

fn obsolete_claude_auth_cache(p: Provider, q: &QuotaSnapshot, options: FetchOptions) -> bool {
    p == Provider::Claude
        && options.keychain
        && (q.message.contains("--keychain")
        || q.refresh_error.as_deref().is_some_and(|s|s.contains("--keychain"))
        // Earlier builds suppressed the missing-access error while retaining the old observation.
        || (q.availability==Availability::Available && q.refresh_error.is_none()
            && q.observed_at.is_some_and(|t|q.fetched_at>t)))
}

pub fn cached_quota(paths: &Paths, p: Provider, options: FetchOptions) -> Option<QuotaSnapshot> {
    let value = parser::read_json(&cache_path(paths, p))?;
    let cache: Cache = serde_json::from_value(value).ok()?;
    (cache.marker == marker(paths, p, options)).then_some(cache.quota)
}
pub fn cache_observation(paths: &Paths, quota: QuotaSnapshot) -> Result<()> {
    let cache = Cache {
        marker: marker(paths, quota.provider, FetchOptions::default()),
        quota,
        failures: 0,
    };
    atomic_write(
        &cache_path(paths, cache.quota.provider),
        &serde_json::to_vec(&cache)?,
    )
}

fn quota(
    paths: &Paths,
    p: Provider,
    options: FetchOptions,
    cancel: &AtomicBool,
) -> Result<QuotaSnapshot> {
    let now = Local::now().timestamp();
    let cached = cached_quota(paths, p, options);
    if !options.offline && !options.force_refresh {
        if let Some(q) = cached.as_ref().filter(|q| {
            retry_at(p, q) > now
                && q.fetched_at <= now
                && !obsolete_agy_cache(p, q)
                && !obsolete_claude_auth_cache(p, q, options)
        }) {
            let mut q = q.clone();
            q.retry_after = retry_at(p, &q);
            return Ok(q);
        }
    }
    let local = Store::open(paths)?.local_quota(p)?;
    let previous = [cached, local]
        .into_iter()
        .flatten()
        .filter(|q| {
            q.availability == Availability::Available
                && q.observed_at.is_some_and(|t| t <= now + 60)
        })
        .max_by_key(|q| q.observed_at);
    if options.offline {
        return Ok(previous.unwrap_or_else(|| {
            QuotaSnapshot::unavailable(
                p,
                Availability::NoData,
                "Offline: no stored quota snapshot",
                now,
            )
        }));
    }
    let before = marker(paths, p, options);
    let fetched = parser::fetch(p, paths, options, cancel, now);
    if before != marker(paths, p, options) {
        return Err("Credentials changed during collection; result discarded".into());
    }
    let failure = fetched.availability == Availability::Error || fetched.source == "https";
    let failures = if failure {
        parser::read_json(&cache_path(paths, p))
            .and_then(|v| v.get("failures").and_then(serde_json::Value::as_u64))
            .unwrap_or(0)
            .saturating_add(1)
            .min(6) as u32
    } else {
        0
    };
    let mut q = if fetched.availability != Availability::Available {
        if let Some(mut previous) = previous {
            previous.fetched_at = now;
            previous.retry_after = fetched.retry_after;
            previous.refresh_error = Some(fetched.message);
            previous
        } else {
            fetched
        }
    } else {
        fetched
    };
    if failures > 0 {
        q.retry_after = if p == Provider::Agy {
            now + 60
        } else {
            q.retry_after.max(now + 60 * (1i64 << (failures - 1)))
        };
    }
    // Only normalized metrics are cached. Credentials and raw vendor responses are not stored.
    let cache = Cache {
        marker: marker(paths, p, options),
        quota: q.clone(),
        failures,
    };
    atomic_write(&cache_path(paths, p), &serde_json::to_vec(&cache)?)?;
    Ok(q)
}

pub fn start(
    paths: Paths,
    cfg: Config,
    options: FetchOptions,
    period: Period,
    group: GroupBy,
    cancel: Arc<AtomicBool>,
) -> mpsc::Receiver<Update> {
    start_filtered(paths, cfg, options, period, group, cancel, None)
}

pub fn start_filtered(
    paths: Paths,
    cfg: Config,
    options: FetchOptions,
    period: Period,
    group: GroupBy,
    cancel: Arc<AtomicBool>,
    history_provider: Option<Provider>,
) -> mpsc::Receiver<Update> {
    let options = options.configured(&cfg);
    let (tx, rx) = mpsc::channel();
    for p in Provider::ALL.into_iter().filter(|&p| cfg.enabled(p)) {
        let paths = paths.clone();
        let tx = tx.clone();
        let cancel = cancel.clone();
        thread::spawn(move || {
            let result = std::panic::catch_unwind(|| quota(&paths, p, options, &cancel));
            let q = match result {
                Ok(Ok(q)) => q,
                _ => QuotaSnapshot::unavailable(
                    p,
                    Availability::Error,
                    "Cannot collect or store quota; run aitop doctor",
                    Local::now().timestamp(),
                ),
            };
            let _ = tx.send(Update::Quota(q));
        });
    }
    thread::spawn(move || {
        let work = || -> Result<HistoryReport> {
            let mut store = Store::open(&paths)?;
            let scan = store.scan(&paths, &cfg, &cancel)?;
            let pricing_result = Pricing::load(&paths);
            let invalid_prices = pricing_result.is_err();
            let pricing = pricing_result.unwrap_or_default();
            let mut history_cfg = cfg.clone();
            if let Some(selected) = history_provider {
                for p in Provider::ALL {
                    if p != selected && history_cfg.enabled(p) {
                        history_cfg.toggle(p);
                    }
                }
            }
            let mut report = store.report(&history_cfg, period, group, Local::now(), &pricing)?;
            if let Some(p) = history_provider {
                report.provider = Some(p);
                report.scope = format!("{} / {}", p.name(), report.scope);
            }
            if invalid_prices {
                report
                    .warnings
                    .push("Invalid prices.json; token history retained, costs unavailable".into());
            }
            let issues = scan.issues(history_provider);
            if issues.errors > 0 || issues.incomplete_files > 0 {
                report.partial = true;
                report.warnings.push(format!(
                    "Scan: {} read/parse errors, {} incomplete tails",
                    issues.errors, issues.incomplete_files
                ));
            }
            for p in Provider::ALL.into_iter().filter(|&p| cfg.enabled(p)) {
                if let Some(q) = store.local_quota(p)? {
                    let _ = tx.send(Update::LocalQuota(q));
                }
            }
            let mut details = Vec::new();
            let now = Local::now();
            for p in Provider::ALL.into_iter().filter(|&p| cfg.enabled(p)) {
                let mut usage = store.provider_usage(p, now, &pricing)?;
                let issues = scan.issues(Some(p));
                if issues.errors > 0 || issues.incomplete_files > 0 {
                    usage.partial = true;
                    usage
                        .warnings
                        .push("Scan incomplete; accepted local records only".into());
                }
                if invalid_prices {
                    usage
                        .warnings
                        .push("Invalid prices.json; costs unavailable until repaired".into());
                }
                details.push(usage);
            }
            let _ = tx.send(Update::ProviderUsage(details));
            Ok(report)
        };
        match std::panic::catch_unwind(work) {
            Ok(Ok(report)) => {
                let _ = tx.send(Update::History(report));
            }
            _ => {
                let _=tx.send(Update::Warning("Local history collection failed; check storage permissions, database version, and prices.json".into()));
            }
        }
    });
    rx
}

pub fn empty_history(period: Period, group: GroupBy) -> HistoryReport {
    let now = Local::now();
    HistoryReport {
        provider: None,
        period,
        group_by: group,
        since: period.since(now),
        until: now.timestamp(),
        time_zone: now.offset().to_string(),
        scope: "Observed local history; account attribution unknown".into(),
        partial: false,
        available: false,
        warnings: vec![],
        rows: vec![],
        total_tokens: None,
        estimated_cost_usd: None,
        priced_cost_usd: None,
        pricing_source: "Loading rates".into(),
        unpriced_events: 0,
    }
}
pub fn initial(
    paths: &Paths,
    cfg: &Config,
    options: FetchOptions,
    period: Period,
    group: GroupBy,
) -> Snapshot {
    let now = Local::now().timestamp();
    Snapshot {
        schema_version: 1,
        generated_at: now,
        demo: false,
        providers: Provider::ALL
            .into_iter()
            .filter(|&p| cfg.enabled(p))
            .map(|p| {
                cached_quota(paths, p, options).unwrap_or_else(|| {
                    QuotaSnapshot::unavailable(
                        p,
                        Availability::NoData,
                        "Waiting for first observation",
                        now,
                    )
                })
            })
            .collect(),
        history: empty_history(period, group),
        provider_usage: vec![],
        warnings: vec![],
    }
}
pub fn apply(snapshot: &mut Snapshot, update: Update) {
    snapshot.generated_at = Local::now().timestamp();
    match update {
        Update::Quota(q) => {
            if let Some(old) = snapshot
                .providers
                .iter_mut()
                .find(|v| v.provider == q.provider)
            {
                if q.availability != Availability::Available
                    && old.availability == Availability::Available
                {
                    if q.availability == Availability::Error || q.source == "https" {
                        old.refresh_error = Some(q.message);
                    }
                    old.fetched_at = q.fetched_at;
                    old.retry_after = q.retry_after;
                } else {
                    *old = q;
                }
            } else {
                snapshot.providers.push(q);
                snapshot
                    .providers
                    .sort_by_key(|q| Provider::ALL.iter().position(|&p| p == q.provider));
            }
        }
        Update::LocalQuota(mut q) => {
            if let Some(old) = snapshot
                .providers
                .iter_mut()
                .find(|v| v.provider == q.provider)
            {
                if old.availability != Availability::Available || old.observed_at < q.observed_at {
                    if old.availability == Availability::Error || old.source == "https" {
                        q.refresh_error = Some(old.message.clone());
                    }
                    *old = q;
                }
            }
        }
        Update::History(report) => snapshot.history = report,
        Update::ProviderUsage(usage) => snapshot.provider_usage = usage,
        Update::Warning(w) => snapshot.warnings.push(w),
    }
}
pub fn collect(
    paths: &Paths,
    cfg: &Config,
    options: FetchOptions,
    period: Period,
    group: GroupBy,
    cancel: Arc<AtomicBool>,
) -> Snapshot {
    let mut snapshot = initial(paths, cfg, options, period, group);
    for update in start(paths.clone(), cfg.clone(), options, period, group, cancel) {
        apply(&mut snapshot, update);
    }
    snapshot
}
fn demo_usage(now: i64) -> Vec<ProviderUsage> {
    use chrono::Timelike;
    let local_now = chrono::DateTime::from_timestamp(now, 0)
        .unwrap()
        .with_timezone(&Local);
    let current_hour = local_now
        .with_minute(0)
        .and_then(|d| d.with_second(0))
        .and_then(|d| d.with_nanosecond(0))
        .unwrap()
        .timestamp();
    let first_hour = Period::Today.since(local_now);
    let active_hours = ((current_hour - first_hour) / 3600 + 1) as u64;
    let hour_weight = |hour: u64| (hour * 7 + 3) % 11 + 1;
    let hour_weight_total: u64 = (0..active_hours).map(hour_weight).sum();
    let portion = |t: &UsageTotals, numerator: u64, denominator: u64| UsageTotals {
        tokens: Tokens {
            input: t.tokens.input * numerator / denominator,
            output: t.tokens.output * numerator / denominator,
            cache_read: t.tokens.cache_read * numerator / denominator,
            cache_write: t.tokens.cache_write * numerator / denominator,
        },
        events: t.events * numerator / denominator,
        estimated_cost_usd: t
            .estimated_cost_usd
            .map(|v| v * numerator as f64 / denominator as f64),
        priced_cost_usd: t
            .priced_cost_usd
            .map(|v| v * numerator as f64 / denominator as f64),
        unpriced_events: 0,
    };
    [Provider::Claude, Provider::Codex]
        .into_iter()
        .map(|provider| {
            let daily = UsageTotals {
                tokens: if provider == Provider::Claude {
                    Tokens {
                        input: 180_000,
                        output: 62_000,
                        cache_read: 460_000,
                        cache_write: 40_000,
                    }
                } else {
                    Tokens {
                        input: 120_000,
                        output: 38_000,
                        cache_read: 300_000,
                        cache_write: 40_000,
                    }
                },
                events: active_hours,
                estimated_cost_usd: Some(if provider == Provider::Claude {
                    3.16
                } else {
                    1.66
                }),
                priced_cost_usd: Some(if provider == Provider::Claude {
                    3.16
                } else {
                    1.66
                }),
                unpriced_events: 0,
            };
            let periods = [
                (Period::Today, 1),
                (Period::SevenDays, 4),
                (Period::ThirtyDays, 12),
            ]
            .into_iter()
            .map(|(period, scale)| {
                let totals = portion(&daily, scale, 1);
                let mut last = totals.clone();
                let projects = [("aitop", 5), ("workspace-api", 3), ("personal-notes", 2)]
                    .into_iter()
                    .enumerate()
                    .map(|(i, (label, weight))| {
                        let totals = if i == 2 {
                            last.clone()
                        } else {
                            portion(&totals, weight, 10)
                        };
                        last.tokens.input -= totals.tokens.input;
                        last.tokens.output -= totals.tokens.output;
                        last.tokens.cache_read -= totals.tokens.cache_read;
                        last.tokens.cache_write -= totals.tokens.cache_write;
                        last.events -= totals.events;
                        last.estimated_cost_usd = Some(
                            last.estimated_cost_usd.unwrap() - totals.estimated_cost_usd.unwrap(),
                        );
                        last.priced_cost_usd = last.estimated_cost_usd;
                        UsageItem {
                            label: label.into(),
                            totals,
                        }
                    })
                    .collect();
                UsagePeriod {
                    period,
                    since: period.since(local_now),
                    until: now,
                    models: vec![UsageItem {
                        label: if provider == Provider::Claude {
                            "demo-model-a".into()
                        } else {
                            "demo-model-b".into()
                        },
                        totals: totals.clone(),
                    }],
                    projects,
                    totals,
                }
            })
            .collect();
            let mut remaining = daily.clone();
            let hours = (0..24)
                .map(|i| {
                    let since = current_hour - (23 - i) * 3600;
                    let totals = if since < first_hour {
                        UsageTotals {
                            estimated_cost_usd: Some(0.0),
                            priced_cost_usd: Some(0.0),
                            ..Default::default()
                        }
                    } else if i == 23 {
                        remaining.clone()
                    } else {
                        let mut totals = portion(
                            &daily,
                            hour_weight(((since - first_hour) / 3600) as u64),
                            hour_weight_total,
                        );
                        totals.events = 1;
                        remaining.tokens.input -= totals.tokens.input;
                        remaining.tokens.output -= totals.tokens.output;
                        remaining.tokens.cache_read -= totals.tokens.cache_read;
                        remaining.tokens.cache_write -= totals.tokens.cache_write;
                        remaining.events -= 1;
                        remaining.estimated_cost_usd = Some(
                            remaining.estimated_cost_usd.unwrap()
                                - totals.estimated_cost_usd.unwrap(),
                        );
                        remaining.priced_cost_usd = remaining.estimated_cost_usd;
                        totals
                    };
                    UsageHour { since, totals }
                })
                .collect();
            ProviderUsage {
                provider,
                available: true,
                partial: false,
                warnings: vec![],
                pricing_source: "Demo rates".into(),
                periods,
                hours,
                days: (0..365)
                    .map(|i| {
                        let ago = 364 - i;
                        let totals = if ago == 0
                            || [1, 3, 5, 7, 10, 13, 16, 19, 22, 25, 28].contains(&ago)
                        {
                            daily.clone()
                        } else if ago < 30
                            || (ago + if provider == Provider::Claude { 0 } else { 3 }) % 11 < 3
                        {
                            portion(&daily, 0, 1)
                        } else {
                            portion(&daily, 1 + (ago * 7 % 8) as u64, 4)
                        };
                        UsageDay {
                            date: (local_now.date_naive() - chrono::Duration::days(ago))
                                .to_string(),
                            events: if totals.tokens.total() > 0 {
                                totals.events.max(1)
                            } else {
                                0
                            },
                            tokens: totals.tokens,
                        }
                    })
                    .collect(),
            }
        })
        .collect()
}

pub fn demo(period: Period, group: GroupBy) -> Snapshot {
    demo_filtered(period, group, None)
}

pub fn demo_filtered(period: Period, group: GroupBy, filter: Option<Provider>) -> Snapshot {
    let now = Local::now().timestamp();
    let mut providers = vec![];
    for (p, short, week) in [
        (Provider::Claude, 36.0, 72.0),
        (Provider::Codex, 19.0, 43.0),
    ] {
        providers.push(parser::measured(
            p,
            "demo data",
            vec![
                QuotaWindow::new("5h", "5h", short, Some(now + 8040), Some(300)).unwrap(),
                QuotaWindow::new("7d", "7d", week, Some(now + 367200), Some(10080)).unwrap(),
            ],
            now,
        ));
    }
    providers.push(QuotaSnapshot::unavailable(
        Provider::Cursor,
        Availability::AuthRequired,
        "Demo of an unavailable source; no quota inferred",
        now,
    ));
    let provider_usage = demo_usage(now);
    let mut history = empty_history(period, group);
    history.provider = filter;
    history.scope = "DEMO: synthetic local usage".into();
    let mut rows = std::collections::BTreeMap::<String, HistoryRow>::new();
    let mut tokens = 0;
    let mut cost = 0.0;
    for usage in provider_usage
        .iter()
        .filter(|u| filter.is_none_or(|p| p == u.provider))
    {
        let p = usage.periods.iter().find(|p| p.period == period).unwrap();
        history.available = true;
        tokens += p.totals.tokens.total();
        cost += p.totals.estimated_cost_usd.unwrap();
        let provider_item = [UsageItem {
            label: usage.provider.id().into(),
            totals: p.totals.clone(),
        }];
        let items = match group {
            GroupBy::Project => &p.projects,
            GroupBy::Model => &p.models,
            GroupBy::Provider => provider_item.as_slice(),
        };
        for item in items {
            let row = rows
                .entry(item.label.clone())
                .or_insert_with(|| HistoryRow {
                    label: item.label.clone(),
                    estimated_cost_usd: Some(0.0),
                    ..Default::default()
                });
            row.tokens += item.totals.tokens.total();
            row.sessions += 1;
            *row.estimated_cost_usd.as_mut().unwrap() += item.totals.estimated_cost_usd.unwrap();
            row.priced_cost_usd = row.estimated_cost_usd;
        }
    }
    history.total_tokens = history.available.then_some(tokens);
    history.estimated_cost_usd = history.available.then_some(cost);
    history.priced_cost_usd = history.estimated_cost_usd;
    history.pricing_source = "Demo rates".into();
    history.rows = rows.into_values().collect();
    history
        .rows
        .sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.label.cmp(&b.label)));
    Snapshot {
        schema_version: 1,
        generated_at: now,
        demo: true,
        providers,
        history,
        provider_usage,
        warnings: vec![],
    }
}
