#![cfg(unix)]
use aitop::process;
use std::{
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[test]
fn command_timeout_kills_and_reaps_owned_child() {
    let start = Instant::now();
    let result = process::run(
        Command::new("/bin/sh").args(["-c", "exec sleep 30"]),
        &[],
        Duration::from_millis(80),
        &AtomicBool::new(false),
        None,
    );
    assert!(result.is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
}
#[test]
fn rpc_reply_finishes_without_waiting_for_server_exit() {
    let start = Instant::now();
    let result = process::run(
        Command::new("/bin/sh").args(["-c", "printf '{\"id\":3,\"result\":{}}\\n'; sleep 30"]),
        &[],
        Duration::from_secs(2),
        &AtomicBool::new(false),
        Some(3),
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&result).contains("result"));
    assert!(start.elapsed() < Duration::from_secs(2));
}
#[test]
fn command_output_is_bounded() {
    let result = process::run(
        Command::new("/bin/sh").args(["-c", "head -c 1048577 /dev/zero"]),
        &[],
        Duration::from_secs(2),
        &AtomicBool::new(false),
        None,
    );
    assert!(result.unwrap_err().to_string().contains("1 MiB"));
}
#[test]
fn command_stderr_never_appears_in_error() {
    let result = process::run(
        Command::new("/bin/sh").args(["-c", "echo PRIVATE_TOKEN >&2; exit 1"]),
        &[],
        Duration::from_secs(2),
        &AtomicBool::new(false),
        None,
    );
    assert!(!result.unwrap_err().to_string().contains("PRIVATE_TOKEN"));
}
#[test]
fn cancellation_stops_process() {
    let result = process::run(
        Command::new("/bin/sh").args(["-c", "exec sleep 30"]),
        &[],
        Duration::from_secs(2),
        &AtomicBool::new(true),
        None,
    );
    assert!(result.is_err());
}
