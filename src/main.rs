use aitop::{
    config::{Config, Paths},
    history::{Pricing, Store},
    models::{Availability, BarMode, GroupBy, Period, Provider, Result},
    output,
    parser::{self, FetchOptions},
    runtime, ui,
};
use clap::{Parser, Subcommand};
use std::{
    io::{self, IsTerminal, Read},
    path::PathBuf,
    process::ExitCode,
    sync::{atomic::AtomicBool, Arc},
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "AI coding quotas and local token history in your terminal"
)]
struct Args {
    /// User home to inspect (useful for fixtures and portable installations)
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// aitop-owned cache directory
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Refresh seconds; saves to configuration as in aitop 0.1
    #[arg(short, long, global = true)]
    interval: Option<u64>,
    /// Print once without entering the TUI
    #[arg(long, global = true)]
    once: bool,
    /// Structured output, schema version 1
    #[arg(long, global = true)]
    json: bool,
    /// Read local history and stored quota snapshots without vendor requests
    #[arg(long, global = true)]
    offline: bool,
    /// Allow Claude macOS Keychain access and remember the permission
    #[arg(long, global = true)]
    keychain: bool,
    /// Use the Codex App Server instead of its default API source
    #[arg(long, global = true)]
    cli: bool,
    /// Compatibility flag; enabled AGY providers now query automatically
    #[arg(long, global = true)]
    agy_cli: bool,
    /// ASCII-only TUI symbols
    #[arg(long, global = true)]
    ascii: bool,
    /// Disable TUI colors (also honors NO_COLOR)
    #[arg(long, global = true)]
    no_color: bool,
    /// Force TUI colors for this run, overriding NO_COLOR and the saved preference
    #[arg(long, global = true, conflicts_with = "no_color")]
    color: bool,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Debug, Subcommand)]
enum Command {
    /// Print quotas and today's local summary
    Status {
        #[arg(long)]
        provider: Option<Provider>,
    },
    /// Show local usage by period and grouping
    History {
        #[arg(long, value_enum, default_value = "today")]
        period: Period,
        #[arg(long, value_enum, default_value = "project")]
        group_by: GroupBy,
        #[arg(long)]
        provider: Option<Provider>,
    },
    /// List available sources and their limitations
    Providers,
    /// Diagnose configuration and local source availability
    Doctor {
        #[arg(long)]
        network: bool,
    },
    /// Inspect configuration or change a setting
    Config {
        #[command(subcommand)]
        action: Option<ConfigCommand>,
    },
    /// Explore synthetic data; never reads credentials or writes configuration
    Demo,
    /// Capture Claude statusline JSON from stdin, without changing Claude settings
    Ingest {
        #[arg(value_enum)]
        provider: Provider,
    },
    /// A cached, short summary for tmux (no scanning or vendor requests)
    Statusline {
        #[arg(long)]
        cached: bool,
    },
}
#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Set { key: String, value: String },
}

fn main() -> ExitCode {
    match execute(Args::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("aitop: {error}");
            ExitCode::from(2)
        }
    }
}
fn execute(args: Args) -> Result<u8> {
    let cancel = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, cancel.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, cancel.clone())?;
    let home = args
        .home
        .clone()
        .or_else(dirs::home_dir)
        .ok_or("Cannot locate a home directory; pass --home")?;
    let paths = Paths::new(home, args.data_dir.clone());
    let options = FetchOptions {
        offline: args.offline,
        // --keychain is persisted below; read the saved setting so it can also be revoked in Settings.
        keychain: false,
        cli: args.cli,
        agy_cli: args.agy_cli,
        force_refresh: false,
    };
    let is_demo = matches!(args.command, Some(Command::Demo));
    // Demo has no dependency on the real user's settings, data or authentication.
    let mut cfg = if is_demo {
        Config {
            show_claude: true,
            show_codex: true,
            show_cursor: true,
            ..Config::default()
        }
    } else {
        paths.load()?
    };
    let display = ui::DisplayOptions {
        ascii: args.ascii,
        no_color: args.no_color || std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
        color: args.color,
    };
    if args.keychain && !is_demo && !cfg.claude_keychain {
        cfg.claude_keychain = true;
        paths.save(&cfg)?;
    }
    if let Some(seconds) = args.interval {
        if seconds > 86400 {
            return Err("Interval must not exceed 86400 seconds".into());
        }
        cfg.refresh_interval_secs = seconds.max(5);
        if !is_demo {
            paths.save(&cfg)?;
        }
    }
    match args.command {
        Some(Command::Demo) => {
            if args.once || args.json || !io::stdout().is_terminal() || !io::stdin().is_terminal() {
                print_snapshot(
                    &runtime::demo(Period::Today, GroupBy::Project),
                    args.json,
                    cfg.bar_mode,
                )?;
            } else {
                ui::run(paths, cfg, options, cancel, true, display)?;
            }
            Ok(0)
        }
        Some(Command::Providers) => {
            let entries=Provider::ALL.map(|p|serde_json::json!({"provider":p,"enabled":cfg.enabled(p),"local_history":p.supports_history(),"quota_source":match p {Provider::Claude=>"statusline capture / experimental OAuth (Keychain opt-in)",Provider::Codex=>"local snapshot / experimental OAuth / opt-in App Server",Provider::Agy=>"experimental opt-in structured CLI; strict schema",Provider::Cursor=>"experimental legacy profile; strict schema",Provider::Copilot=>"experimental internal API; strict schema",Provider::Grok=>"unavailable; never inferred"},"verification":"fixture-tested; live account compatibility not certified"}));
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"schema_version":1,"providers":entries})
                    )?
                );
            } else {
                for e in entries {
                    println!(
                        "{:<10} {}\n  {}",
                        e["provider"].as_str().unwrap_or(""),
                        if e["enabled"].as_bool() == Some(true) {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        e["quota_source"].as_str().unwrap_or("")
                    );
                }
            }
            Ok(0)
        }
        Some(Command::Doctor { network }) => {
            let checks=Provider::ALL.map(|p|{
                let roots=aitop::history::roots(&paths,p);
                serde_json::json!({"provider":p,"enabled":cfg.enabled(p),"history_directories_present":roots.iter().filter(|p|p.is_dir()).count(),"history_supported":p.supports_history()})
            });
            let prices_ok = Pricing::load(&paths).is_ok();
            let storage_ok = Store::open(&paths).is_ok();
            let network_snapshot = network.then(|| {
                runtime::collect(
                    &paths,
                    &cfg,
                    options,
                    Period::Today,
                    GroupBy::Project,
                    cancel,
                )
            });
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"schema_version":1,"config":"valid","pricing_valid":prices_ok,"storage_writable":storage_ok,"network_requested":network && !args.offline,"providers":checks,"network_result":network_snapshot})
                    )?
                );
            } else {
                println!(
                    "Config: valid\nPricing: {}\nStorage: {}",
                    if prices_ok {
                        "valid (missing rates stay unknown)"
                    } else {
                        "invalid prices.json"
                    },
                    if storage_ok {
                        "writable"
                    } else {
                        "unavailable"
                    }
                );
                for check in checks {
                    println!(
                        "{}: {} local history directories; {}",
                        check["provider"].as_str().unwrap_or(""),
                        check["history_directories_present"],
                        if check["enabled"].as_bool() == Some(true) {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    );
                }
                println!("No credentials displayed. Local diagnostics do not open Keychain or start provider commands.");
            }
            if let Some(snapshot) = network_snapshot {
                if !args.json {
                    print!("{}", output::status(&snapshot, cfg.bar_mode));
                }
                return Ok(u8::from(
                    output::failed(&snapshot) || !prices_ok || !storage_ok,
                ));
            }
            Ok(u8::from(!prices_ok || !storage_ok))
        }
        Some(Command::Config { action }) => {
            if let Some(ConfigCommand::Set { key, value }) = action {
                match key.as_str() {
                    "refresh_interval" => {
                        cfg.refresh_interval_secs = value
                            .parse()
                            .map_err(|_| "refresh_interval must be an integer")?;
                    }
                    "bar_mode" => {
                        cfg.bar_mode = match value.to_lowercase().as_str() {
                            "remaining" => BarMode::Remaining,
                            "used" => BarMode::Used,
                            _ => return Err("bar_mode must be remaining or used".into()),
                        }
                    }
                    "ascii" => {
                        cfg.ascii = value.parse().map_err(|_| "ascii must be true or false")?
                    }
                    "no_color" => {
                        cfg.no_color = value
                            .parse()
                            .map_err(|_| "no_color must be true or false")?
                    }
                    "claude_keychain" => {
                        cfg.claude_keychain = value
                            .parse()
                            .map_err(|_| "claude_keychain must be true or false")?;
                    }
                    p => {
                        let provider = Provider::ALL
                            .into_iter()
                            .find(|v| v.id() == p)
                            .ok_or("Unknown configuration key")?;
                        let enabled: bool = value
                            .parse()
                            .map_err(|_| "Provider setting must be true or false")?;
                        if cfg.enabled(provider) != enabled {
                            cfg.toggle(provider);
                        }
                    }
                }
                paths.save(&cfg)?;
            }
            println!("{}", serde_json::to_string_pretty(&cfg)?);
            Ok(0)
        }
        Some(Command::Ingest { provider }) => {
            if provider != Provider::Claude {
                return Err("Only Claude statusline ingestion is supported".into());
            }
            let mut bytes = Vec::new();
            io::stdin().take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 1024 * 1024 {
                return Err("Statusline input exceeds 1 MiB".into());
            }
            let value = serde_json::from_slice(&bytes).map_err(|_| "Invalid statusline JSON")?;
            let q = parser::claude::statusline(&value, chrono::Local::now().timestamp());
            if q.availability == Availability::Available {
                Store::open(&paths)?.save_quota(&q)?;
                runtime::cache_observation(&paths, q.clone())?;
            }
            let text = q
                .windows
                .iter()
                .map(|w| format!("{} {:.0}% left", w.label, 100.0 - w.used_percent))
                .collect::<Vec<_>>()
                .join(" | ");
            println!(
                "{}",
                if text.is_empty() {
                    "Claude limits unavailable"
                } else {
                    &text
                }
            );
            Ok(0)
        }
        Some(Command::Statusline { .. }) => {
            let now = chrono::Local::now().timestamp();
            let text = Provider::ALL
                .into_iter()
                .filter(|&p| cfg.enabled(p))
                .filter_map(|p| runtime::cached_quota(&paths, p, options))
                .map(|q| {
                    let window = q.windows.first();
                    match window {
                        Some(w) if !w.expired(now) => format!(
                            "{} {:.0}%{}",
                            q.provider.name(),
                            100.0 - w.used_percent,
                            if q.stale(now) || q.refresh_error.is_some() {
                                " stale"
                            } else {
                                ""
                            }
                        ),
                        _ => format!("{} --", q.provider.name()),
                    }
                })
                .collect::<Vec<_>>()
                .join(" | ");
            println!(
                "{}",
                if text.is_empty() {
                    "aitop: no cached quotas"
                } else {
                    &text
                }
            );
            Ok(0)
        }
        Some(Command::History {
            period,
            group_by,
            provider,
        }) => {
            if let Some(p) = provider {
                filter(&mut cfg, p);
            }
            // History never reads credentials or invokes a provider.
            let mut store = Store::open(&paths)?;
            let scan = store.scan(&paths, &cfg, &cancel)?;
            let pricing = Pricing::load(&paths);
            let mut report = store.report(
                &cfg,
                period,
                group_by,
                chrono::Local::now(),
                pricing.as_ref().unwrap_or(&Pricing::default()),
            )?;
            if pricing.is_err() {
                report
                    .warnings
                    .push("Invalid prices.json; costs unavailable".into());
            }
            report.partial |= scan.errors > 0 || scan.incomplete_files > 0;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"schema_version":1,"history":report})
                    )?
                );
            } else {
                print!("{}", output::history(&report));
            }
            Ok(u8::from(report.partial || pricing.is_err()))
        }
        Some(Command::Status { provider }) => {
            if let Some(p) = provider {
                filter(&mut cfg, p);
            }
            let snapshot = runtime::collect(
                &paths,
                &cfg,
                options,
                Period::Today,
                GroupBy::Project,
                cancel,
            );
            print_snapshot(&snapshot, args.json, cfg.bar_mode)?;
            Ok(u8::from(output::failed(&snapshot)))
        }
        None => {
            if args.once || args.json || !io::stdout().is_terminal() || !io::stdin().is_terminal() {
                let snapshot = runtime::collect(
                    &paths,
                    &cfg,
                    options,
                    Period::Today,
                    GroupBy::Project,
                    cancel,
                );
                print_snapshot(&snapshot, args.json, cfg.bar_mode)?;
                Ok(u8::from(output::failed(&snapshot)))
            } else {
                ui::run(paths, cfg, options, cancel, false, display)?;
                Ok(0)
            }
        }
    }
}
fn filter(cfg: &mut Config, provider: Provider) {
    for p in Provider::ALL {
        if cfg.enabled(p) != (p == provider) {
            cfg.toggle(p);
        }
    }
}
fn print_snapshot(snapshot: &aitop::models::Snapshot, json: bool, mode: BarMode) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(snapshot)?);
    } else {
        print!("{}", output::status(snapshot, mode));
    }
    for warning in snapshot
        .warnings
        .iter()
        .chain(snapshot.history.warnings.iter())
    {
        eprintln!("aitop: {warning}");
    }
    Ok(())
}
