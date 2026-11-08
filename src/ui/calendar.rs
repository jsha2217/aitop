use crate::{
    config::Config,
    models::{format_number, UsageDay},
};
use chrono::{Datelike, Duration, Local};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::collections::BTreeMap;

const COLORS: [Color; 5] = [
    Color::Indexed(238),
    Color::Indexed(22),
    Color::Indexed(29),
    Color::Indexed(35),
    Color::Indexed(42),
];

fn level(tokens: u64, peak: u64) -> usize {
    if tokens == 0 || peak == 0 {
        0
    } else {
        ((tokens as f64 / peak as f64 * 4.0).ceil() as usize).clamp(1, 4)
    }
}

fn cell(value: usize, selected: bool, cfg: &Config) -> Span<'static> {
    let symbol = if cfg.ascii {
        [".", "1", "2", "3", "4"][value]
    } else if cfg.no_color {
        ["·", "░", "▒", "▓", "█"][value]
    } else {
        "■"
    };
    let mut style = if cfg.no_color {
        Style::default()
    } else {
        Style::default().fg(COLORS[value])
    };
    if selected {
        style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
    }
    Span::styled(symbol, style)
}

fn exact(tokens: u64) -> String {
    let digits = tokens.to_string();
    digits
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect::<Vec<_>>()
        .join(",")
}

/// Sunday-first GitHub-style grid. Gray cells mean no indexed local usage, not a complete account ledger.
pub(super) fn lines(
    days: &[UsageDay],
    scope: &str,
    cfg: &Config,
    width: u16,
    offset: u16,
    anchor: i64,
) -> Vec<Line<'static>> {
    let today = chrono::DateTime::from_timestamp(anchor, 0)
        .map(|t| t.with_timezone(&Local))
        .unwrap_or_else(Local::now)
        .date_naive();
    let first = today - Duration::days(364);
    let selected = today - Duration::days(i64::from(offset.min(364)));
    let room = usize::from(width.saturating_sub(2) / 2).max(1);
    let weeks = [53, 26, 13, 8, 4, 1]
        .into_iter()
        .find(|&n| n <= room)
        .unwrap_or(1);
    let mut end = today + Duration::days(6 - i64::from(today.weekday().num_days_from_sunday()));
    if selected < end - Duration::days(weeks as i64 * 7 - 1) {
        end = selected + Duration::days(6 - i64::from(selected.weekday().num_days_from_sunday()));
    }
    let start = end - Duration::days(weeks as i64 * 7 - 1);
    let lookup: BTreeMap<&str, &UsageDay> = days.iter().map(|d| (d.date.as_str(), d)).collect();
    let peak = days.iter().map(|d| d.tokens.total()).max().unwrap_or(0);
    let title_style = Style::default().add_modifier(Modifier::BOLD);
    let mut lines = vec![Line::from(Span::styled(
        format!(
            " DAILY TOKENS / {scope} / {}..{}",
            start.max(first).format("%Y-%m"),
            end.min(today).format("%Y-%m")
        ),
        title_style,
    ))];
    let mut months = vec![' '; weeks * 2 + 2];
    for col in 0..weeks {
        let date = start + Duration::weeks(col as i64);
        if date.day() <= 7 || col == 0 {
            for (i, c) in date.format("%b").to_string().chars().enumerate() {
                if let Some(slot) = months.get_mut(2 + col * 2 + i) {
                    *slot = c;
                }
            }
        }
    }
    lines.push(Line::from(months.into_iter().collect::<String>()));
    for row in 0..7 {
        let mut spans = vec![Span::raw(["S ", "M ", "T ", "W ", "T ", "F ", "S "][row])];
        for col in 0..weeks {
            let date = start + Duration::days((col * 7 + row) as i64);
            if date < first || date > today {
                spans.push(Span::raw("  "));
                continue;
            }
            let key = date.to_string();
            let tokens = lookup
                .get(key.as_str())
                .map(|d| d.tokens.total())
                .unwrap_or(0);
            spans.push(cell(level(tokens, peak), date == selected, cfg));
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }
    let mut legend = vec![Span::raw(" Less ")];
    for i in 0..5 {
        legend.push(cell(i, false, cfg));
        legend.push(Span::raw(" "));
    }
    legend.push(Span::raw(format!(
        "More / peak {}/day",
        format_number(peak)
    )));
    lines.push(Line::from(legend));
    let key = selected.to_string();
    let count = lookup.get(key.as_str());
    let value = if let Some(day) = count.filter(|d| d.events > 0) {
        format!("{} tokens", exact(day.tokens.total()))
    } else {
        "no indexed usage".into()
    };
    lines.push(Line::from(format!(
        " {selected}: {value}  [ / ] Date [0] Today"
    )));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Tokens;
    use chrono::TimeZone;

    #[test]
    fn calendar_keeps_empty_dates_and_color_levels_without_future_cells() {
        let anchor = Local
            .with_ymd_and_hms(2026, 11, 8, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let day = UsageDay {
            date: "2026-11-08".into(),
            tokens: Tokens {
                input: 1_000,
                ..Default::default()
            },
            events: 1,
        };
        let cfg = Config::default();
        let grid = lines(&[day], "ALL ENABLED", &cfg, 108, 0, anchor);
        let cells: Vec<_> = grid[2..9]
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.content == "■")
            .collect();
        assert_eq!(cells.len(), 365);
        assert_eq!(
            cells
                .iter()
                .filter(|s| s.style.fg == Some(COLORS[4]))
                .count(),
            1
        );
        assert_eq!(
            cells
                .iter()
                .filter(|s| s.style.fg == Some(COLORS[0]))
                .count(),
            364
        );
        assert_eq!(
            cells
                .iter()
                .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
                .count(),
            1
        );
        assert!(grid.last().unwrap().to_string().contains("1,000 tokens"));
        assert_eq!(
            [0, 1, 250, 251, 500, 501, 750, 751, 1000].map(|n| level(n, 1000)),
            [0, 1, 1, 2, 2, 3, 3, 4, 4]
        );
        let narrow = lines(
            &[],
            "ALL ENABLED",
            &Config {
                ascii: true,
                ..Default::default()
            },
            28,
            364,
            anchor,
        );
        assert!(narrow[2..9].iter().all(|l| l.width() <= 28));
        assert!(narrow.last().unwrap().to_string().contains("2025-11-09"));
        assert!(narrow[2..9].iter().all(|l| l.to_string().is_ascii()));
    }
}
