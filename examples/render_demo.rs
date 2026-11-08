//! Render the real Ratatui demo buffer for documentation. Not a hand-drawn UI.
use aitop::{
    config::Config,
    models::{GroupBy, Period},
    runtime, ui,
};
use ratatui::{
    backend::TestBackend,
    style::{Color, Modifier},
    Terminal,
};

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn color(c: Color, default: &str) -> String {
    match c {
        Color::Green | Color::LightGreen => "#8ad5ac".into(),
        Color::Yellow | Color::LightYellow => "#eac576".into(),
        Color::Red | Color::LightRed => "#ee9b8e".into(),
        Color::Black => "#111613".into(),
        Color::White => "#e8eee9".into(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) if i >= 232 => {
            let gray = 8 + (i - 232) * 10;
            format!("#{gray:02x}{gray:02x}{gray:02x}")
        }
        Color::Indexed(i) if i >= 16 => {
            let n = i - 16;
            let levels = [0, 95, 135, 175, 215, 255];
            format!(
                "#{:02x}{:02x}{:02x}",
                levels[(n / 36) as usize],
                levels[(n / 6 % 6) as usize],
                levels[(n % 6) as usize]
            )
        }
        _ => default.into(),
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1);
    let width = std::env::args()
        .nth(2)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(110);
    let height = std::env::args()
        .nth(3)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(30);
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    let cfg = Config {
        show_claude: true,
        show_codex: true,
        show_cursor: true,
        ..Default::default()
    };
    let snapshot = runtime::demo(Period::Today, GroupBy::Project);
    let view = match mode.as_deref() {
        Some("settings") => ui::View::Settings,
        Some("history" | "calendar") => ui::View::History,
        _ => ui::View::Overview,
    };
    terminal.draw(|f| {
        ui::render(
            f,
            &snapshot,
            &cfg,
            &ui::State {
                view,
                details: matches!(
                    mode.as_deref(),
                    Some("details" | "usage" | "detail-calendar")
                ),
                detail_scroll: match mode.as_deref() {
                    Some("usage") => 31,
                    Some("detail-calendar") => 17,
                    _ => 0,
                },
                ..Default::default()
            },
        )
    })?;
    let svg_width = width as usize * 9;
    let svg_height = height as usize * 18 + 24;
    println!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{svg_width}\" height=\"{svg_height}\" viewBox=\"0 0 {svg_width} {svg_height}\"><title>aitop demo — rendered from the actual terminal UI</title><rect width=\"{svg_width}\" height=\"{svg_height}\" fill=\"#111613\"/><g font-family=\"Menlo,Consolas,monospace\" font-size=\"13\" xml:space=\"preserve\">");
    let buffer = terminal.backend().buffer();
    for y in 0..height {
        for x in 0..width {
            let c = &buffer[(x, y)];
            let reversed = c.modifier.contains(Modifier::REVERSED);
            let px = 9 + x as usize * 9;
            let py = 20 + y as usize * 18;
            let mut foreground = color(c.fg, "#e8eee9");
            let mut background = color(c.bg, "#111613");
            if reversed {
                std::mem::swap(&mut foreground, &mut background);
            }
            if background != "#111613" {
                println!(
                    "<rect x=\"{px}\" y=\"{}\" width=\"9\" height=\"18\" fill=\"{background}\"/>",
                    py - 13
                );
            }
            if c.symbol() != " " {
                println!(
                    "<text x=\"{px}\" y=\"{py}\" fill=\"{}\"{}>{}</text>",
                    foreground,
                    if c.modifier.contains(Modifier::BOLD) {
                        " font-weight=\"bold\""
                    } else {
                        ""
                    },
                    escape(c.symbol())
                );
            }
        }
    }
    println!("</g></svg>");
    Ok(())
}
