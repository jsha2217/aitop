# Provider sources

Support describes fixture-tested parsers, not live account certification. The exact vendor CLI/app version must still be checked before release. Unsupported fields are never replaced with invented limits.

## Claude Code

History reads `~/.claude/projects/**/*.jsonl`, assistant `message.usage` records with an event timestamp. Cache reads/writes are separate from ordinary input. Repeated message IDs within a session update one event rather than adding it twice. `stats-cache.json` is not added to the same history, preventing double counting.

For quota observations, a Claude statusline command can pass its input to:

```sh
aitop ingest claude
```

This accepts JSON on stdin and prints a short quota summary. It stores only supported quota fields, the capture time, and the source. It does not change Claude settings or persist the stdin body. Configure it manually according to [Claude's statusline documentation](https://code.claude.com/docs/en/statusline). If you already use a statusline, use a wrapper that supplies the same input to both commands; do not blindly replace it.

Fields: `rate_limits.five_hour` / `seven_day`, `used_percentage`, `resets_at` (epoch seconds). Windows can be absent. A capture updates only when Claude invokes the statusline; it is not a continuously polled quota API and has unknown account attribution.

The legacy OAuth adapter uses `.claude/.credentials.json`, or macOS Keychain with saved Keychain permission or `--keychain`. It calls Anthropic directly and is experimental. The Keychain lookup is bounded but may ask for OS access. `--keychain` remembers permission across launches; revoke it in Settings or with `aitop config set claude_keychain false`. A missing credential or denied Keychain lookup keeps a visible error beside the last observation. No login/refresh token is written by aitop.

## Codex

History reads `.codex/sessions` and `.codex/archived_sessions`. Supported records: `session_meta`, `turn_context`, `event_msg` → `token_count` → `info.total_token_usage`, and legacy `token_usage_record` → `payload.usage`. Cached input is subtracted from inclusive input before pricing. Reasoning is not added again on top of output. Timestamped cumulative differences are indexed; repeated cumulative snapshots are not added twice. Regressing/invalid counters are partial, not guessed.

Quota reads timestamped `payload.rate_limits` snapshots. Legacy OAuth uses `tokens.access_token` / `account_id` from `.codex/auth.json` and a direct experimental usage endpoint. API-key-only authentication is not subscription quota authentication.

`--cli` instead opts in to `codex app-server` and `account/rateLimits/read`. The helper has an 8-second deadline and 1 MiB output cap, and is terminated/reaped after the response. Missing windows and multi-bucket replies are accepted. This path requires an installed, compatible Codex CLI. See [App Server documentation](https://learn.chatgpt.com/docs/app-server). Its normal authentication maintenance belongs to Codex.

## Antigravity

Enabling AGY in Settings or configuration enables its read-only quota query automatically. No `--cli` or `--agy-cli` flag is required; `--agy-cli` is retained for compatibility. Offline mode disables the query. The adapter checks the installed version first (5-second limit), then runs `agy -p /usage --output-format json --print-timeout 20s` in a private working directory with a separate 25-second process deadline, including startup and shutdown. Automatic updates are disabled for the probe. Unknown versions and versions below 1.1.11 are not queried: Google's [changelog](https://antigravity.google/docs/changelog) documents the introduction of read-only print-mode commands. No text/TUI output is guessed.

The experimental parser accepts `command.name == "usage"` and grouped quota buckets. It rejects explicit failure statuses, nonzero agent turns, disabled buckets, and invalid fractions. A synthetic example of the accepted shape:

```json
{"command":{"name":"usage","data":{"groups":[{"name":"Example group","buckets":[{"id":"example-5h","window":"5h","remaining_fraction":0.7,"reset_time":1999999999}]}]}}}
```

Gemini, Claude/GPT, and Argon quota pools keep separate bucket IDs. The grouped shape was described in [a direct CLI observation](https://github.com/omacom/omarchy/issues/16906); it is fixture-tested and was also checked locally with AGY 1.5.3. This does not certify other versions, accounts or layouts. The CLI can have its own login behavior; use its own terminal login first. Ordinary AGY failures retain the last observation and permit another query after 60 seconds, without escalating to a 32-minute cooldown. Automatic attempts still follow the configured dashboard interval; `r` bypasses this local cooldown and requests a live query. Old query-disabled caches and obsolete extended failure delays are handled on upgrade.

History reads only canonical `.gemini/antigravity-cli/brain/**/transcript.jsonl` records with explicit usage and timestamps; mirror `transcript_full.jsonl` and per-step files are excluded. Flat transcript records with `cache_read_tokens` use input counts exclusive of cache. Nested headless-style usage retains inclusive-input semantics. Unknown layouts are not estimated from file size.

## Cursor

The existing database is opened read-only (`Library/Application Support/Cursor/User/globalStorage/state.vscdb` on macOS or `.config/Cursor/User/globalStorage/state.vscdb` on Linux). The experimental legacy profile endpoint must supply both numeric `numRequests` and positive `maxRequestUsage`; no default 500-request cap or monthly reset is assumed. Missing fields produce unavailable data. Token/cost history is unsupported.

## GitHub Copilot

Reads existing CLI login files in `.config/github-copilot/{hosts,apps}.json`. The experimental internal endpoint must supply `quota_snapshots.premium_interactions.percent_remaining`. Reset timestamps come from `quota_reset_date_utc`; no calendar reset is invented. Token/cost history is unsupported.

## Grok

No verified quota source is implemented. It never displays fixed “100% left / 2h”. Local `.grok/**/*.jsonl` records are accepted only if they include timestamps and explicit usage fields. File size is not a token count. This generic history adapter is experimental and requires real sanitized samples for certification.

## Shared rules

Native HTTPS requests have a 5-second total timeout, a 1 MiB response cap, and disabled redirects. Credentials never become process arguments in native HTTP adapters. Standard proxy configuration is honored by the HTTP client. Automatic refresh caches successful/failed probes for at least 60 seconds; Retry-After can extend that interval. Manual `r` bypasses the local cache and sends a new request; the server may still reject or rate-limit that request. Manual refresh never overrides offline mode.

Offline mode reads local logs and snapshots only. Stored source observation times are preserved. Local history cannot establish which signed-in account owns every old record, so no account-wide totals or cross-account guarantees are made.
