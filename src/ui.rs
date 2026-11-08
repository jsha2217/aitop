use crate::{
    config::{Config, Paths},
    models::{
        countdown, format_number, Availability, BarMode, GroupBy, Period, Provider, QuotaSnapshot,
        Result, Snapshot, UsageItem, UsageTotals,
    },
    output,
    parser::FetchOptions,
    runtime::{self, Update},
};
use chrono::Local;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Table, TableState, Wrap,
    },
    Frame, Terminal,
};
use std::{
    io::{self, stdout},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, TryRecvError},
        Arc,
    },
    time::{Duration, Instant},
};

const PRESETS: [u64; 6] = [30, 60, 180, 300, 600, 900];
const SETTINGS_ROWS: usize = 11;
mod calendar;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum View {
    #[default]
    Overview,
    History,
    Settings,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DisplayOptions {
    pub ascii: bool,
    pub no_color: bool,
    pub color: bool,
}
impl DisplayOptions {
    fn apply(self, cfg: &Config) -> Config {
        let mut display = cfg.clone();
        display.ascii |= self.ascii;
        display.no_color = !self.color && (display.no_color || self.no_color);
        display
    }
}

#[derive(Debug)]
pub struct State {
    pub display: DisplayOptions,
    pub view: View,
    pub selected: usize,
    pub settings_row: usize,
    pub period: Period,
    pub group: GroupBy,
    pub paused: bool,
    pub loading: bool,
    pub details: bool,
    pub detail_scroll: u16,
    pub dollar_chart: bool,
    pub history_provider: Option<Provider>,
    pub manual_refresh: bool,
    pub refreshing_manually: bool,
    pub calendar_offset: u16,
    pub help: bool,
    pub notice: String,
}
impl Default for State {
    fn default() -> Self {
        Self {
            display: DisplayOptions::default(),
            view: View::Overview,
            selected: 0,
            settings_row: 0,
            period: Period::Today,
            group: GroupBy::Project,
            paused: false,
            loading: false,
            details: false,
            detail_scroll: 0,
            dollar_chart: false,
            history_provider: None,
            manual_refresh: false,
            refreshing_manually: false,
            calendar_offset: 0,
            help: false,
            notice: String::new(),
        }
    }
}

fn accent(cfg: &Config) -> Style {
    if cfg.no_color {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    }
}
fn muted() -> Style {
    Style::default()
}
fn border() -> Block<'static> {
    Block::default().borders(Borders::TOP)
}

pub fn render(frame: &mut Frame, snapshot: &Snapshot, cfg: &Config, state: &State) {
    let saved_cfg = cfg;
    let display_cfg = state.display.apply(cfg);
    let cfg = &display_cfg;
    let area = frame.area();
    if area.width < 32 || area.height < 10 {
        frame.render_widget(
            Paragraph::new("aitop\nEnlarge terminal to 32x10\n[q] Quit"),
            area,
        );
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);
    let brand = if cfg.ascii {
        "||| aitop"
    } else {
        "▂▅▇ aitop"
    };
    let state_tag = if snapshot.demo {
        "DEMO DATA / EXAMPLE VALUES"
    } else if state.paused {
        "PAUSED"
    } else if state.loading {
        "Syncing"
    } else {
        "Local monitor"
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {brand}  "), accent(cfg)),
            Span::styled(state_tag, Style::default().add_modifier(Modifier::BOLD)),
        ])),
        rows[0],
    );
    let navigation = [
        (View::Overview, "1 Overview"),
        (View::History, "2 History"),
        (View::Settings, "3 Settings"),
    ]
    .into_iter()
    .map(|(view, label)| {
        Span::styled(
            format!(" {label} "),
            if view == state.view {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            },
        )
    })
    .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Line::from(navigation)), rows[1]);
    if state.help {
        frame.render_widget(Paragraph::new("HELP\n\n1/2/3 or Tab  Change view\nj/k or arrows  Select / scroll\nEnter  Open provider details\ns  Settings\nr  Query enabled providers now (bypass local cache)\nSpace  Pause/resume automatic refresh\np  Period (History/details): today / 7d / 30d\ng / f  History grouping / service filter\n[ / ]  Calendar day; { / } week; 0 today\nDetails: Up/Down or PgUp/PgDn scroll; Home/End\nDetails: Left/Right service; c tokens/USD graph\nDetails: 2 filtered History; Enter/Esc back\n?  Close help\nq / Esc / Ctrl-C  Quit\n\nQuota values are observations, not guarantees.\nAPI estimates are separate from subscription bills.").wrap(Wrap{trim:false}),rows[2]);
    } else {
        match state.view {
            View::Overview => overview(frame, rows[2], snapshot, cfg, state),
            View::History => history(frame, rows[2], snapshot, cfg, state),
            View::Settings => settings(frame, rows[2], snapshot, saved_cfg, state),
        }
    }
    let notice = if !state.notice.is_empty() {
        state.notice.as_str()
    } else if state.details && state.view == View::Overview {
        if area.width < 62 {
            "[Up/Down] Scroll  [Enter/Esc] Back"
        } else {
            "[Up/Down] Scroll [L/R] Service [p] Period [c] USD [Enter/Esc] Back"
        }
    } else if state.view == View::History {
        if area.width < 62 {
            "[/] Day [0] Today [r] Refresh [q] Quit"
        } else {
            "[/] Day [0] Today [p] Period [g] Group [f] Filter [r] Refresh [q] Quit"
        }
    } else if !snapshot.warnings.is_empty() {
        snapshot.warnings[0].as_str()
    } else {
        "[Tab] View  [j/k] Select  [r] Refresh  [s] Settings  [?] Help  [q] Quit"
    };
    frame.render_widget(
        Paragraph::new(notice)
            .block(border())
            .wrap(Wrap { trim: false }),
        rows[3],
    );
}

fn overview(frame: &mut Frame, area: Rect, snapshot: &Snapshot, cfg: &Config, state: &State) {
    let now = Local::now().timestamp();
    let enabled = snapshot
        .providers
        .iter()
        .filter(|q| cfg.enabled(q.provider))
        .collect::<Vec<_>>();
    if enabled.is_empty() {
        frame.render_widget(Paragraph::new("No providers enabled.\n\nPress [s] to open Settings and enable your tools.\nUse `aitop demo` to explore without credentials."),area);
        return;
    }
    let index = state.selected.min(enabled.len() - 1);
    let selected = enabled[index];
    if state.details {
        let lines = detail_lines(selected, snapshot, cfg, state, area.width.saturating_sub(2));
        let limit = lines
            .len()
            .saturating_sub(area.height as usize)
            .min(u16::MAX as usize) as u16;
        let content_length = lines.len();
        frame.render_widget(
            Paragraph::new(lines).scroll((state.detail_scroll.min(limit), 0)),
            area,
        );
        if limit > 0 {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some(if cfg.ascii { "|" } else { "│" }))
                .thumb_symbol(if cfg.ascii { "#" } else { "█" })
                .thumb_style(accent(cfg));
            frame.render_stateful_widget(
                scrollbar,
                area,
                &mut ScrollbarState::new(content_length)
                    .position(state.detail_scroll.min(limit) as usize)
                    .viewport_content_length(area.height as usize),
            );
        }
        return;
    }
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);
    let list_area = sections[0];
    let mut lines = vec![];
    let mut selected_end = 0;
    for (i, q) in enabled.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.extend(provider_lines(q, cfg, list_area.width, i == index, now));
        if i == index {
            selected_end = lines.len();
        }
    }
    let scroll = selected_end.saturating_sub(list_area.height as usize);
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        list_area,
    );
    let history = &snapshot.history;
    let total = history
        .total_tokens
        .map(format_number)
        .unwrap_or_else(|| "unavailable".into());
    let status = if history.partial {
        "PARTIAL"
    } else {
        "observed local history"
    };
    let summary = vec![
        Line::from(vec![
            Span::styled(
                format!(" LOCAL USAGE / {} ", history.period.label().to_uppercase()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(status),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" {total} TOKENS  "),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "API estimate {}   [2] History",
                output::estimated_money(history.estimated_cost_usd, history.priced_cost_usd)
            )),
        ]),
    ];
    frame.render_widget(Paragraph::new(summary).block(border()), sections[1]);
}

fn local_time(timestamp: Option<i64>) -> String {
    timestamp
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .map(|d| {
            d.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M %:z")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".into())
}

fn usage_money(totals: &UsageTotals) -> String {
    if totals.estimated_cost_usd.is_some() {
        output::money(totals.estimated_cost_usd)
    } else if totals.priced_cost_usd.is_some() {
        format!("{} + ?", output::money(totals.priced_cost_usd))
    } else {
        "unknown".into()
    }
}

// Split styled content before scrolling so every line remains reachable, even on a narrow TTY.
fn wrap_detail(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut output = Vec::new();
    for line in lines {
        let mut spans = Vec::new();
        let mut used = 0;
        for span in line.spans {
            let mut text = String::new();
            for character in span.content.chars() {
                let n = Span::raw(character.to_string()).width();
                if used + n > width && used > 0 {
                    if !text.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut text), span.style));
                    }
                    output.push(Line::from(std::mem::take(&mut spans)));
                    used = 0;
                }
                text.push(character);
                used += n;
            }
            if !text.is_empty() {
                spans.push(Span::styled(text, span.style));
            }
        }
        output.push(Line::from(spans));
    }
    output
}

fn detail_lines(
    q: &QuotaSnapshot,
    snapshot: &Snapshot,
    cfg: &Config,
    state: &State,
    width: u16,
) -> Vec<Line<'static>> {
    let now = Local::now().timestamp();
    let heading = |label: String| Line::from(Span::styled(label, accent(cfg)));
    let mut lines = vec![
        heading(format!(
            " {} / DETAILS   {}",
            q.provider.name(),
            q.status_label(now)
        )),
        Line::from(format!(
            " Updated {}  |  {}",
            local_time(q.observed_at),
            source_name(&q.source)
        )),
        Line::from(""),
        heading(" ACCOUNT LIMITS".into()),
    ];
    if let Some(error) = &q.refresh_error {
        lines.insert(
            2,
            Line::from(Span::styled(
                format!(" Refresh failed: {error}"),
                Style::default().fg(if cfg.no_color {
                    Color::Reset
                } else {
                    Color::Yellow
                }),
            )),
        );
        lines.insert(
            3,
            Line::from(format!(
                " Last successful observation: {} / {}",
                local_time(q.observed_at),
                retry_label(q, now)
            )),
        );
    } else if q.availability != Availability::Available {
        lines.insert(2, Line::from(format!(" {}", q.message)));
    }
    if q.windows.is_empty() {
        lines.push(Line::from(
            " No quota available. See connection details below.",
        ));
    }
    for w in &q.windows {
        let expired = w.expired(now);
        let color = if cfg.no_color || expired {
            Color::Reset
        } else if w.used_percent >= 85.0 {
            Color::Red
        } else if w.used_percent >= 60.0 {
            Color::Yellow
        } else {
            Color::Green
        };
        let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
        lines.push(Line::from(vec![
            Span::raw(format!(" {}  ", w.label)),
            Span::styled(
                format!(
                    "{:.1}% LEFT / {:.1}% USED{}",
                    100.0 - w.used_percent,
                    w.used_percent,
                    if expired {
                        " (past reset; recheck)"
                    } else if q.stale(now) {
                        " (last observation)"
                    } else {
                        ""
                    }
                ),
                style,
            ),
        ]));
        let bar_width = usize::from(width.saturating_sub(4)).min(64);
        let n =
            ((w.percent(cfg.bar_mode) / 100.0 * bar_width as f64).round() as usize).min(bar_width);
        let (on, off) = if cfg.ascii {
            ("#", "-")
        } else {
            ("█", "░")
        };
        let bar = if expired {
            "-".repeat(bar_width)
        } else {
            format!("{}{}", on.repeat(n), off.repeat(bar_width - n))
        };
        lines.push(Line::from(Span::styled(
            format!(" {bar}"),
            Style::default().fg(color),
        )));
        let absolute = w
            .resets_at
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|_| format!("  /  {}", local_time(w.resets_at)))
            .unwrap_or_default();
        lines.push(Line::from(format!(
            " {} {}{absolute}",
            if cfg.ascii { "[t]" } else { "◷" },
            w.reset_label(now)
        )));
        if w.used_percent >= 85.0 && !expired {
            lines.push(Line::from(Span::styled(
                " Low remaining quota in this observation",
                style,
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(heading(" LOCAL USAGE / API-EQUIVALENT USD".into()));
    lines.push(Line::from(
        " This device's indexed logs; account ownership and other devices unknown.",
    ));
    if let Some(usage) = snapshot
        .provider_usage
        .iter()
        .find(|u| u.provider == q.provider)
    {
        lines.push(Line::from(format!(" {}", usage.pricing_source)));
        if usage.partial {
            lines.push(Line::from(" PARTIAL: accepted local records only"));
        }
        if !usage.available {
            lines.push(Line::from(if q.provider.supports_history() {
                " No supported local usage records. Tokens and USD unavailable."
            } else {
                " Local token history is unsupported for this provider."
            }));
            lines.extend(calendar::lines(
                &usage.days,
                q.provider.name(),
                cfg,
                width,
                state.calendar_offset,
                snapshot.generated_at,
            ));
        } else {
            lines.push(Line::from(
                " PERIOD          TOKENS       API ESTIMATE (USD)",
            ));
            for p in &usage.periods {
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        " {}{:<12} {:>9}    {}",
                        if p.period == state.period { ">" } else { " " },
                        p.period.label(),
                        format_number(p.totals.tokens.total()),
                        usage_money(&p.totals)
                    ),
                    if p.period == state.period {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                )]));
            }
            lines.push(Line::from(""));
            lines.extend(calendar::lines(
                &usage.days,
                q.provider.name(),
                cfg,
                width,
                state.calendar_offset,
                snapshot.generated_at,
            ));
            if let Some(p) = usage.periods.iter().find(|p| p.period == state.period) {
                lines.push(Line::from(""));
                lines.push(heading(format!(
                    " {} BREAKDOWN   [p] Change period",
                    p.period.label().to_uppercase()
                )));
                lines.push(Line::from(format!(
                    " Input {}  /  Output {}",
                    format_number(p.totals.tokens.input),
                    format_number(p.totals.tokens.output)
                )));
                lines.push(Line::from(format!(
                    " Cache read {}  /  Cache write {}",
                    format_number(p.totals.tokens.cache_read),
                    format_number(p.totals.tokens.cache_write)
                )));
                lines.push(Line::from(format!(
                    " {} accepted usage events  /  {} unpriced",
                    p.totals.events, p.totals.unpriced_events
                )));
                lines.push(Line::from(""));
                lines.push(heading(" RECENT 24 HOURS   [c] Tokens / USD".into()));
                let values: Option<Vec<f64>> = usage
                    .hours
                    .iter()
                    .map(|h| {
                        if state.dollar_chart {
                            h.totals.estimated_cost_usd
                        } else {
                            Some(h.totals.tokens.total() as f64)
                        }
                    })
                    .collect();
                if let Some(values) = values {
                    let max = values.iter().copied().fold(0.0_f64, f64::max);
                    lines.push(Line::from(format!(
                        " Peak {} per hour{}",
                        if state.dollar_chart {
                            output::money(Some(max))
                        } else {
                            format!("{} tokens", format_number(max as u64))
                        },
                        if state.dollar_chart {
                            " (API estimate)"
                        } else {
                            ""
                        }
                    )));
                    let spacer = if width >= 52 { " " } else { "" };
                    for row in (0..4).rev() {
                        let bars = values
                            .iter()
                            .map(|v| {
                                let height = if max > 0.0 {
                                    (v / max * 4.0).ceil() as usize
                                } else {
                                    0
                                };
                                format!(
                                    "{}{spacer}",
                                    if height > row {
                                        if cfg.ascii {
                                            "#"
                                        } else {
                                            "█"
                                        }
                                    } else {
                                        " "
                                    }
                                )
                            })
                            .collect::<String>();
                        lines.push(Line::from(Span::styled(format!(" {bars}"), accent(cfg))));
                    }
                    if let (Some(first), Some(last)) = (usage.hours.first(), usage.hours.last()) {
                        lines.push(Line::from(format!(
                            " {}  ->  {}",
                            local_time(Some(first.since)),
                            local_time(Some(last.since))
                        )));
                    }
                    lines.push(Line::from(
                        " Local elapsed-hour buckets; current hour is partial.",
                    ));
                } else {
                    lines.push(Line::from(
                        " USD chart unavailable: missing rates. Press [c] for token graph.",
                    ));
                }
                let mut top = |title: &str, items: &[UsageItem]| {
                    lines.push(Line::from(""));
                    lines.push(heading(format!(
                        " {title} / {} / TOKENS + USD",
                        p.period.label()
                    )));
                    if items.is_empty() {
                        lines.push(Line::from(" No recorded usage in this period."));
                    }
                    for item in items {
                        lines.push(Line::from(format!(
                            " {}  {}  {}",
                            item.label,
                            format_number(item.totals.tokens.total()),
                            usage_money(&item.totals)
                        )));
                    }
                };
                top("TOP MODELS", &p.models);
                top("TOP PROJECTS", &p.projects);
                if p.totals.unpriced_events > 0 {
                    lines.push(Line::from(
                        " Rates missing: configure exact model IDs in ~/.config/aitop/prices.json.",
                    ));
                    lines.push(Line::from(
                        " A dollar amount followed by '+ ?' is a known subtotal, not the total.",
                    ));
                }
            }
        }
        for warning in &usage.warnings {
            lines.push(Line::from(format!(" {warning}")));
        }
    } else {
        lines.push(Line::from(if state.loading {
            " Loading local usage..."
        } else {
            " Local usage unavailable. Press [r] to retry."
        }));
    }
    lines.extend([
        Line::from(""),
        Line::from(
            " USD uses official standard API rates or local overrides, not your subscription bill.",
        ),
        Line::from(" Claude cache writes use a 5m baseline; speed, region and historical rates may differ."),
        Line::from(""),
        heading(" CONNECTION DETAILS".into()),
        Line::from(format!(" {}", q.message)),
        Line::from(format!(" Observed: {}", local_time(q.observed_at))),
        Line::from(format!(
            " Last fetch attempt: {}",
            local_time(Some(q.fetched_at))
        )),
        Line::from(format!(
            " Refresh: {} / interval {} / source: {}",
            if state.paused { "paused" } else { "automatic" },
            refresh_interval_label(cfg.refresh_interval_secs),
            q.source
        )),
    ]);
    if q.retry_after > now {
        lines.push(Line::from(format!(
            " Automatic refresh cache until {}",
            local_time(Some(q.retry_after))
        )));
    }
    if let Some(error) = &q.refresh_error {
        lines.push(Line::from(format!(" Last refresh failed: {error}")));
    }
    lines.push(Line::from(
        " [r] Query now / [2] Open this provider's History / [Enter] Back",
    ));
    wrap_detail(lines, width)
}

fn source_name(source: &str) -> &'static str {
    if source.contains("demo") {
        "Sample data"
    } else if source == "app-server" {
        "Codex CLI query"
    } else if source.contains("agy") {
        "AGY CLI query"
    } else if source.contains("oauth") || source.contains("https") {
        "Account query"
    } else if source == "statusline" {
        "Statusline observation"
    } else if source.contains("local") {
        "Local observation"
    } else {
        "Provider observation"
    }
}

fn provider_lines(
    q: &QuotaSnapshot,
    cfg: &Config,
    width: u16,
    selected: bool,
    now: i64,
) -> Vec<Line<'static>> {
    let width = usize::from(width).max(4);
    let inner = width - 4;
    let (tl, tr, bl, br, h, v) = if cfg.ascii {
        ("+", "+", "+", "+", "-", "|")
    } else {
        ("╭", "╮", "╰", "╯", "─", "│")
    };
    let outline = if selected {
        accent(cfg)
    } else {
        Style::default()
    };
    let status = match q.availability {
        Availability::Available if q.refresh_error.is_some() => "CACHED",
        Availability::Available if q.stale(now) => "STALE",
        Availability::Available => "OBSERVED",
        Availability::AuthRequired if q.message.contains("Keychain") => "ACCESS NEEDED",
        Availability::AuthRequired => "SIGN IN",
        Availability::Unsupported => "UNSUPPORTED",
        Availability::Error => "ERROR",
        Availability::NoData => "WAITING",
    };
    let title = format!(
        " {}{} ",
        if selected { "> " } else { "" },
        q.provider.name()
    );
    let gap = width.saturating_sub(title.len() + status.len() + 3);
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{tl}{title}"), outline),
        Span::styled(h.repeat(gap), outline),
        Span::styled(format!(" {status}{tr}"), outline),
    ])];
    let framed = |mut spans: Vec<Span<'static>>| {
        let count: usize = spans.iter().map(Span::width).sum();
        let mut row = vec![Span::styled(format!("{v} "), outline)];
        row.append(&mut spans);
        row.push(Span::raw(" ".repeat(inner.saturating_sub(count))));
        row.push(Span::styled(format!(" {v}"), outline));
        Line::from(row)
    };
    if q.windows.is_empty() {
        lines.push(framed(vec![
            Span::styled("-- ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("No quota available. [Enter] Details"),
        ]));
    } else {
        for w in &q.windows {
            let expired = w.expired(now);
            let label_width = if inner >= 62 {
                15
            } else if inner >= 48 {
                12
            } else {
                8
            };
            let bar_width = if inner >= 62 {
                18
            } else if inner >= 48 {
                10
            } else {
                0
            };
            let value = w.percent(cfg.bar_mode);
            let text = if expired {
                "--       ".into()
            } else {
                format!(
                    "{value:3.0}% {}",
                    if cfg.bar_mode == BarMode::Remaining {
                        "LEFT"
                    } else {
                        "USED"
                    }
                )
            };
            let color = if cfg.no_color || expired {
                Color::Reset
            } else if w.used_percent >= 85.0 {
                Color::Red
            } else if w.used_percent >= 60.0 {
                Color::Yellow
            } else {
                Color::Green
            };
            let mut spans = vec![
                Span::raw(format!(
                    "{:<label_width$}  ",
                    w.label.chars().take(label_width).collect::<String>()
                )),
                Span::styled(
                    text,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ];
            if bar_width > 0 {
                let n = ((value / 100.0 * bar_width as f64).round() as usize).min(bar_width);
                let (on, off) = if cfg.ascii {
                    ("#", "-")
                } else {
                    ("█", "░")
                };
                let bar = if expired {
                    "-".repeat(bar_width)
                } else {
                    format!("{}{}", on.repeat(n), off.repeat(bar_width - n))
                };
                spans.push(Span::styled(format!("  {bar}"), Style::default().fg(color)));
            }
            let reset = format!(
                "{} {}",
                if cfg.ascii { "[t]" } else { "◷" },
                w.reset_label(now)
            );
            spans.push(Span::raw(format!("  {reset}")));
            lines.push(framed(spans));
        }
        let source = source_name(&q.source);
        let age = q
            .observed_at
            .map(|t| {
                let age = now.saturating_sub(t).max(0);
                if age >= 3600 {
                    format!("{}h {}m ago", age / 3600, age % 3600 / 60)
                } else if age >= 60 {
                    format!("{}m ago", age / 60)
                } else {
                    format!("{age}s ago")
                }
            })
            .unwrap_or_else(|| "unknown".into());
        lines.push(framed(vec![Span::raw(format!(
            "Updated {age}  /  {source}"
        ))]));
    }
    if q.refresh_error.is_some() {
        lines.push(framed(vec![Span::styled(
            format!("Refresh failed / {}", retry_label(q, now)),
            Style::default().fg(if cfg.no_color {
                Color::Reset
            } else {
                Color::Yellow
            }),
        )]));
    }
    lines.push(Line::from(Span::styled(
        format!("{bl}{}{br}", h.repeat(width - 2)),
        outline,
    )));
    lines
}

fn retry_label(q: &QuotaSnapshot, now: i64) -> String {
    if q.retry_after > now {
        format!(
            "auto cache {} / [r] query now",
            countdown(Some(q.retry_after), now)
        )
    } else {
        "[r] query now".into()
    }
}

fn history(frame: &mut Frame, area: Rect, snapshot: &Snapshot, cfg: &Config, state: &State) {
    let days = crate::models::combined_days(
        snapshot
            .provider_usage
            .iter()
            .filter(|u| cfg.enabled(u.provider)),
    );
    let calendar = calendar::lines(
        &days,
        if snapshot
            .provider_usage
            .iter()
            .any(|u| cfg.enabled(u.provider) && u.partial)
        {
            "ALL ENABLED (PARTIAL)"
        } else {
            "ALL ENABLED"
        },
        cfg,
        area.width.saturating_sub(2),
        state.calendar_offset,
        snapshot.generated_at,
    );
    let calendar_height = if area.height >= 17 {
        calendar.len() as u16
    } else {
        0
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(calendar_height),
            Constraint::Min(2),
            Constraint::Length(2),
        ])
        .split(area);
    let report = &snapshot.history;
    frame.render_widget(
        Paragraph::new(format!(
            " {} / {:?} / {}  [p] Period  [g] Group  [f] Service\n {}",
            report.period.label(),
            report.group_by,
            report.provider.map(|p| p.name()).unwrap_or("All enabled"),
            if report.partial {
                "PARTIAL: accepted local records only"
            } else {
                "Observed local history"
            }
        )),
        rows[0],
    );
    if calendar_height > 0 {
        frame.render_widget(Paragraph::new(calendar), rows[1]);
    }
    let records = report.rows.iter().map(|r| {
        Row::new(vec![
            Cell::from(r.label.clone()),
            Cell::from(format_number(r.tokens)),
            Cell::from(output::estimated_money(
                r.estimated_cost_usd,
                r.priced_cost_usd,
            )),
        ])
    });
    let table = Table::new(
        records,
        [
            Constraint::Percentage(50),
            Constraint::Percentage(20),
            Constraint::Percentage(30),
        ],
    )
    .header(
        Row::new([" Name", "Tokens", "API estimate"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut selected = TableState::default();
    if !report.rows.is_empty() {
        selected.select(Some(state.selected.min(report.rows.len() - 1)));
    }
    frame.render_stateful_widget(table, rows[2], &mut selected);
    let text = if !report.available {
        "No supported usage events indexed. Local history is not a complete account ledger.".into()
    } else {
        format!(
            " {} unpriced events / {} / + ? = subtotal",
            report.unpriced_events, report.pricing_source
        )
    };
    frame.render_widget(
        Paragraph::new(text)
            .block(border())
            .wrap(Wrap { trim: false }),
        rows[3],
    );
}

fn refresh_interval_label(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

fn settings(frame: &mut Frame, area: Rect, snapshot: &Snapshot, cfg: &Config, state: &State) {
    let mut labels = vec![
        format!(
            "Refresh interval     {}",
            refresh_interval_label(cfg.refresh_interval_secs)
        ),
        format!("Gauge mode           {:?}", cfg.bar_mode),
        format!("ASCII                {}", cfg.ascii),
        format!("No color             {}", cfg.no_color),
    ];
    for p in Provider::ALL {
        let status = snapshot
            .providers
            .iter()
            .find(|q| q.provider == p)
            .map(|q| q.status_label(Local::now().timestamp()))
            .unwrap_or("Not queried");
        labels.push(format!(
            "{} {:<17} {}",
            if cfg.enabled(p) { "[x]" } else { "[ ]" },
            p.name(),
            status
        ));
    }
    labels.push(format!(
        "Claude Keychain      {}",
        if cfg.claude_keychain {
            "allowed (saved)"
        } else {
            "not allowed"
        }
    ));
    let mut lines = vec![
        Line::from(" SETTINGS / CONNECTIONS"),
        Line::from(" [1/2/3/Tab] View  [j/k] Move  [h/l/Enter] Change  [s/Esc] Return"),
        Line::from(""),
    ];
    if state.display.no_color && !state.display.color {
        lines[2] =
            Line::from(" NO_COLOR / --no-color applies to this run only; saved preferences below.");
    } else if state.display.color {
        lines[2] = Line::from(" --color applies to this run only; saved preferences below.");
    }
    for (i, label) in labels.iter().enumerate() {
        lines.push(Line::from(Span::styled(
            format!(" {label}"),
            if i == state.settings_row {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                muted()
            },
        )));
    }
    if let Some(p) = state
        .settings_row
        .checked_sub(4)
        .and_then(|i| Provider::ALL.get(i))
    {
        if let Some(q) = snapshot.providers.iter().find(|q| q.provider == *p) {
            lines.extend([
                Line::from(""),
                Line::from(format!(" {}", q.message)),
                Line::from(format!(" Source: {}", q.source)),
            ]);
        }
    }
    if state.settings_row == 10 {
        lines.extend([
            Line::from(""),
            Line::from(" Allow reading the existing Claude Code credential from macOS Keychain."),
            Line::from(
                " This permission is saved for future launches; switch it off here to revoke it.",
            ),
        ]);
    }
    let scroll = state
        .settings_row
        .saturating_add(4)
        .saturating_sub(area.height as usize);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll as u16, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

struct TerminalGuard;
impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(stdout(), EnterAlternateScreen)?;
        Ok(guard)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

fn next_view(view: View) -> View {
    match view {
        View::Overview => View::History,
        View::History => View::Settings,
        View::Settings => View::Overview,
    }
}
fn change_setting(cfg: &mut Config, row: usize, forward: bool) {
    match row {
        0 => {
            cfg.refresh_interval_secs = if forward {
                PRESETS
                    .iter()
                    .copied()
                    .find(|&p| p > cfg.refresh_interval_secs)
                    .unwrap_or(PRESETS[PRESETS.len() - 1])
            } else {
                PRESETS
                    .iter()
                    .rev()
                    .copied()
                    .find(|&p| p < cfg.refresh_interval_secs)
                    .unwrap_or(PRESETS[0])
            };
        }
        1 => {
            cfg.bar_mode = if cfg.bar_mode == BarMode::Remaining {
                BarMode::Used
            } else {
                BarMode::Remaining
            }
        }
        2 => cfg.ascii = !cfg.ascii,
        3 => cfg.no_color = !cfg.no_color,
        10 => cfg.claude_keychain = !cfg.claude_keychain,
        n => {
            if let Some(&p) = Provider::ALL.get(n - 4) {
                cfg.toggle(p);
            }
        }
    }
}

pub fn run(
    paths: Paths,
    mut cfg: Config,
    options: FetchOptions,
    cancel: Arc<AtomicBool>,
    is_demo: bool,
    display: DisplayOptions,
) -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    // DisplayOptions resolves NO_COLOR. Let the backend emit those resolved styles,
    // including an explicit --color override and resets when color is toggled off.
    crossterm::style::force_color_output(true);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        previous_hook(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut state = State {
        display,
        ..Default::default()
    };
    let mut snapshot = if is_demo {
        runtime::demo(state.period, state.group)
    } else {
        runtime::initial(&paths, &cfg, options, state.period, state.group)
    };
    let mut receiver: Option<Receiver<Update>> = None;
    let mut pending = !is_demo;
    let mut refresh = Instant::now();
    let mut tick = Instant::now();
    let mut redraw = true;
    let result = (|| -> Result<()> {
        while !cancel.load(Ordering::Relaxed) {
            if pending && receiver.is_none() && !is_demo {
                snapshot.warnings.clear();
                state.refreshing_manually = state.manual_refresh;
                if state.refreshing_manually {
                    state.notice = "Refreshing enabled providers now".into();
                }
                receiver = Some(runtime::start_filtered(
                    paths.clone(),
                    cfg.clone(),
                    FetchOptions {
                        force_refresh: state.manual_refresh,
                        ..options
                    },
                    state.period,
                    state.group,
                    cancel.clone(),
                    state.history_provider,
                ));
                state.loading = true;
                state.manual_refresh = false;
                pending = false;
                redraw = true;
                refresh = Instant::now();
            }
            if let Some(rx) = &receiver {
                let mut complete = false;
                loop {
                    match rx.try_recv() {
                        Ok(update) => {
                            runtime::apply(&mut snapshot, update);
                            redraw = true;
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            complete = true;
                            break;
                        }
                    }
                }
                if complete {
                    receiver = None;
                    state.loading = false;
                    state.refreshing_manually = false;
                    if state.notice == "Refreshing enabled providers now"
                        || state.notice == "Refresh already in progress"
                    {
                        state.notice.clear();
                    }
                    redraw = true;
                }
            }
            if !state.paused && refresh.elapsed() >= Duration::from_secs(cfg.refresh_interval_secs)
            {
                pending = true;
                refresh = Instant::now();
            }
            if tick.elapsed() >= Duration::from_secs(1) {
                redraw = true;
                tick = Instant::now();
            }
            if redraw {
                terminal.draw(|f| render(f, &snapshot, &cfg, &state))?;
                redraw = false;
            }
            if !event::poll(Duration::from_millis(if state.loading { 80 } else { 200 }))? {
                continue;
            }
            let event = event::read()?;
            if let Event::Resize(_, _) = event {
                redraw = true;
                continue;
            }
            let Event::Key(key) = event else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                break;
            }
            if key.code == KeyCode::Char('q') {
                break;
            }
            if key.code == KeyCode::Esc
                && state.view != View::Settings
                && !state.details
                && !state.help
            {
                break;
            }
            redraw = true;
            if state.help {
                state.help = false;
                continue;
            }
            if key.code == KeyCode::Char('?') {
                state.help = true;
                continue;
            }
            if state.view == View::Settings
                && !matches!(key.code, KeyCode::Char('1' | '2' | '3') | KeyCode::Tab)
            {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('s') => state.view = View::Overview,
                    KeyCode::Down | KeyCode::Char('j') => {
                        state.settings_row = (state.settings_row + 1) % SETTINGS_ROWS
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        state.settings_row =
                            (state.settings_row + SETTINGS_ROWS - 1) % SETTINGS_ROWS
                    }
                    KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('h' | 'l' | ' ')
                    | KeyCode::Enter => {
                        let old = cfg.clone();
                        change_setting(
                            &mut cfg,
                            state.settings_row,
                            !matches!(key.code, KeyCode::Left | KeyCode::Char('h')),
                        );
                        if !is_demo {
                            if paths.save(&cfg).is_err() {
                                cfg = old;
                                state.notice =
                                    "Could not save settings; existing configuration preserved"
                                        .into();
                            } else {
                                state.notice = "Settings saved".into();
                                pending = true;
                                if old.claude_keychain != cfg.claude_keychain {
                                    state.manual_refresh = true;
                                }
                            }
                        }
                    }
                    KeyCode::Char('r') => request_refresh(&mut state, &mut pending, &mut refresh),
                    _ => {}
                }
            } else {
                let size = terminal.size()?;
                handle_key(
                    key,
                    &mut state,
                    &snapshot,
                    &cfg,
                    &mut pending,
                    &mut refresh,
                    (size.width, size.height),
                );
            }
            if is_demo {
                snapshot =
                    runtime::demo_filtered(state.period, state.group, state.history_provider);
            }
        }
        Ok(())
    })();
    cancel.store(true, Ordering::Relaxed);
    drop(_guard);
    if let Some(rx) = receiver {
        for _ in rx {}
    }
    result
}
fn handle_key(
    key: KeyEvent,
    state: &mut State,
    snapshot: &Snapshot,
    cfg: &Config,
    pending: &mut bool,
    refresh: &mut Instant,
    size: (u16, u16),
) {
    let providers: Vec<_> = snapshot
        .providers
        .iter()
        .filter(|q| cfg.enabled(q.provider))
        .collect();
    let selected = providers.get(state.selected.min(providers.len().saturating_sub(1)));
    let detail_limit = selected
        .filter(|_| state.details && state.view == View::Overview)
        .map(|q| {
            detail_lines(q, snapshot, cfg, state, size.0.saturating_sub(2))
                .len()
                .saturating_sub(size.1.saturating_sub(6) as usize)
                .min(u16::MAX as usize) as u16
        })
        .unwrap_or(0);
    state.detail_scroll = state.detail_scroll.min(detail_limit);
    match key.code {
        KeyCode::Esc => {
            if state.details {
                state.details = false;
            } else {
                state.notice = "Press q to quit".into();
            }
        }
        KeyCode::Tab => {
            state.view = next_view(state.view);
            state.selected = 0;
            state.details = false;
        }
        KeyCode::Char('1') => {
            state.view = View::Overview;
            state.selected = 0;
            state.details = false;
        }
        KeyCode::Char('2') => {
            if state.details && state.view == View::Overview {
                state.history_provider = selected.map(|q| q.provider);
                *pending = true;
            }
            state.view = View::History;
            state.selected = 0;
            state.details = false;
        }
        KeyCode::Char('3' | 's') => state.view = View::Settings,
        KeyCode::Down | KeyCode::Char('j') => {
            if state.details && state.view == View::Overview {
                state.detail_scroll = state.detail_scroll.saturating_add(1).min(detail_limit);
                return;
            }
            let count = if state.view == View::History {
                snapshot.history.rows.len()
            } else {
                providers.len()
            };
            state.selected = (state.selected + 1).min(count.saturating_sub(1));
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if state.details && state.view == View::Overview {
                state.detail_scroll = state.detail_scroll.saturating_sub(1);
            } else {
                state.selected = state.selected.saturating_sub(1);
            }
        }
        KeyCode::Left | KeyCode::Right if state.details && state.view == View::Overview => {
            state.selected = if key.code == KeyCode::Left {
                state.selected.saturating_sub(1)
            } else {
                (state.selected + 1).min(providers.len().saturating_sub(1))
            };
            state.detail_scroll = 0;
        }
        KeyCode::PageDown | KeyCode::PageUp | KeyCode::Home | KeyCode::End if state.details => {
            state.detail_scroll = match key.code {
                KeyCode::Home => 0,
                KeyCode::End => detail_limit,
                KeyCode::PageDown => state
                    .detail_scroll
                    .saturating_add(size.1.saturating_sub(8))
                    .min(detail_limit),
                _ => state.detail_scroll.saturating_sub(size.1.saturating_sub(8)),
            };
        }
        KeyCode::Enter if state.view == View::Overview => {
            state.details = !state.details;
            state.detail_scroll = 0;
            state.notice.clear();
        }
        KeyCode::Char('c') if state.details => state.dollar_chart = !state.dollar_chart,
        KeyCode::Char('[' | ']' | '{' | '}' | '0')
            if state.view == View::History || state.details =>
        {
            state.calendar_offset = match key.code {
                KeyCode::Char('[') => state.calendar_offset.saturating_add(1).min(364),
                KeyCode::Char('{') => state.calendar_offset.saturating_add(7).min(364),
                KeyCode::Char(']') => state.calendar_offset.saturating_sub(1),
                KeyCode::Char('}') => state.calendar_offset.saturating_sub(7),
                _ => 0,
            };
        }
        KeyCode::Char('r') => request_refresh(state, pending, refresh),
        KeyCode::Char(' ') => state.paused = !state.paused,
        KeyCode::Char('p') if state.view == View::History || state.details => {
            state.period = match state.period {
                Period::Today => Period::SevenDays,
                Period::SevenDays => Period::ThirtyDays,
                Period::ThirtyDays => Period::Today,
            };
            *pending = true;
            if state.view == View::History {
                state.selected = 0;
            }
        }
        KeyCode::Char('f') if state.view == View::History => {
            let mut choices = vec![None];
            choices.extend(providers.iter().map(|q| Some(q.provider)));
            let index = choices
                .iter()
                .position(|p| *p == state.history_provider)
                .unwrap_or(0);
            state.history_provider = choices[(index + 1) % choices.len()];
            state.selected = 0;
            *pending = true;
        }
        KeyCode::Char('g') if state.view == View::History => {
            state.group = match state.group {
                GroupBy::Project => GroupBy::Model,
                GroupBy::Model => GroupBy::Provider,
                GroupBy::Provider => GroupBy::Project,
            };
            *pending = true;
            state.selected = 0;
        }
        _ => {}
    }
}

fn request_refresh(state: &mut State, pending: &mut bool, refresh: &mut Instant) {
    if state.loading && state.refreshing_manually {
        state.notice = "Refresh already in progress".into();
        return;
    }
    state.manual_refresh = true;
    *pending = true;
    *refresh = Instant::now();
    state.notice = if state.loading {
        "Refresh queued after current collection"
    } else {
        "Refreshing enabled providers now"
    }
    .into();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_color_overrides_change_the_render_but_settings_show_saved_preferences() {
        let mut cfg = Config {
            show_claude: true,
            show_codex: true,
            ..Default::default()
        };
        let snapshot = runtime::demo(Period::Today, GroupBy::Project);
        let mut state = State {
            view: View::History,
            display: DisplayOptions {
                no_color: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(110, 30)).unwrap();
        terminal
            .draw(|f| render(f, &snapshot, &cfg, &state))
            .unwrap();
        assert!(!terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|c| matches!(c.fg, Color::Indexed(_))));
        state.view = View::Settings;
        terminal
            .draw(|f| render(f, &snapshot, &cfg, &state))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("No color             false"));
        cfg.no_color = true;
        state.view = View::History;
        state.display.color = true;
        terminal
            .draw(|f| render(f, &snapshot, &cfg, &state))
            .unwrap();
        assert!(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|c| matches!(c.fg, Color::Indexed(42))));
        assert!(cfg.no_color);
    }

    #[test]
    fn manual_refresh_queues_once_and_inflight_manual_refresh_is_not_duplicated() {
        let mut state = State::default();
        let mut pending = false;
        let mut refresh = Instant::now();
        request_refresh(&mut state, &mut pending, &mut refresh);
        assert!(state.manual_refresh && pending);
        state.loading = true;
        state.refreshing_manually = true;
        state.manual_refresh = false;
        pending = false;
        request_refresh(&mut state, &mut pending, &mut refresh);
        assert!(!pending && !state.manual_refresh);
        state.refreshing_manually = false;
        request_refresh(&mut state, &mut pending, &mut refresh);
        assert!(pending && state.manual_refresh);
        request_refresh(&mut state, &mut pending, &mut refresh);
        assert!(pending && state.manual_refresh);
    }

    #[test]
    fn history_calendar_stays_combined_when_the_table_is_filtered() {
        let cfg = Config {
            show_claude: true,
            show_codex: true,
            ..Default::default()
        };
        let snapshot =
            runtime::demo_filtered(Period::Today, GroupBy::Project, Some(Provider::Codex));
        let state = State {
            view: View::History,
            history_provider: Some(Provider::Codex),
            ..Default::default()
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(110, 30)).unwrap();
        terminal
            .draw(|f| render(f, &snapshot, &cfg, &state))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("DAILY TOKENS / ALL ENABLED"));
        assert!(text.contains("1,240,000 tokens"));
        assert_eq!(snapshot.history.total_tokens, Some(498_000));
    }

    #[test]
    fn details_preserve_gauges_and_every_section_is_scrollable() {
        let cfg = Config {
            show_claude: true,
            show_codex: true,
            show_cursor: true,
            ..Default::default()
        };
        let snapshot = runtime::demo(Period::Today, GroupBy::Project);
        let mut state = State {
            details: true,
            ..Default::default()
        };
        let mut pending = false;
        let mut refresh = Instant::now();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let draw = |state: &State, terminal: &mut Terminal<ratatui::backend::TestBackend>| {
            terminal
                .draw(|f| render(f, &snapshot, &cfg, state))
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
        };
        let text = draw(&state, &mut terminal);
        assert!(text.contains("64.0% LEFT / 36.0% USED"));
        assert!(text.contains("28.0% LEFT / 72.0% USED"));
        assert!(text.contains('█'));
        assert!(text.contains("API ESTIMATE (USD)"));
        for _ in 0..10 {
            handle_key(
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                &mut state,
                &snapshot,
                &cfg,
                &mut pending,
                &mut refresh,
                (80, 24),
            );
        }
        assert_eq!(state.selected, 0);
        assert!(draw(&state, &mut terminal).contains("DAILY TOKENS"));
        for _ in 0..20 {
            handle_key(
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                &mut state,
                &snapshot,
                &cfg,
                &mut pending,
                &mut refresh,
                (80, 24),
            );
        }
        assert!(draw(&state, &mut terminal).contains("RECENT 24 HOURS"));
        handle_key(
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
            &mut state,
            &snapshot,
            &cfg,
            &mut pending,
            &mut refresh,
            (80, 24),
        );
        assert!(draw(&state, &mut terminal).contains("CONNECTION DETAILS"));
        let last = state.detail_scroll;
        handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut state,
            &snapshot,
            &cfg,
            &mut pending,
            &mut refresh,
            (80, 24),
        );
        assert_eq!(state.detail_scroll, last);
        handle_key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut state,
            &snapshot,
            &cfg,
            &mut pending,
            &mut refresh,
            (80, 24),
        );
        assert_eq!(state.detail_scroll, last - 1);
        handle_key(
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            &mut state,
            &snapshot,
            &cfg,
            &mut pending,
            &mut refresh,
            (80, 24),
        );
        assert_eq!(state.selected, 1);
        assert_eq!(state.detail_scroll, 0);
        handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
            &mut state,
            &snapshot,
            &cfg,
            &mut pending,
            &mut refresh,
            (80, 24),
        );
        assert_eq!(state.selected, 1);
        handle_key(
            KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
            &mut state,
            &snapshot,
            &cfg,
            &mut pending,
            &mut refresh,
            (80, 24),
        );
        assert_eq!(state.history_provider, Some(Provider::Codex));
        assert!(!state.details);
    }

    #[test]
    fn unpriced_dollar_graph_never_draws_unknown_as_zero() {
        let cfg = Config {
            show_claude: true,
            ..Default::default()
        };
        let mut snapshot = runtime::demo(Period::Today, GroupBy::Project);
        snapshot.provider_usage[0].hours[0]
            .totals
            .estimated_cost_usd = None;
        snapshot.provider_usage[0].periods[0]
            .totals
            .estimated_cost_usd = None;
        snapshot.provider_usage[0].periods[0].totals.unpriced_events = 1;
        let lines = detail_lines(
            &snapshot.providers[0],
            &snapshot,
            &cfg,
            &State {
                details: true,
                dollar_chart: true,
                ..Default::default()
            },
            78,
        );
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("USD chart unavailable: missing rates"));
        assert!(text.contains("$3.1600 + ?"));
        let lines = detail_lines(
            &snapshot.providers[0],
            &snapshot,
            &cfg,
            &State {
                details: true,
                ..Default::default()
            },
            30,
        );
        assert!(lines.iter().all(|l| l.width() <= 30));
        assert!(lines.iter().any(|l| l.to_string().contains('█')));
    }
}
