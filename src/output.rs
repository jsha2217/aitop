use crate::models::{format_number, BarMode, HistoryReport, Snapshot};

pub fn status(snapshot: &Snapshot, mode: BarMode) -> String {
    let now = snapshot.generated_at;
    let mut out = format!(
        "aitop {}{}\n",
        env!("CARGO_PKG_VERSION"),
        if snapshot.demo { " / DEMO DATA" } else { "" }
    );
    if snapshot.providers.is_empty() {
        out.push_str("No providers enabled. Run aitop and open Settings [s].\n");
    }
    for q in &snapshot.providers {
        out.push_str(&format!(
            "\n{}  [{}]\n",
            q.provider.name(),
            q.status_label(now)
        ));
        for w in &q.windows {
            let value = if w.expired(now) {
                "--".into()
            } else {
                format!(
                    "{:.0}% {}",
                    w.percent(mode),
                    if mode == BarMode::Remaining {
                        "left"
                    } else {
                        "used"
                    }
                )
            };
            out.push_str(&format!(
                "  {:<20} {:>10}  {}\n",
                w.label,
                value,
                w.reset_label(now)
            ));
        }
        out.push_str(&format!(
            "  Source: {} | observed: {}\n",
            q.source,
            q.observed_at
                .map(|t| chrono::DateTime::from_timestamp(t, 0)
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_else(|| "unknown".into()))
                .unwrap_or_else(|| "unknown".into())
        ));
        if !q.message.is_empty() {
            out.push_str(&format!("  {}\n", q.message));
        }
        if let Some(error) = &q.refresh_error {
            out.push_str(&format!("  Last refresh: {error}\n"));
        }
    }
    out.push_str(&format!(
        "\n{} local tokens: {} | API-equivalent estimate: {}\n",
        snapshot.history.period.label(),
        snapshot
            .history
            .total_tokens
            .map(format_number)
            .unwrap_or_else(|| "unavailable".into()),
        estimated_money(
            snapshot.history.estimated_cost_usd,
            snapshot.history.priced_cost_usd
        )
    ));
    if snapshot.history.partial {
        out.push_str("History is PARTIAL.\n");
    }
    out
}
pub fn money(value: Option<f64>) -> String {
    value
        .map(|v| format!("${v:.4}"))
        .unwrap_or_else(|| "unavailable".into())
}
pub fn estimated_money(total: Option<f64>, priced: Option<f64>) -> String {
    if total.is_some() {
        money(total)
    } else if priced.is_some() {
        format!("{} + ?", money(priced))
    } else {
        "unavailable".into()
    }
}
pub fn history(report: &HistoryReport) -> String {
    let mut out = format!(
        "{} / local history / {:?}\n{}\n",
        report.period.label(),
        report.group_by,
        report.scope
    );
    if !report.available {
        out.push_str("No supported local usage records indexed.\n");
    }
    for row in &report.rows {
        out.push_str(&format!(
            "{:<30} {:>10} tokens  {}\n",
            row.label,
            format_number(row.tokens),
            estimated_money(row.estimated_cost_usd, row.priced_cost_usd)
        ));
    }
    if report.partial {
        out.push_str("PARTIAL: some records could not be read or attributed.\n");
    }
    for warning in &report.warnings {
        out.push_str(&format!("{warning}\n"));
    }
    out
}
pub fn failed(snapshot: &Snapshot) -> bool {
    !snapshot.demo
        && (!snapshot.warnings.is_empty()
            || snapshot.history.partial
            || snapshot.providers.iter().any(|q| {
                q.availability != crate::models::Availability::Available
                    || q.refresh_error.is_some()
            }))
}
