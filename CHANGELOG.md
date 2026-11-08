# Changes

## 0.2.0-alpha.5 — source preview

- Fixed `1/2/3` view switching from Settings and changed refresh presets to `30s`, `1m`, `3m`, `5m`, `10m`, and `15m`, with minute labels for longer intervals.
- Added automatic USD estimates from a bundled, dated official Claude/OpenAI price catalog, with optional exact-model overrides. Haiku prompt tiers are applied per request; unknown model costs remain explicit and known subtotals stay visible.

## 0.2.0-alpha.4 — source preview

- Replaced reset text prefixes with a clock symbol. Missing reset timestamps display the known window maximum, such as `5h max`, without fabricating a reset deadline.

- Separated temporary display flags and `NO_COLOR` from persisted preferences so saving authentication or interval settings cannot turn off colors. Added a run-only `--color` override and labeled temporary overrides in Settings.

## 0.2.0-alpha.3 — source preview

- Persisted explicit Claude Keychain permission across launches and added a Settings toggle to revoke it. Missing credential access now stays visible alongside cached quota values; older suppressed-auth caches recover after permission is granted.

## 0.2.0-alpha.2 — source preview

- Added combined daily token heatmaps in History and provider-specific heatmaps in details, with 365 local calendar days, visible empty cells, four usage colors, date selection, and ASCII/no-color fallbacks.
- Made manual `r` refresh bypass local success/failure cache delays; concurrent manual requests are coalesced. Automatic refresh retains its caching policy.
- Corrected AGY transcript cache-token semantics and excluded mirror/step logs. Database v2 archives the old AGY index before rebuilding, leaving other providers intact. Valid JSONL records without a final newline are accepted.

## 0.2.0-alpha.1 — source preview

- Rebuilt domain, collection, storage, CLI, and TUI around shared normalized data.
- Added incremental SQLite history, timestamp-based periods, duplicate handling, explicit cache token semantics, and exact-model user pricing.
- Added full-width, scrollable provider details with all quota gauges, local reset timestamps, today/7d/30d USD/token summaries, 24-hour token/USD charts, top models/projects, and filtered History navigation. Unknown rates stay visible; partial dollar subtotals are labeled explicitly.
- Added JSON, history grouping, local doctor/provider diagnostics, offline mode, synthetic demo, statusline capture, and cache-only tmux output.
- Replaced repeated provider-colored panels with one responsive Overview / History / Settings interface, ASCII/no-color support, and demo-generated graphics.
- Removed fabricated Grok limits and byte-size token estimates, arbitrary subscription caps, and invented monthly resets.
- Restored automatic AGY usage queries for enabled providers, allowed 20 seconds plus process startup/shutdown margin, separated safe command errors, and capped ordinary AGY failure cooldowns at 60 seconds. Existing long failure caches recover without deletion.
- Preserved `--once`, `-i` persistence, remaining/used mode, pause, and manual refresh.
- Retained six provider targets, with unsupported features and experimental source compatibility reported explicitly.
- Added fixture/CLI/process/UI regression tests, macOS/Linux CI, local release packaging, and rewritten English/Korean documentation.

Personal dollar budgets, standalone weekly summaries, and cache-efficiency analysis are excluded. Exhaustion forecasts, live session tracking, multi-account UI, Windows, automatic third-party configuration editing, and extra promotional media are deferred. Basic quota notifications are not implemented in this preview. No release/tap has been published and no live-account certification is claimed.
