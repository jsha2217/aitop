# Data, pricing, and privacy

## What is read and stored

Local JSONL logs are read without modification. SQLite stores event IDs, hashed session identifiers, provider/model names, timestamps, token counts, project basenames with a short hash, and hashed file locations with incremental cursors. It does not store prompts, assistant text, code, full project paths, or credentials. The supported provider schemas are in [providers.md](providers.md).

The data directory is private to the user on Unix (`0700`); normalized JSON writes use private temporary files and atomic rename. SQLite commits each file's accepted events and cursor together. A crash before commit leaves both unchanged. A valid final JSON object is accepted without a trailing newline; an incomplete JSON fragment is retried next time. A record larger than 1 MiB is skipped and reported as partial.

Missing or changed file identities trigger rereading and event deduplication. Already indexed history is retained after source logs are removed. The cache currently has no automatic retention limit; check its size and manage it manually while aitop is stopped. Stop aitop before removing its derived cache to force a full rebuild; do not remove vendor logs. Database versions newer than the application are rejected. Database v2 preserves the previous AGY-derived records/cursors in `agy_legacy_events` and `agy_legacy_files`, then rebuilds the active AGY index from canonical transcripts to correct cache-token semantics and mirror-log duplication. Other providers and source logs are unchanged.

Deduplication uses available message/session IDs, or a normalized cumulative snapshot key. Records with no usable IDs and copied fragments may not establish complete coverage; the tool does not claim a complete billing ledger. Unsupported usage records can be omitted; partial coverage can only be identified for recognized errors and incomplete tails, not arbitrary unknown schemas.

## Token semantics

Canonical input excludes cache reads and cache writes. Output already includes reasoning when supplied as a subset. Total tokens add the four disjoint categories. Codex and nested AGY headless-style usage use inclusive input; Claude assistant usage and flat AGY transcript records with `cache_read_tokens` use exclusive input. The experimental generic Grok schema assumes exclusive input. Sources with different semantics need an adapter, not an inferred conversion. AGY terminal/cumulative reports must not be mixed with per-step deltas from the same session.

Periods are local calendar days: today, today plus six preceding days, or today plus 29 preceding days. Events are bucketed by timestamp, never file modification time. Future events are excluded. History is scoped to enabled providers and accepted local records. Account ownership, other devices, deleted logs, and unsupported records are not reconstructed.

Quota reset labels use a clock symbol. When the provider omits a reset timestamp, the UI displays the known window length with `max` (for example `5h max` or `7d max`); the normalized `resets_at` remains `null`. Unknown window lengths display `--`. Expired observations still require a fresh query.

## Daily token calendars

Each provider stores a 365-day dense array of local dates in `provider_usage[].days`. Each day adds the four disjoint token categories from accepted events, using event timestamps rather than file times. Future events are excluded. Empty days have no indexed events; they do not prove account-wide zero usage. History combines enabled providers independently of its table filter; provider details remain provider-specific. Color intensity uses four equal nonzero bands relative to the largest daily token total in that calendar. Narrow layouts show fewer weeks, and date navigation moves the visible window. Partial collection remains labeled.

## Automatic prices and exact-model overrides

Recognized models are priced automatically from the bundled [official-rate catalog](../assets/pricing.json), verified on 2026-11-08 against [OpenAI API pricing](https://developers.openai.com/api/docs/pricing) and [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing). There is no live price lookup or model-family guessing. A release updates the catalog; the catalog verification date is displayed in the app. An optional `~/.config/aitop/prices.json` with a `models` map overrides matching entries and preserves the other bundled rates. Every model entry needs `input`, `output`, and `cache_read` rates in USD per million tokens. Supply `cache_write` when cache writes occur; omitting it makes those events unpriced instead of treating writes as free. Model IDs must exactly match History's model grouping.

An empty, valid starting file is:

```json
{"models": {}}
```

For a purely fictional model, the schema looks like this (these are **not vendor prices**):

```json
{
  "models": {
    "fictional-example-model": {
      "input": 2.0,
      "output": 8.0,
      "cache_read": 0.2,
      "cache_write": 2.5
    }
  }
}
```

Bundled and overridden rates are applied retrospectively to the selected local history. Claude Haiku 5.5's prompt-length tier is evaluated per recorded event before period/hour aggregation. Other speed, region, historical price changes, tool charges, subscription fees, taxes and actual invoices are not reconstructed. Claude cache writes use a 5-minute baseline; OpenAI writes use the catalog's 30-minute baseline. A record with a different cache duration can have a different actual cost. Costs are API-equivalent estimates only. Zero-token synthetic records have a zero token estimate; AGY records without a model identity remain unpriced. Claude Code also [computes its /usage dollar estimate locally](https://code.claude.com/docs/en/costs#using-the-usage-command), rather than returning an authoritative subscription bill. A model with an unknown price makes its row cost and the aggregate cost `null`; token counts remain available. Invalid pricing is reported and never turns missing prices into zero.

Provider details show today / 7d / 30d token totals and USD estimates, the selected period's four token categories, and the top three models/projects by tokens with their costs. When only some records have rates, `priced_cost_usd` preserves their subtotal; the complete `estimated_cost_usd` remains `null`. The UI marks a subtotal as `$amount + ?` and counts the unpriced events. An entirely unpriced set has no dollar subtotal. No accepted local history means unavailable, rather than a fabricated zero bill.

The recent graph covers 24 consecutive elapsed-hour buckets ending in the current partial local hour. Press `c` to switch between tokens and USD. Missing rates suppress the USD graph and leave the token graph available; they never become zero-dollar bars. Each provider's periods and hourly aggregates are read in one SQLite transaction. No transcript scan is triggered by opening details. `provider_usage` in status JSON includes these aggregates and their time boundaries. It does not provide quota history: quota caches still contain only the last observation.

## Authentication and network

No telemetry, aitop login, central server, cookie scraping, account switching, or credential export. Native adapters send existing credentials only to built-in HTTPS vendor endpoints, with redirects disabled. HTTP errors expose safe descriptions rather than raw bodies or headers. Quota caches contain normalized metrics and a hash of credential-file metadata used for invalidation, not credentials. Claude Keychain access requires an explicit saved permission. `--keychain` grants and remembers it; the Settings toggle or `aitop config set claude_keychain false` revokes it. Missing access keeps an error beside the last observation instead of silently reporting a successful refresh. An explicitly enabled vendor CLI is a separate process and can perform its normal authentication maintenance.

`--offline`, `history`, `statusline --cached`, and local `doctor` do not call provider APIs. `demo` does not read/write the real configuration, history, or credentials. Tests use isolated fixture homes. `doctor --network` explicitly allows the same provider probes as `status`.

When reporting bugs, share your OS, aitop/provider version, safe diagnostic state, and a minimal synthetic fixture. Do not share auth files or complete conversation logs.
