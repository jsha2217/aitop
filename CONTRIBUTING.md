# Contributing

Use a current stable Rust toolchain. Run:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Tests must use isolated homes and synthetic/sanitized fixtures. Never depend on a contributor's signed-in provider accounts. Do not commit credentials, prompts, private paths, or full conversation logs.

For a provider change, include a minimal schema fixture, observed vendor version, missing-field behavior, and the correct token semantics. Unknown quota is unavailable, not 0% used. Preserve source timestamps through cache/fallback. A parser that accepts a fixture is not automatically certified for all live versions.

For accounting, test repeated/cumulative events, cache inclusion, period boundaries, and restart/rotation behavior. For UI changes, cover 80×24 and narrow screens, keyboard behavior, and no-color/ASCII. The demo renderer uses the real Ratatui buffer and should stay aligned with the app.

Issues should include aitop version, provider version, OS, safe diagnostic state, and a small reproduction. Do not attach auth files or raw conversation logs. See [data handling](docs/data.md).
