use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use crate::models::{format_number, AgentSummary, AllUsageStats, AppConfig, BarMode};

pub const INTERVAL_PRESETS: &[u64] = &[30, 60, 180, 300, 600, 900, 1800];
pub const SETTINGS_ROW_COUNT: usize = 8;
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn spinner_char(tick: usize) -> &'static str {
    SPINNER_FRAMES[tick % SPINNER_FRAMES.len()]
}

pub fn format_interval(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs % 60 == 0 {
        format!("{}m", secs / 60)
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

fn hp_color(rem_pct: u16) -> Color {
    if rem_pct <= 15 {
        Color::Red
    } else if rem_pct <= 40 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn parse_label_parts(label: &str) -> (&str, &str) {
    let name = label.split(':').next().unwrap_or(label).trim();
    let reset = label.split("·").nth(1).unwrap_or("-").trim();
    (name, reset)
}

fn render_compact_bar_row<'a>(
    used_pct: u16,
    label: &'a str,
    bar_width: usize,
    bar_mode: BarMode,
) -> Line<'a> {
    let clamped_used = used_pct.min(100);
    let rem_pct = 100u16.saturating_sub(clamped_used);

    let (display_pct, color) = match bar_mode {
        BarMode::Remaining => (rem_pct, hp_color(rem_pct)),
        BarMode::Used => (clamped_used, Color::White),
    };

    let (name, reset) = parse_label_parts(label);

    let filled = ((display_pct as usize) * bar_width + 50) / 100;
    let filled = filled.min(bar_width);
    let empty = bar_width.saturating_sub(filled);

    let filled_str = "█".repeat(filled);
    let empty_str = "░".repeat(empty);

    Line::from(vec![
        Span::raw("   "),
        Span::styled(
            format!("{:<22}", name),
            Style::default().fg(Color::Gray),
        ),
        Span::raw(" "),
        Span::styled(filled_str, Style::default().fg(color)),
        Span::styled(empty_str, Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {:>3}%", display_pct),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            format!("⏱ {}", reset),
            Style::default().fg(Color::White),
        ),
    ])
}

fn render_agent_section<'a>(
    lines: &mut Vec<Line<'a>>,
    agent: &'a AgentSummary,
    theme_color: Color,
    bar_width: usize,
    bar_mode: BarMode,
    is_loading: bool,
    spin: &'static str,
) {
    let mut header_spans = vec![
        Span::raw("  "),
        Span::styled(
            format!("{} ", agent.name),
            Style::default().fg(theme_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({})", agent.provider),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw("   "),
    ];

    if is_loading && agent.usage.session_usage_label.is_empty() {
        header_spans.push(Span::styled(
            format!("{} Loading...", spin),
            Style::default().fg(Color::Cyan),
        ));
    } else {
        header_spans.push(Span::styled(
            format!(
                "{} sessions · {} tok · ${:.2}",
                agent.session_count,
                format_number(agent.tokens.total_tokens),
                agent.tokens.estimated_cost_usd
            ),
            Style::default().fg(Color::DarkGray),
        ));
        if is_loading {
            header_spans.push(Span::raw("  "));
            header_spans.push(Span::styled(
                format!("{} Syncing", spin),
                Style::default().fg(Color::Cyan),
            ));
        }
    }

    lines.push(Line::from(header_spans));

    let u = &agent.usage;
    if u.session_usage_label.is_empty() && u.weekly_usage_label.is_empty() && is_loading {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                format!("{} Fetching live quota...", spin),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        return;
    }

    if !u.session_usage_label.is_empty() {
        lines.push(render_compact_bar_row(
            u.session_usage_pct,
            &u.session_usage_label,
            bar_width,
            bar_mode,
        ));
    }
    if !u.weekly_usage_label.is_empty() {
        lines.push(render_compact_bar_row(
            u.weekly_usage_pct,
            &u.weekly_usage_label,
            bar_width,
            bar_mode,
        ));
    }
    for extra in &u.extra_bars {
        lines.push(render_compact_bar_row(
            extra.pct,
            &extra.label,
            bar_width,
            bar_mode,
        ));
    }
}

pub fn render_ui(
    f: &mut Frame,
    stats: &AllUsageStats,
    cfg: &AppConfig,
    status_msg: Option<&str>,
    show_settings: bool,
    settings_cursor: usize,
    loading_mask: &[bool; 6],
    spinner_tick: usize,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Min(14),   // Clean Unified Board
            Constraint::Length(2), // Minimal Footer
        ])
        .split(f.area());

    render_unified_board(f, chunks[0], stats, cfg, loading_mask, spinner_tick);
    render_footer(f, chunks[1], stats, status_msg, show_settings);

    if show_settings {
        render_settings_modal(f, f.area(), cfg, settings_cursor, loading_mask, spinner_tick);
    }
}

fn render_unified_board(
    f: &mut Frame,
    area: Rect,
    stats: &AllUsageStats,
    cfg: &AppConfig,
    loading_mask: &[bool; 6],
    spinner_tick: usize,
) {
    let mode_tag = match cfg.bar_mode {
        BarMode::Remaining => "Remaining",
        BarMode::Used => "Used",
    };
    let spin = spinner_char(spinner_tick);
    let any_loading = loading_mask.iter().any(|&b| b);

    let mut title_spans = vec![
        Span::styled(
            " ⚡ aitop ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "· Updated {} · Refresh {} · {} ",
                if stats.last_updated.is_empty() {
                    "-"
                } else {
                    &stats.last_updated
                },
                format_interval(cfg.refresh_interval_secs),
                mode_tag
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    if any_loading {
        title_spans.push(Span::styled(
            format!("· {} Syncing ", spin),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
    }

    let title = Line::from(title_spans);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let bar_width = (inner.width as usize)
        .saturating_sub(48)
        .clamp(16, 30);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));

    let sections: &[(usize, bool, &AgentSummary, Color)] = &[
        (0, cfg.show_claude, &stats.claude, Color::Rgb(217, 119, 87)),
        (1, cfg.show_codex, &stats.codex, Color::Rgb(16, 163, 127)),
        (2, cfg.show_agy, &stats.agy, Color::Rgb(66, 133, 244)),
        (3, cfg.show_cursor, &stats.cursor, Color::Rgb(0, 180, 216)),
        (4, cfg.show_copilot, &stats.copilot, Color::Rgb(139, 92, 246)),
        (5, cfg.show_grok, &stats.grok, Color::Rgb(220, 220, 220)),
    ];

    let mut any_shown = false;
    for &(idx, enabled, agent, color) in sections {
        if enabled {
            render_agent_section(
                &mut lines,
                agent,
                color,
                bar_width,
                cfg.bar_mode,
                loading_mask[idx],
                spin,
            );
            lines.push(Line::from(""));
            any_shown = true;
        }
    }

    if !any_shown {
        lines.push(Line::from(Span::styled(
            "   No agents enabled. Press [s] to open Settings and enable agents.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

fn render_footer(
    f: &mut Frame,
    area: Rect,
    stats: &AllUsageStats,
    status_msg: Option<&str>,
    show_settings: bool,
) {
    let msg = if let Some(m) = status_msg {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" STATUS ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::raw(" "),
            Span::styled(m, Style::default().fg(Color::Yellow)),
        ])
    } else if show_settings {
        Line::from(vec![
            Span::raw(" "),
            Span::styled("[↑/↓]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" Select  ", Style::default().fg(Color::Gray)),
            Span::styled("[←/→/Space]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" Change  ", Style::default().fg(Color::Gray)),
            Span::styled("[s/Esc]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" Close", Style::default().fg(Color::Gray)),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                format!(
                    " Total: {} tokens (${:.2})   ",
                    format_number(stats.total_tokens),
                    stats.total_cost_usd
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("[r]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" Refresh  ", Style::default().fg(Color::Gray)),
            Span::styled("[s]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" Settings  ", Style::default().fg(Color::Gray)),
            Span::styled("[Space]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" Pause  ", Style::default().fg(Color::Gray)),
            Span::styled("[q]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" Quit", Style::default().fg(Color::Gray)),
        ])
    };

    let paragraph = Paragraph::new(msg);
    f.render_widget(paragraph, area);
}

fn render_settings_modal(
    f: &mut Frame,
    area: Rect,
    cfg: &AppConfig,
    cursor: usize,
    loading_mask: &[bool; 6],
    spinner_tick: usize,
) {
    let modal_width = 64u16.min(area.width.saturating_sub(4));
    let modal_height = 16u16.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(modal_width)) / 2;
    let y = area.y + (area.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    f.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " ⚙ Settings ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(modal_area);
    f.render_widget(block, modal_area);

    let spin = spinner_char(spinner_tick);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));

    // Row 0: Refresh Interval
    let row0_selected = cursor == 0;
    let mut r0_spans = vec![
        Span::styled(
            if row0_selected { " ▸ " } else { "   " },
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<18}", "Refresh Interval"),
            Style::default()
                .fg(if row0_selected { Color::White } else { Color::Gray })
                .add_modifier(if row0_selected { Modifier::BOLD } else { Modifier::empty() }),
        ),
    ];
    for &preset in INTERVAL_PRESETS {
        let label = format!(" {} ", format_interval(preset));
        if preset == cfg.refresh_interval_secs {
            r0_spans.push(Span::styled(
                label,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            r0_spans.push(Span::styled(label, Style::default().fg(Color::DarkGray)));
        }
    }
    lines.push(Line::from(r0_spans));

    // Row 1: Bar Mode
    let row1_selected = cursor == 1;
    let rem_active = cfg.bar_mode == BarMode::Remaining;
    let r1_spans = vec![
        Span::styled(
            if row1_selected { " ▸ " } else { "   " },
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<18}", "Bar Style"),
            Style::default()
                .fg(if row1_selected { Color::White } else { Color::Gray })
                .add_modifier(if row1_selected { Modifier::BOLD } else { Modifier::empty() }),
        ),
        Span::styled(
            " Remaining ",
            if rem_active {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
        Span::raw(" "),
        Span::styled(
            " Used ",
            if !rem_active {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ];
    lines.push(Line::from(r1_spans));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "   Visible Agents",
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    )));

    let agent_rows: &[(usize, usize, &str, bool, Color)] = &[
        (2, 0, "Claude Code", cfg.show_claude, Color::Rgb(217, 119, 87)),
        (3, 1, "Codex CLI", cfg.show_codex, Color::Rgb(16, 163, 127)),
        (4, 2, "Antigravity (AGY)", cfg.show_agy, Color::Rgb(66, 133, 244)),
        (5, 3, "Cursor", cfg.show_cursor, Color::Rgb(0, 180, 216)),
        (6, 4, "GitHub Copilot", cfg.show_copilot, Color::Rgb(139, 92, 246)),
        (7, 5, "Grok (xAI)", cfg.show_grok, Color::Rgb(220, 220, 220)),
    ];

    for &(row_idx, agent_idx, name, enabled, brand_color) in agent_rows {
        let sel = cursor == row_idx;
        let check = if enabled { "[✓] ON " } else { "[ ] OFF" };
        let check_style = if enabled {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let mut row_spans = vec![
            Span::styled(
                if sel { " ▸ " } else { "   " },
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:<22}", name),
                Style::default()
                    .fg(if sel { brand_color } else { Color::Gray })
                    .add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() }),
            ),
            Span::styled(check, check_style),
        ];
        if loading_mask[agent_idx] {
            row_spans.push(Span::raw("  "));
            row_spans.push(Span::styled(
                format!("{} Loading", spin),
                Style::default().fg(Color::Cyan),
            ));
        }
        lines.push(Line::from(row_spans));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "   ↑/↓ Move · ←/→/Space Toggle · s/Esc Close",
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines).alignment(Alignment::Left);
    f.render_widget(paragraph, inner);
}
