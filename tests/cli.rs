use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "aitop-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_aitop"));
        c.arg("--home")
            .arg(&self.0)
            .arg("--data-dir")
            .arg(self.0.join("cache"));
        c
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn demo_has_no_user_side_effects() {
    let home = Home::new();
    let output = home.command().args(["demo", "--json"]).output().unwrap();
    assert!(output.status.success());
    let data: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(data["demo"], true);
    assert_eq!(data["history"]["total_tokens"], 1_240_000);
    assert_eq!(fs::read_dir(&home.0).unwrap().count(), 0);
}

#[test]
fn keychain_permission_is_saved_once_and_can_be_revoked_without_a_live_lookup() {
    let home = Home::new();
    assert!(home
        .command()
        .args(["--keychain", "--offline", "--once"])
        .output()
        .unwrap()
        .status
        .success());
    let saved = home.command().arg("config").output().unwrap();
    let cfg: Value = serde_json::from_slice(&saved.stdout).unwrap();
    assert_eq!(cfg["claude_keychain"], true);
    assert!(home
        .command()
        .args(["config", "set", "claude_keychain", "false"])
        .output()
        .unwrap()
        .status
        .success());
    let saved = home.command().arg("config").output().unwrap();
    let cfg: Value = serde_json::from_slice(&saved.stdout).unwrap();
    assert_eq!(cfg["claude_keychain"], false);
}

#[test]
fn temporary_display_flags_and_no_color_environment_do_not_leak_into_saved_settings() {
    let home = Home::new();
    assert!(home
        .command()
        .env("NO_COLOR", "1")
        .args([
            "--ascii",
            "--no-color",
            "config",
            "set",
            "claude_keychain",
            "true"
        ])
        .output()
        .unwrap()
        .status
        .success());
    assert!(home
        .command()
        .env("NO_COLOR", "1")
        .args(["--ascii", "--no-color", "--offline", "--once", "-i", "60"])
        .output()
        .unwrap()
        .status
        .success());
    let result = home.command().arg("config").output().unwrap();
    let cfg: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(cfg["ascii"], false);
    assert_eq!(cfg["no_color"], false);
    assert_eq!(cfg["claude_keychain"], true);
    assert_eq!(cfg["refresh_interval_secs"], 60);
    assert!(
        home.command()
            .args(["--color", "--no-color", "demo"])
            .output()
            .unwrap()
            .status
            .code()
            == Some(2)
    );
}
#[test]
fn help_and_version_do_not_read_broken_configuration() {
    let home = Home::new();
    fs::create_dir_all(home.0.join(".config/aitop")).unwrap();
    fs::write(home.0.join(".config/aitop/config.json"), "broken").unwrap();
    for flag in ["--help", "--version"] {
        assert!(home.command().arg(flag).output().unwrap().status.success());
    }
}
#[test]
fn old_interval_option_still_persists() {
    let home = Home::new();
    let output = home
        .command()
        .args(["--offline", "--once", "-i", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let cfg: Value =
        serde_json::from_slice(&fs::read(home.0.join(".config/aitop/config.json")).unwrap())
            .unwrap();
    assert_eq!(cfg["refresh_interval_secs"], 5);
}
#[test]
fn statusline_is_cached_only_and_creates_no_database() {
    let home = Home::new();
    let output = home
        .command()
        .args(["statusline", "--cached"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!home.0.join("cache").exists());
}
#[test]
fn missing_quota_is_null_and_nonzero_exit() {
    let home = Home::new();
    let output = home
        .command()
        .args(["status", "--provider", "grok", "--offline", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let data: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(data["providers"][0]["windows"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(data["history"]["total_tokens"].is_null());
}
#[test]
fn history_is_offline_and_reports_token_counts() {
    let home = Home::new();
    fs::create_dir_all(home.0.join(".codex/sessions")).unwrap();
    fs::write(
        home.0.join(".codex/sessions/test.jsonl"),
        include_str!("fixtures/codex.jsonl")
            .replace("2026-11-08T07:00:00Z", &chrono::Utc::now().to_rfc3339())
            .replace("2026-11-08T07:01:00Z", &chrono::Utc::now().to_rfc3339())
            .replace("2026-11-08T07:02:00Z", &chrono::Utc::now().to_rfc3339()),
    )
    .unwrap();
    let output = home
        .command()
        .args([
            "history",
            "--period",
            "30d",
            "--group-by",
            "model",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let data: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(data["history"]["rows"][0]["label"], "test-model");
    // Use a fixed event timestamp in the library tests; CLI verifies the output contract.
    assert!(data["history"]["total_tokens"].is_number());
}
#[test]
fn config_validation_preserves_previous_bytes() {
    let home = Home::new();
    assert!(home
        .command()
        .args(["config", "set", "codex", "true"])
        .output()
        .unwrap()
        .status
        .success());
    let path = home.0.join(".config/aitop/config.json");
    let before = fs::read(&path).unwrap();
    let result = home
        .command()
        .args(["config", "set", "refresh_interval", "0"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(fs::read(path).unwrap(), before);
}
#[test]
fn ingest_whitelists_fields_and_does_not_save_credentials() {
    let home = Home::new();
    let mut child = home
        .command()
        .args(["ingest", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(br#"{"secret":"NEVER_SAVE_THIS","rate_limits":{"five_hour":{"used_percentage":23,"resets_at":1999999999}}}"#).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("77% left"));
    assert!(home
        .command()
        .args(["config", "set", "claude", "true"])
        .output()
        .unwrap()
        .status
        .success());
    let cached = home
        .command()
        .args(["statusline", "--cached"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&cached.stdout).contains("77%"));
    for file in fs::read_dir(home.0.join("cache")).unwrap() {
        let data = fs::read(file.unwrap().path()).unwrap();
        assert!(!String::from_utf8_lossy(&data).contains("NEVER_SAVE_THIS"));
    }
}
