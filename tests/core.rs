use aitop::{
    config::{Config, Paths},
    history::{Pricing, Store},
    models::{Availability, BarMode, GroupBy, Period, Provider, QuotaWindow},
    parser::{self, FetchOptions},
    runtime, ui,
};
use chrono::{DateTime, Local};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    paths: Paths,
    cfg: Config,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "aitop-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = Paths::new(root.clone(), Some(root.join("cache")));
        Self {
            root,
            paths,
            cfg: Config {
                show_claude: true,
                show_codex: true,
                ..Config::default()
            },
        }
    }
    fn write(&self, path: &str, text: &str) {
        let p = self.root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    fn scan(&self, store: &mut Store) -> aitop::history::ScanReport {
        store
            .scan(&self.paths, &self.cfg, &AtomicBool::new(false))
            .unwrap()
    }
    fn report(&self, store: &Store, period: Period) -> aitop::models::HistoryReport {
        store
            .report(
                &self.cfg,
                period,
                GroupBy::Project,
                now(),
                &Pricing::default(),
            )
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn now() -> DateTime<Local> {
    DateTime::parse_from_rfc3339("2026-11-08T12:00:00Z")
        .unwrap()
        .with_timezone(&Local)
}

#[test]
fn provider_dollars_periods_and_hours_keep_unknown_costs_visible() {
    let f = Fixture::new();
    let today = Period::Today.since(now());
    let week = Period::SevenDays.since(now());
    let month = Period::ThirtyDays.since(now());
    let event = |id: &str, timestamp: i64, model: &str| {
        json!({
        "type":"assistant", "timestamp":timestamp, "sessionId":"test-session", "cwd":"/tmp/project",
        "message":{"id":id,"model":model,"usage":{"input_tokens":1000,"output_tokens":100,"cache_read_input_tokens":500,"cache_creation_input_tokens":200}}
    }).to_string()
    };
    f.write(
        ".claude/projects/a/usage.jsonl",
        &[
            event("today", today, "priced-model"),
            event("yesterday", today - 1, "unknown-model"),
            event("week-start", week, "priced-model"),
            event("month-start", month, "priced-model"),
            event("too-old", month - 1, "priced-model"),
            event("future", now().timestamp() + 1, "priced-model"),
        ]
        .join("\n")
        .chars()
        .chain(std::iter::once('\n'))
        .collect::<String>(),
    );
    f.write(
        ".codex/sessions/usage.jsonl",
        include_str!("fixtures/codex.jsonl"),
    );
    let mut store = Store::open(&f.paths).unwrap();
    f.scan(&mut store);
    let prices: Pricing = serde_json::from_value(json!({"models":{"priced-model":{"input":2.0,"output":8.0,"cache_read":0.2,"cache_write":2.5}}})).unwrap();
    let detail = store
        .provider_usage(Provider::Claude, now(), &prices)
        .unwrap();
    assert!(detail.available);
    assert!(!detail.partial);
    let daily = &detail.periods[0];
    assert_eq!(daily.totals.tokens.total(), 1800);
    assert_eq!(daily.totals.tokens.cache_write, 200);
    assert!((daily.totals.estimated_cost_usd.unwrap() - 0.0034).abs() < 1e-10);
    let weekly = &detail.periods[1];
    assert_eq!(weekly.totals.tokens.total(), 5400);
    assert_eq!(weekly.totals.estimated_cost_usd, None);
    assert_eq!(weekly.totals.unpriced_events, 1);
    assert!((weekly.totals.priced_cost_usd.unwrap() - 0.0068).abs() < 1e-10);
    assert_eq!(detail.periods[2].totals.tokens.total(), 7200);
    assert_eq!(weekly.models.len(), 2);
    assert_eq!(weekly.models[1].totals.estimated_cost_usd, None);
    assert_eq!(weekly.projects[0].totals.unpriced_events, 1);
    assert_eq!(detail.hours.len(), 24);
    assert_eq!(detail.days.len(), 365);
    assert_eq!(
        detail.days.iter().map(|d| d.tokens.total()).sum::<u64>(),
        9000
    );
    assert_eq!(
        detail
            .days
            .iter()
            .find(|d| d.date == now().date_naive().to_string())
            .unwrap()
            .tokens
            .total(),
        1800
    );
    assert!(detail
        .days
        .iter()
        .any(|d| d.events == 0 && d.tokens.total() == 0));
    let codex = store
        .provider_usage(Provider::Codex, now(), &prices)
        .unwrap();
    let combined = aitop::models::combined_days([&detail, &codex].into_iter());
    assert_eq!(combined.len(), 365);
    assert_eq!(
        combined.iter().map(|d| d.tokens.total()).sum::<u64>(),
        11250
    );
    for day in &combined {
        let expected = detail
            .days
            .iter()
            .chain(codex.days.iter())
            .filter(|d| d.date == day.date)
            .map(|d| d.tokens.total())
            .sum::<u64>();
        assert_eq!(day.tokens.total(), expected);
    }
    assert_eq!(
        detail
            .hours
            .iter()
            .map(|h| h.totals.tokens.total())
            .sum::<u64>(),
        3600
    );
    assert!(detail.hours.iter().any(|h| h.totals.unpriced_events == 1));
    assert!(detail.hours.iter().all(|h| h.since <= now().timestamp()));
    let absent = store
        .provider_usage(Provider::Cursor, now(), &prices)
        .unwrap();
    assert!(!absent.available);
    assert!(absent
        .periods
        .iter()
        .all(|p| p.totals.estimated_cost_usd.is_none()));
    assert!(absent
        .hours
        .iter()
        .all(|h| h.totals.estimated_cost_usd.is_none()));
}

#[test]
fn missing_cache_write_rate_is_not_a_free_cache_write() {
    let prices: Pricing = serde_json::from_value(
        json!({"models":{"model":{"input":2.0,"output":8.0,"cache_read":0.2}}}),
    )
    .unwrap();
    let mut tokens = aitop::models::Tokens {
        input: 1000,
        ..Default::default()
    };
    assert_eq!(prices.cost("model", &tokens), Some(0.002));
    tokens.cache_write = 100;
    assert_eq!(prices.cost("model", &tokens), None);
}

#[test]
fn codex_cumulative_cache_and_repeat_are_normalized() {
    let f = Fixture::new();
    f.write(
        ".codex/sessions/a.jsonl",
        include_str!("fixtures/codex.jsonl"),
    );
    let mut store = Store::open(&f.paths).unwrap();
    f.scan(&mut store);
    let report = f.report(&store, Period::Today);
    assert_eq!(report.total_tokens, Some(2250));
    assert_eq!(report.rows[0].sessions, 1);
    assert_eq!(report.estimated_cost_usd, None);
    assert_eq!(report.unpriced_events, 2);
    let prices:Pricing=serde_json::from_value(json!({"models":{"test-model":{"input":2.0,"output":8.0,"cache_read":0.2,"cache_write":0.0}}})).unwrap();
    let priced = store
        .report(&f.cfg, Period::Today, GroupBy::Model, now(), &prices)
        .unwrap();
    assert!((priced.estimated_cost_usd.unwrap() - 0.0051).abs() < 1e-9);
}
#[test]
fn claude_duplicates_and_event_date_not_mtime() {
    let f = Fixture::new();
    f.write(
        ".claude/projects/a/session.jsonl",
        include_str!("fixtures/claude.jsonl"),
    );
    let mut store = Store::open(&f.paths).unwrap();
    f.scan(&mut store);
    assert_eq!(f.report(&store, Period::Today).total_tokens, Some(1350));
    let week = f.report(&store, Period::SevenDays);
    assert_eq!(week.total_tokens, Some(1475));
    assert_eq!(week.rows[0].sessions, 1);
}
#[test]
fn copied_logs_do_not_double_count() {
    let f = Fixture::new();
    for path in [
        ".codex/sessions/a.jsonl",
        ".codex/archived_sessions/copied.jsonl",
    ] {
        f.write(path, include_str!("fixtures/codex.jsonl"));
    }
    let mut store = Store::open(&f.paths).unwrap();
    f.scan(&mut store);
    assert_eq!(f.report(&store, Period::Today).total_tokens, Some(2250));
}
#[test]
fn unchanged_refresh_reads_no_log_bodies() {
    let f = Fixture::new();
    f.write(
        ".codex/sessions/a.jsonl",
        include_str!("fixtures/codex.jsonl"),
    );
    let mut store = Store::open(&f.paths).unwrap();
    assert!(f.scan(&mut store).bytes_read > 0);
    assert_eq!(f.scan(&mut store).bytes_read, 0);
    drop(store);
    let mut store = Store::open(&f.paths).unwrap();
    assert_eq!(f.scan(&mut store).bytes_read, 0);
}
#[test]
fn partial_last_line_is_retried_without_losing_events() {
    let f = Fixture::new();
    let content = include_str!("fixtures/codex.jsonl");
    let split = content.rfind("cached_input_tokens").unwrap() + 8;
    f.write(".codex/sessions/a.jsonl", &content[..split]);
    let mut store = Store::open(&f.paths).unwrap();
    assert_eq!(f.scan(&mut store).incomplete_files, 1);
    assert_eq!(f.report(&store, Period::Today).total_tokens, Some(1100));
    fs::OpenOptions::new()
        .append(true)
        .open(f.root.join(".codex/sessions/a.jsonl"))
        .unwrap()
        .write_all(&content.as_bytes()[split..])
        .unwrap();
    f.scan(&mut store);
    assert_eq!(f.report(&store, Period::Today).total_tokens, Some(2250));
}
#[test]
fn truncate_and_rewrite_does_not_double_count() {
    let f = Fixture::new();
    f.write(
        ".codex/sessions/a.jsonl",
        include_str!("fixtures/codex.jsonl"),
    );
    let mut store = Store::open(&f.paths).unwrap();
    f.scan(&mut store);
    f.write(
        ".codex/sessions/a.jsonl",
        include_str!("fixtures/codex.jsonl")
            .lines()
            .take(3)
            .collect::<Vec<_>>()
            .join("\n")
            .as_str(),
    );
    f.scan(&mut store);
    assert_eq!(f.report(&store, Period::Today).total_tokens, Some(2250));
}
#[test]
fn malformed_and_oversized_records_are_partial_not_zero() {
    let f = Fixture::new();
    f.write(
        ".codex/sessions/a.jsonl",
        &format!(
            "{}\n{}\n{}",
            "bad json",
            "x".repeat(1024 * 1024 + 1),
            include_str!("fixtures/codex.jsonl")
        ),
    );
    let mut store = Store::open(&f.paths).unwrap();
    let scan = f.scan(&mut store);
    assert_eq!(scan.errors, 2);
    let report = f.report(&store, Period::Today);
    assert!(report.partial);
    assert_eq!(report.total_tokens, Some(2250));
}
#[test]
fn unknown_schemas_and_disabled_sources_do_not_claim_zero() {
    let mut f = Fixture::new();
    f.write(".grok/session.jsonl", "{\"text\":\"no token fields\"}\n");
    f.cfg.show_grok = true;
    let mut store = Store::open(&f.paths).unwrap();
    f.scan(&mut store);
    assert_eq!(f.report(&store, Period::Today).total_tokens, None);
    f.write(
        ".codex/sessions/a.jsonl",
        include_str!("fixtures/codex.jsonl"),
    );
    f.scan(&mut store);
    f.cfg.show_codex = false;
    assert_eq!(f.report(&store, Period::Today).total_tokens, None);
}
#[test]
fn database_does_not_store_content_or_full_project_paths() {
    let f = Fixture::new();
    f.write(
        ".claude/projects/a/session.jsonl",
        include_str!("fixtures/claude.jsonl"),
    );
    let mut store = Store::open(&f.paths).unwrap();
    f.scan(&mut store);
    drop(store);
    for entry in fs::read_dir(&f.paths.data).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("This content must never be persisted"));
        assert!(!text.contains("/private/work/aitop"));
    }
}
#[test]
fn damaged_config_is_preserved() {
    let f = Fixture::new();
    f.write(".config/aitop/config.json", "{broken");
    assert!(f.paths.load().is_err());
    assert!(f.paths.save(&f.cfg).is_err());
    assert_eq!(fs::read_to_string(&f.paths.config).unwrap(), "{broken");
}
#[test]
fn old_config_and_interval_survive_migration() {
    let f = Fixture::new();
    f.write(
        ".config/aitop/config.json",
        r#"{"initialized":true,"refresh_interval_secs":60,"bar_mode":"Used","show_codex":true}"#,
    );
    let cfg = f.paths.load().unwrap();
    assert_eq!(cfg.refresh_interval_secs, 60);
    assert_eq!(cfg.bar_mode, BarMode::Used);
    assert!(cfg.show_codex);
    f.paths.save(&cfg).unwrap();
    assert_eq!(f.paths.load().unwrap().refresh_interval_secs, 60);
}
#[test]
fn future_configuration_is_not_replaced() {
    let f = Fixture::new();
    f.write(".config/aitop/config.json", r#"{"schema_version":99}"#);
    assert!(f.paths.save(&f.cfg).is_err());
}
#[test]
fn cancellation_rolls_back_history_cursor() {
    let f = Fixture::new();
    f.write(
        ".codex/sessions/a.jsonl",
        include_str!("fixtures/codex.jsonl"),
    );
    let mut store = Store::open(&f.paths).unwrap();
    assert!(store
        .scan(&f.paths, &f.cfg, &AtomicBool::new(true))
        .is_err());
    f.scan(&mut store);
    assert_eq!(f.report(&store, Period::Today).total_tokens, Some(2250));
}
#[test]
fn unknown_quota_fields_never_become_full_remaining() {
    for q in [
        parser::claude::usage(&json!({"five_hour":{},"seven_day":{}}), 100, "test"),
        parser::codex::api_usage(&json!({"rate_limit":{"primary_window":{}}}), 100),
        parser::extras::cursor_usage(&json!({"numRequests":0}), 100),
        parser::extras::copilot_usage(&json!({}), 100),
    ] {
        assert_ne!(q.availability, Availability::Available);
        assert!(q.windows.is_empty());
    }
}
#[test]
fn optional_codex_windows_and_multiple_buckets() {
    let q = parser::codex::rpc_usage(
        &json!({"result":{"rateLimitsByLimitId":{"a":{"primary":{"usedPercent":25,"windowDurationMins":15,"resetsAt":999}},"b":{"secondary":{"usedPercent":42,"windowDurationMins":60}}}}}),
        100,
    );
    assert_eq!(q.windows.len(), 2);
    assert_eq!(q.windows[0].window_minutes, Some(15));
    assert_eq!(q.windows[0].label, "a 15m");
}
#[test]
fn reset_does_not_automatically_refill_quota() {
    let w = QuotaWindow::new("a", "5h", 85.0, Some(100), Some(300)).unwrap();
    assert!(w.expired(101));
    assert_eq!(w.percent(BarMode::Remaining), 15.0);
    let mut q = parser::measured(Provider::Claude, "test", vec![w], 1);
    q.observed_at = Some(1);
    assert!(q.stale(101));
}

#[test]
fn unknown_reset_displays_the_window_maximum_without_inventing_a_reset_time() {
    let mut w = QuotaWindow::new("5h", "5h", 30.0, None, Some(300)).unwrap();
    assert_eq!(w.reset_label(1000), "5h max");
    assert_eq!(w.reset_label(4600), "5h max");
    assert!(w.resets_at.is_none());
    assert!(!w.expired(4600));
    assert_eq!(w.percent(BarMode::Remaining), 70.0);
    w.window_minutes = Some(10080);
    assert_eq!(w.reset_label(1000), "7d max");
    w.resets_at = Some(1120);
    assert_eq!(w.reset_label(1000), "2m");
}
#[test]
fn invalid_percent_is_not_silently_clamped() {
    for pct in [-1.0, 101.0, f64::NAN] {
        assert!(QuotaWindow::new("a", "a", pct, None, None).is_none());
    }
}
#[test]
fn statusline_preserves_available_windows_and_timestamp() {
    let q = parser::claude::statusline(
        &json!({"rate_limits":{"five_hour":{"used_percentage":0,"resets_at":999},"seven_day":null}}),
        100,
    );
    assert_eq!(q.windows.len(), 1);
    assert_eq!(q.observed_at, Some(100));
    assert_eq!(q.windows[0].used_percent, 0.0);
}
#[test]
fn first_offline_collection_uses_newly_indexed_snapshot() {
    let f = Fixture::new();
    let now = chrono::Utc::now().to_rfc3339();
    let line=json!({"type":"event_msg","timestamp":now,"payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":25,"resets_at":chrono::Utc::now().timestamp()+3600}},"info":null}}).to_string()+"\n";
    f.write(".codex/sessions/a.jsonl", &line);
    let s = runtime::collect(
        &f.paths,
        &f.cfg,
        FetchOptions {
            offline: true,
            ..Default::default()
        },
        Period::Today,
        GroupBy::Project,
        Arc::new(AtomicBool::new(false)),
    );
    let q = s
        .providers
        .iter()
        .find(|q| q.provider == Provider::Codex)
        .unwrap();
    assert_eq!(q.windows.len(), 1);
}
#[test]
fn snapshot_json_contains_counts_but_no_credentials() {
    let s = runtime::demo(Period::Today, GroupBy::Project);
    let json = serde_json::to_value(s).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["history"]["total_tokens"], 1_240_000);
    assert!(json.get("access_token").is_none());
}
#[test]
fn tui_handles_supported_and_small_sizes() {
    for (width, height) in [(120, 30), (80, 24), (40, 12), (20, 6)] {
        for view in [ui::View::Overview, ui::View::History, ui::View::Settings] {
            let backend = ratatui::backend::TestBackend::new(width, height);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            let snapshot = runtime::demo(Period::Today, GroupBy::Project);
            let cfg = Config {
                show_claude: true,
                show_codex: true,
                show_cursor: true,
                ..Default::default()
            };
            terminal
                .draw(|f| {
                    ui::render(
                        f,
                        &snapshot,
                        &cfg,
                        &ui::State {
                            view,
                            ..Default::default()
                        },
                    )
                })
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("aitop"));
            if width >= 80 && view == ui::View::Overview {
                assert!(text.contains("Claude Code"));
                assert!(text.contains("64%"));
            }
        }
    }
}

#[test]
fn agy_version_gate_excludes_prompt_fallback_versions() {
    for version in ["agy 1.1.10", "unknown", "1.1.11-preview"] {
        assert!(!parser::agy::supported_version(version));
    }
    for version in ["agy 1.1.11", "Antigravity CLI v1.4.12"] {
        assert!(parser::agy::supported_version(version));
    }
}

#[test]
fn agy_keeps_all_distinct_quota_pools() {
    let value = serde_json::from_str(include_str!("fixtures/agy-quota.json")).unwrap();
    let q = parser::agy::usage(&value, 100);
    assert_eq!(q.windows.len(), 5);
    assert!(q.windows.iter().any(|w| w.label == "Argon 7d"));
    assert!(q.windows.iter().any(|w| w.label == "Claude/GPT 5h"));
    let mut value = value;
    value["num_turns"] = json!(1);
    assert!(parser::agy::usage(&value, 100).windows.is_empty());
}

#[test]
fn json_exposes_stale_observation_and_refresh_failure() {
    let now = chrono::Local::now().timestamp();
    let mut q = parser::measured(
        Provider::Codex,
        "test",
        vec![QuotaWindow::new("a", "5h", 25.0, None, Some(300)).unwrap()],
        now - 1000,
    );
    q.refresh_error = Some("Timed out".into());
    let value = serde_json::to_value(&q).unwrap();
    assert_eq!(value["freshness"], "stale");
    assert_eq!(value["refresh_error"], "Timed out");
    assert_eq!(value["observed_at"], now - 1000);
}

#[test]
fn failed_refresh_preserves_last_observation() {
    let mut snapshot = runtime::demo(Period::Today, GroupBy::Project);
    let before = snapshot.providers[0].observed_at;
    runtime::apply(
        &mut snapshot,
        runtime::Update::Quota(aitop::models::QuotaSnapshot::unavailable(
            Provider::Claude,
            Availability::Error,
            "Timed out",
            chrono::Local::now().timestamp(),
        )),
    );
    let q = &snapshot.providers[0];
    assert_eq!(q.observed_at, before);
    assert_eq!(q.windows.len(), 2);
    assert_eq!(q.refresh_error.as_deref(), Some("Timed out"));
}

#[test]
fn equal_project_basenames_are_not_merged() {
    let f = Fixture::new();
    let original = include_str!("fixtures/claude.jsonl");
    f.write(".claude/projects/a/one.jsonl", original);
    f.write(
        ".claude/projects/b/two.jsonl",
        &original
            .replace("claude-test", "other-session")
            .replace("/private/work/aitop", "/another/place/aitop"),
    );
    let mut store = Store::open(&f.paths).unwrap();
    f.scan(&mut store);
    let report = f.report(&store, Period::Today);
    assert_eq!(report.rows.len(), 2);
    assert_ne!(report.rows[0].label, report.rows[1].label);
    assert_eq!(report.total_tokens, Some(2700));
}

#[cfg(unix)]
fn fake_agy(f: &Fixture, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    f.write(
        ".local/bin/agy",
        &format!("#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 1.5.3; else\n{body}\nfi\n"),
    );
    fs::set_permissions(
        f.root.join(".local/bin/agy"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
}
#[cfg(unix)]
fn agy_fixture_body() -> String {
    format!(
        "cat <<'QUOTA'\n{}\nQUOTA",
        include_str!("fixtures/agy-quota.json")
    )
}
#[cfg(unix)]
fn agy_only() -> Fixture {
    let mut f = Fixture::new();
    f.cfg.show_claude = false;
    f.cfg.show_codex = false;
    f.cfg.show_agy = true;
    f
}
#[cfg(unix)]
fn collect_agy(f: &Fixture) -> aitop::models::QuotaSnapshot {
    runtime::collect(
        &f.paths,
        &f.cfg,
        FetchOptions::default(),
        Period::Today,
        GroupBy::Project,
        Arc::new(AtomicBool::new(false)),
    )
    .providers
    .remove(0)
}

#[test]
#[cfg(unix)]
fn agy_queries_automatically_and_recovers_from_a_query_disabled_cache() {
    let f = agy_only();
    fake_agy(&f, &agy_fixture_body());
    runtime::cache_observation(
        &f.paths,
        aitop::models::QuotaSnapshot::unavailable(
            Provider::Agy,
            Availability::NoData,
            "Use --cli or --agy-cli to opt in",
            Local::now().timestamp(),
        ),
    )
    .unwrap();
    let q = collect_agy(&f);
    assert_eq!(q.availability, Availability::Available);
    assert_eq!(q.windows.len(), 5);
    assert!(q.refresh_error.is_none());
}

#[test]
#[cfg(unix)]
fn agy_accepts_a_valid_response_that_takes_more_than_six_seconds() {
    let f = agy_only();
    fake_agy(&f, &format!("sleep 6.2\n{}", agy_fixture_body()));
    let q = collect_agy(&f);
    assert_eq!(q.availability, Availability::Available);
    assert_eq!(q.windows.len(), 5);
}

#[test]
#[cfg(unix)]
fn agy_shortens_old_backoff_preserves_observations_and_recovers() {
    let f = agy_only();
    let now = Local::now().timestamp();
    let mut q = parser::agy::usage(
        &serde_json::from_str(include_str!("fixtures/agy-quota.json")).unwrap(),
        now - 3600,
    );
    q.fetched_at = now - 61;
    q.retry_after = now + 1920;
    q.refresh_error =
        Some("agy quota probe failed or timed out; check CLI version and sign-in".into());
    runtime::cache_observation(&f.paths, q).unwrap();
    let path = f.paths.data.join("quota-agy.json");
    let mut cache: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    cache["failures"] = json!(5);
    fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
    fake_agy(&f, "echo PRIVATE_DIAGNOSTIC >&2; exit 1");
    let failed = collect_agy(&f);
    assert_eq!(failed.observed_at, Some(now - 3600));
    assert_eq!(failed.windows.len(), 5);
    assert!(failed
        .refresh_error
        .as_deref()
        .unwrap()
        .contains("exited unsuccessfully"));
    assert!(!failed
        .refresh_error
        .as_deref()
        .unwrap()
        .contains("PRIVATE_DIAGNOSTIC"));
    assert_eq!(failed.retry_after - failed.fetched_at, 60);
    fake_agy(&f, &agy_fixture_body());
    let cached = collect_agy(&f);
    assert_eq!(cached.observed_at, failed.observed_at);
    assert!(cached.refresh_error.is_some());
    let mut cache: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(cache["failures"], 6);
    cache["quota"]["fetched_at"] = json!(Local::now().timestamp() - 61);
    fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
    let recovered = collect_agy(&f);
    assert!(recovered.observed_at.unwrap() > now - 10);
    assert!(recovered.refresh_error.is_none());
    let cache: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(cache["failures"], 0);
}

#[test]
fn agy_failed_report_keeps_a_useful_error_message() {
    let q = parser::agy::usage(
        &json!({"status":"ERROR","command":{"name":"usage"}}),
        now().timestamp(),
    );
    assert_eq!(q.availability, Availability::Error);
    assert!(q.message.contains("no recognized quota fields"));
}

#[test]
#[cfg(unix)]
fn manual_refresh_bypasses_recent_success_and_failure_caches_but_offline_stays_local() {
    let f = agy_only();
    fake_agy(&f, &agy_fixture_body());
    let original = collect_agy(&f);
    assert!(original.refresh_error.is_none());
    // The same provider now fails. Automatic refresh still uses the successful cached result.
    fake_agy(&f, "exit 1");
    assert!(collect_agy(&f).refresh_error.is_none());
    let manual = |offline| {
        runtime::collect(
            &f.paths,
            &f.cfg,
            FetchOptions {
                force_refresh: true,
                offline,
                ..Default::default()
            },
            Period::Today,
            GroupBy::Project,
            Arc::new(AtomicBool::new(false)),
        )
        .providers
        .remove(0)
    };
    let failed = manual(false);
    assert!(failed
        .refresh_error
        .as_deref()
        .unwrap()
        .contains("exited unsuccessfully"));
    assert_eq!(failed.observed_at, original.observed_at);
    fake_agy(&f, &agy_fixture_body());
    assert!(collect_agy(&f).refresh_error.is_some());
    assert!(manual(true).refresh_error.is_some());
    let fresh = manual(false);
    assert!(fresh.refresh_error.is_none());
    assert_eq!(fresh.windows.len(), 5);
}

#[test]
#[cfg(unix)]
fn agy_calendar_uses_canonical_transcript_and_exclusive_cache_counts() {
    let f = agy_only();
    let event=json!({"type":"step","created_at":now().to_rfc3339(),"step_index":1,"input_tokens":12,"output_tokens":7,"cache_read_tokens":1000}).to_string();
    // A final, valid JSON object does not require a trailing newline.
    f.write(
        ".gemini/antigravity-cli/brain/session/transcript.jsonl",
        &event,
    );
    f.write(
        ".gemini/antigravity-cli/brain/session/transcript_full.jsonl",
        &event,
    );
    f.write(
        ".gemini/antigravity-cli/brain/session/steps/00000001.jsonl",
        &event,
    );
    let mut store = Store::open(&f.paths).unwrap();
    let scan = f.scan(&mut store);
    assert_eq!(scan.files, 1);
    assert_eq!(scan.errors, 0);
    assert_eq!(scan.incomplete_files, 0);
    let usage = store
        .provider_usage(Provider::Agy, now(), &Pricing::default())
        .unwrap();
    assert!(!usage.partial);
    assert_eq!(usage.periods[0].totals.tokens.total(), 1019);
    assert_eq!(usage.periods[0].totals.tokens.input, 12);
    assert_eq!(usage.periods[0].totals.tokens.cache_read, 1000);
    assert_eq!(
        usage.days.iter().map(|d| d.tokens.total()).sum::<u64>(),
        1019
    );
    assert_eq!(f.scan(&mut store).bytes_read, 0);
}

#[test]
#[cfg(unix)]
fn agy_index_upgrade_archives_old_counts_and_leaves_other_providers_intact() {
    let f = agy_only();
    drop(Store::open(&f.paths).unwrap());
    let path = f.paths.data.join("history.sqlite3");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute(
            "INSERT INTO events VALUES('old-agy','agy',?1,'s','p','m',20,1,0,0)",
            [now().timestamp()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO events VALUES('keep-codex','codex',?1,'s','p','m',100,0,0,0)",
            [now().timestamp()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO files VALUES('old-file','agy','i','m',1,1,'{}',5)",
            [],
        )
        .unwrap();
        c.pragma_update(None, "user_version", 1).unwrap();
    }
    let store = Store::open(&f.paths).unwrap();
    let mut cfg = f.cfg.clone();
    cfg.show_codex = true;
    assert_eq!(
        store
            .report(
                &cfg,
                Period::Today,
                GroupBy::Provider,
                now(),
                &Pricing::default()
            )
            .unwrap()
            .total_tokens,
        Some(100)
    );
    drop(store);
    drop(Store::open(&f.paths).unwrap());
    let c = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT SUM(input+output+cache_read+cache_write) FROM agy_legacy_events",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        21
    );
    assert_eq!(
        c.query_row("SELECT SUM(errors) FROM agy_legacy_files", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        5
    );
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
}

#[test]
fn calendar_collection_errors_stay_scoped_to_their_provider() {
    let f = Fixture::new();
    f.write(".claude/projects/a/damaged.jsonl", "damaged json\n");
    f.write(
        ".codex/sessions/valid.jsonl",
        include_str!("fixtures/codex.jsonl"),
    );
    let options = FetchOptions {
        offline: true,
        ..Default::default()
    };
    let mut snapshot =
        runtime::initial(&f.paths, &f.cfg, options, Period::Today, GroupBy::Provider);
    for update in runtime::start_filtered(
        f.paths.clone(),
        f.cfg.clone(),
        options,
        Period::Today,
        GroupBy::Provider,
        Arc::new(AtomicBool::new(false)),
        Some(Provider::Codex),
    ) {
        runtime::apply(&mut snapshot, update);
    }
    assert!(!snapshot.history.partial);
    assert!(
        snapshot
            .provider_usage
            .iter()
            .find(|u| u.provider == Provider::Claude)
            .unwrap()
            .partial
    );
    assert!(
        !snapshot
            .provider_usage
            .iter()
            .find(|u| u.provider == Provider::Codex)
            .unwrap()
            .partial
    );
}

#[test]
fn saved_keychain_permission_is_resolved_without_changing_other_fetch_options() {
    let mut cfg = Config::default();
    let options = FetchOptions {
        offline: true,
        force_refresh: true,
        ..Default::default()
    };
    assert!(!options.configured(&cfg).keychain);
    cfg.claude_keychain = true;
    let resolved = options.configured(&cfg);
    assert!(resolved.keychain && resolved.offline && resolved.force_refresh);
    cfg.claude_keychain = false;
    assert!(!options.configured(&cfg).keychain);
}

#[test]
fn official_prices_work_without_a_user_file_and_exact_overrides_remain_optional() {
    let f = Fixture::new();
    let prices = Pricing::load(&f.paths).unwrap();
    let tokens = aitop::models::Tokens {
        input: 1_000_000,
        output: 100_000,
        cache_read: 2_000_000,
        cache_write: 100_000,
    };
    assert!((prices.cost("gpt-6.1-sol", &tokens).unwrap() - 3.45).abs() < 1e-10);
    assert!((prices.cost("claude-opus-5-5", &tokens).unwrap() - 6.9).abs() < 1e-10);
    assert_eq!(prices.cost("Unknown", &tokens), None);
    assert_eq!(prices.cost("<synthetic>", &Default::default()), Some(0.0));
    f.write(
        ".config/aitop/prices.json",
        r#"{"models":{"gpt-6.1-sol":{"input":1,"output":1,"cache_read":1,"cache_write":1}}}"#,
    );
    let prices = Pricing::load(&f.paths).unwrap();
    assert!((prices.cost("gpt-6.1-sol", &tokens).unwrap() - 3.2).abs() < 1e-10);
    assert!(prices.cost("claude-opus-5-5", &tokens).is_some());
    assert!(prices.source().contains("local overrides"));
}

#[test]
fn automatic_tiered_prices_are_calculated_per_request_before_period_and_hour_sums() {
    let mut f = Fixture::new();
    f.cfg.show_codex = false;
    let make = |id: &str, cache: u64| {
        json!({"type":"assistant","sessionId":"pricing-test","timestamp":now().to_rfc3339(),"message":{"id":id,"model":"claude-haiku-5-5","usage":{"input_tokens":60000,"output_tokens":0,"cache_read_input_tokens":cache}}}).to_string()
    };
    f.write(
        ".claude/projects/a/usage.jsonl",
        &format!(
            "{}\n{}\n{}\n",
            make("a", 20000),
            make("b", 20000),
            make("c", 40001)
        ),
    );
    let mut store = Store::open(&f.paths).unwrap();
    f.scan(&mut store);
    let pricing = Pricing::load(&f.paths).unwrap();
    let expected = 2.0 * (60000.0 * 0.1 + 20000.0 * 0.01) / 1_000_000.0
        + (60000.0 * 0.5 + 40001.0 * 0.05) / 1_000_000.0;
    let report = store
        .report(&f.cfg, Period::Today, GroupBy::Model, now(), &pricing)
        .unwrap();
    let detail = store
        .provider_usage(Provider::Claude, now(), &pricing)
        .unwrap();
    assert!((report.estimated_cost_usd.unwrap() - expected).abs() < 1e-10);
    assert!((detail.periods[0].totals.estimated_cost_usd.unwrap() - expected).abs() < 1e-10);
    assert!(
        (detail
            .hours
            .iter()
            .map(|h| h.totals.estimated_cost_usd.unwrap())
            .sum::<f64>()
            - expected)
            .abs()
            < 1e-10
    );
}

#[test]
fn missing_claude_access_keeps_the_observation_and_reports_why_it_cannot_refresh() {
    let mut f = Fixture::new();
    f.cfg.show_codex = false;
    let observed = Local::now().timestamp() - 3600;
    let q = parser::claude::usage(
        &json!({"five_hour":{"utilization":30.0,"resets_at":1999999999}}),
        observed,
        "oauth-api (experimental)",
    );
    runtime::cache_observation(&f.paths, q).unwrap();
    let snapshot = runtime::collect(
        &f.paths,
        &f.cfg,
        FetchOptions {
            force_refresh: true,
            ..Default::default()
        },
        Period::Today,
        GroupBy::Project,
        Arc::new(AtomicBool::new(false)),
    );
    let q = &snapshot.providers[0];
    assert_eq!(q.observed_at, Some(observed));
    assert_eq!(q.windows.len(), 1);
    assert!(q.refresh_error.as_deref().unwrap().contains("Keychain"));
    assert_eq!(
        q.status_label(Local::now().timestamp()),
        "Refresh failed / cached"
    );
}
