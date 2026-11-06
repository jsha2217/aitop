mod models;
mod parser;
mod ui;

use std::error::Error;
use std::fs;
use std::io::{self, stdout};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Local;
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
    },
};
use ratatui::{backend::CrosstermBackend, Terminal};
use models::{format_number, AgentSummary, AllUsageStats, AppConfig, BarMode};

#[derive(Parser, Debug)]
#[command(
    name = "aitop",
    author = "windr",
    version = "0.1.0",
    about = "⚡ Real-time Token & Cost Monitor for AI Coding Agents"
)]
struct Args {
    /// Refresh interval in seconds (overrides saved setting)
    #[arg(short, long)]
    interval: Option<u64>,

    /// Print current statistics once and exit (no interactive TUI)
    #[arg(long)]
    once: bool,
}

fn config_file_path(home_dir: &PathBuf) -> PathBuf {
    home_dir.join(".config").join("aitop").join("config.json")
}

fn load_config(home_dir: &PathBuf) -> AppConfig {
    let path = config_file_path(home_dir);
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<AppConfig>(&content) {
            if cfg.initialized {
                return cfg;
            }
        }
    }
    // First run after install: auto-detect which AI tools have local Auth on this Mac
    let detected = parser::detect_authenticated_agents(home_dir);
    save_config(home_dir, &detected);
    detected
}

fn save_config(home_dir: &PathBuf, cfg: &AppConfig) {
    let path = config_file_path(home_dir);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json_str) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(path, json_str);
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let home_dir = dirs::home_dir().expect("Unable to find user home directory");
    let mut cfg = load_config(&home_dir);

    if let Some(secs) = args.interval {
        cfg.refresh_interval_secs = secs.max(5);
        save_config(&home_dir, &cfg);
    }

    // Option 1: One-shot CLI output
    if args.once {
        let stats = parser::collect_all_stats(&home_dir, &cfg);
        println!("============================================================");
        println!("⚡ aitop - AI Coding Agent Usage Summary");
        println!("Updated at: {}", stats.last_updated);
        println!("------------------------------------------------------------");
        println!("{:<18} | {:<10} | {:<15} | {:<10}", "Agent", "Sessions", "Total Tokens", "Est. Cost");
        println!("------------------------------------------------------------");
        let list = [
            (cfg.show_claude, &stats.claude),
            (cfg.show_codex, &stats.codex),
            (cfg.show_agy, &stats.agy),
            (cfg.show_cursor, &stats.cursor),
            (cfg.show_copilot, &stats.copilot),
            (cfg.show_grok, &stats.grok),
        ];
        for (enabled, agent) in list {
            if !enabled {
                continue;
            }
            println!(
                "{:<18} | {:<10} | {:<15} | ${:<10.4}",
                agent.name,
                agent.session_count,
                format_number(agent.tokens.total_tokens),
                agent.tokens.estimated_cost_usd
            );
            if !agent.usage.session_usage_label.is_empty() {
                println!("  ├─ {}", agent.usage.session_usage_label);
            }
            if !agent.usage.weekly_usage_label.is_empty() {
                println!("  └─ {}", agent.usage.weekly_usage_label);
            }
        }
        println!("------------------------------------------------------------");
        println!(
            "{:<18} | {:<10} | {:<15} | ${:<10.4}",
            "TOTAL",
            "-",
            format_number(stats.total_tokens),
            stats.total_cost_usd
        );
        println!("============================================================");
        return Ok(());
    }

    // Option 2: Full interactive TUI
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, SetTitle("aitop"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_tui(&mut terminal, &home_dir, cfg);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("Error: {:?}", err);
    }

    Ok(())
}

fn spawn_agent_fetch(
    agent_idx: usize,
    home_dir: &PathBuf,
    stats: &mut AllUsageStats,
    loading_mask: &mut [bool; 6],
    tx: &Sender<(usize, AgentSummary)>,
) {
    if loading_mask[agent_idx] {
        return;
    }
    loading_mask[agent_idx] = true;

    let slot = match agent_idx {
        0 => &mut stats.claude,
        1 => &mut stats.codex,
        2 => &mut stats.agy,
        3 => &mut stats.cursor,
        4 => &mut stats.copilot,
        5 => &mut stats.grok,
        _ => return,
    };
    if slot.name.is_empty() {
        *slot = parser::placeholder_agent(agent_idx);
    }

    let home_clone = home_dir.clone();
    let tx_clone = tx.clone();
    thread::spawn(move || {
        let summary = parser::fetch_agent_by_index(&home_clone, agent_idx);
        let _ = tx_clone.send((agent_idx, summary));
    });
}

fn spawn_all_enabled_fetches(
    cfg: &AppConfig,
    home_dir: &PathBuf,
    stats: &mut AllUsageStats,
    loading_mask: &mut [bool; 6],
    tx: &Sender<(usize, AgentSummary)>,
) {
    let flags = [
        cfg.show_claude,
        cfg.show_codex,
        cfg.show_agy,
        cfg.show_cursor,
        cfg.show_copilot,
        cfg.show_grok,
    ];
    for (idx, &enabled) in flags.iter().enumerate() {
        if enabled {
            spawn_agent_fetch(idx, home_dir, stats, loading_mask, tx);
        }
    }
}

fn toggle_setting(cfg: &mut AppConfig, cursor: usize, forward: bool) -> Option<usize> {
    // Returns Some(agent_idx) if an agent was newly enabled and needs background fetch
    match cursor {
        0 => {
            let presets = ui::INTERVAL_PRESETS;
            let idx = presets
                .iter()
                .position(|&p| p >= cfg.refresh_interval_secs)
                .unwrap_or(3);
            let new_idx = if forward {
                (idx + 1).min(presets.len() - 1)
            } else {
                idx.saturating_sub(1)
            };
            cfg.refresh_interval_secs = presets[new_idx];
            None
        }
        1 => {
            cfg.bar_mode = match cfg.bar_mode {
                BarMode::Remaining => BarMode::Used,
                BarMode::Used => BarMode::Remaining,
            };
            None
        }
        2 => {
            cfg.show_claude = !cfg.show_claude;
            if cfg.show_claude { Some(0) } else { None }
        }
        3 => {
            cfg.show_codex = !cfg.show_codex;
            if cfg.show_codex { Some(1) } else { None }
        }
        4 => {
            cfg.show_agy = !cfg.show_agy;
            if cfg.show_agy { Some(2) } else { None }
        }
        5 => {
            cfg.show_cursor = !cfg.show_cursor;
            if cfg.show_cursor { Some(3) } else { None }
        }
        6 => {
            cfg.show_copilot = !cfg.show_copilot;
            if cfg.show_copilot { Some(4) } else { None }
        }
        7 => {
            cfg.show_grok = !cfg.show_grok;
            if cfg.show_grok { Some(5) } else { None }
        }
        _ => None,
    }
}

fn run_tui<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    home_dir: &PathBuf,
    mut cfg: AppConfig,
) -> io::Result<()> {
    let (tx, rx) = mpsc::channel::<(usize, AgentSummary)>();
    let mut stats = AllUsageStats::default();
    let mut loading_mask = [false; 6];
    let mut spinner_tick: usize = 0;
    let mut last_spinner_step = Instant::now();

    // Start initial fetch asynchronously so TUI opens in 0ms
    spawn_all_enabled_fetches(&cfg, home_dir, &mut stats, &mut loading_mask, &tx);

    let mut last_refresh = Instant::now();
    let mut status_msg: Option<String> = None;
    let mut status_timer: Option<Instant> = None;
    let mut is_paused = false;
    let mut show_settings = false;
    let mut settings_cursor: usize = 0;
    let mut needs_redraw = true;

    let status_duration = Duration::from_secs(3);
    let spinner_interval = Duration::from_millis(80);

    loop {
        let refresh_interval = Duration::from_secs(cfg.refresh_interval_secs.max(1));

        // Receive any completed background agent fetches
        while let Ok((agent_idx, summary)) = rx.try_recv() {
            match agent_idx {
                0 => stats.claude = summary,
                1 => stats.codex = summary,
                2 => stats.agy = summary,
                3 => stats.cursor = summary,
                4 => stats.copilot = summary,
                5 => stats.grok = summary,
                _ => {}
            }
            loading_mask[agent_idx] = false;
            stats.last_updated = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            parser::recompute_totals(&mut stats, &cfg);
            needs_redraw = true;
        }

        // Clear temporary status message after 3 seconds
        if let Some(st) = status_timer {
            if st.elapsed() >= status_duration {
                status_msg = None;
                status_timer = None;
                needs_redraw = true;
            }
        }

        // Automatic interval refresh (non-blocking)
        if !is_paused && last_refresh.elapsed() >= refresh_interval {
            spawn_all_enabled_fetches(&cfg, home_dir, &mut stats, &mut loading_mask, &tx);
            last_refresh = Instant::now();
            needs_redraw = true;
        }

        let any_loading = loading_mask.iter().any(|&b| b);
        if any_loading && last_spinner_step.elapsed() >= spinner_interval {
            spinner_tick = spinner_tick.wrapping_add(1);
            last_spinner_step = Instant::now();
            needs_redraw = true;
        }

        // Only draw when state actually changed, spinner stepped, or terminal resized
        if needs_redraw {
            let _ = execute!(io::stdout(), SetTitle("aitop"));
            terminal.draw(|f| {
                ui::render_ui(
                    f,
                    &stats,
                    &cfg,
                    status_msg.as_deref(),
                    show_settings,
                    settings_cursor,
                    &loading_mask,
                    spinner_tick,
                );
            })?;
            needs_redraw = false;
        }

        // Calculate exact sleep timeout: 80ms while loading spinner is active, otherwise sleep until next refresh
        let mut wait_timeout = if any_loading {
            spinner_interval.saturating_sub(last_spinner_step.elapsed())
        } else if is_paused {
            Duration::from_secs(3600)
        } else {
            refresh_interval.saturating_sub(last_refresh.elapsed())
        };

        if let Some(st) = status_timer {
            let rem_status = status_duration.saturating_sub(st.elapsed());
            wait_timeout = wait_timeout.min(rem_status);
        }

        if event::poll(wait_timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if show_settings {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('s') => {
                                show_settings = false;
                                needs_redraw = true;
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                settings_cursor = if settings_cursor == 0 {
                                    ui::SETTINGS_ROW_COUNT - 1
                                } else {
                                    settings_cursor - 1
                                };
                                needs_redraw = true;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                settings_cursor = (settings_cursor + 1) % ui::SETTINGS_ROW_COUNT;
                                needs_redraw = true;
                            }
                            KeyCode::Left | KeyCode::Char('h') => {
                                let maybe_idx = toggle_setting(&mut cfg, settings_cursor, false);
                                save_config(home_dir, &cfg);
                                if let Some(agent_idx) = maybe_idx {
                                    spawn_agent_fetch(
                                        agent_idx,
                                        home_dir,
                                        &mut stats,
                                        &mut loading_mask,
                                        &tx,
                                    );
                                } else {
                                    parser::recompute_totals(&mut stats, &cfg);
                                }
                                needs_redraw = true;
                            }
                            KeyCode::Right
                            | KeyCode::Char('l')
                            | KeyCode::Char(' ')
                            | KeyCode::Enter => {
                                let maybe_idx = toggle_setting(&mut cfg, settings_cursor, true);
                                save_config(home_dir, &cfg);
                                if let Some(agent_idx) = maybe_idx {
                                    spawn_agent_fetch(
                                        agent_idx,
                                        home_dir,
                                        &mut stats,
                                        &mut loading_mask,
                                        &tx,
                                    );
                                } else {
                                    parser::recompute_totals(&mut stats, &cfg);
                                }
                                needs_redraw = true;
                            }
                            KeyCode::Char('q') => break,
                            _ => {}
                        }
                    } else {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Char('r') => {
                                spawn_all_enabled_fetches(
                                    &cfg,
                                    home_dir,
                                    &mut stats,
                                    &mut loading_mask,
                                    &tx,
                                );
                                last_refresh = Instant::now();
                                needs_redraw = true;
                            }
                            KeyCode::Char('s') => {
                                show_settings = true;
                                status_msg = None;
                                status_timer = None;
                                needs_redraw = true;
                            }
                            KeyCode::Char(' ') => {
                                is_paused = !is_paused;
                                status_msg = Some(if is_paused {
                                    "Auto-refresh PAUSED".to_string()
                                } else {
                                    "Auto-refresh RESUMED".to_string()
                                });
                                status_timer = Some(Instant::now());
                                needs_redraw = true;
                            }
                            _ => {}
                        }
                    }
                }
                Event::Resize(_, _) => {
                    needs_redraw = true;
                }
                _ => {}
            }
        }
    }

    Ok(())
}
