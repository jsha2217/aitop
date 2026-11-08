//! Bounded, cancellable subprocesses. No shell and no raw vendor stderr in errors.
use crate::models::Result;
use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // The child was created in a new process group owned by this invocation.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn run(
    command: &mut Command,
    input: &[u8],
    timeout: Duration,
    cancel: &AtomicBool,
    reply_id: Option<u64>,
) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = OwnedChild(
        command
            .spawn()
            .map_err(|_| "Provider command is not installed or cannot start")?,
    );
    if let Some(mut stdin) = child.0.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|_| "Provider command did not accept input")?;
    }
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or("Cannot read provider command")?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = stdout.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err("Cannot configure bounded provider pipe".into());
        }
    }
    let reader_stop = std::sync::Arc::new(AtomicBool::new(false));
    let stop = reader_stop.clone();
    let (tx, rx) = mpsc::sync_channel(8);
    let reader = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => {
                    let _ = tx.send(Err(()));
                    break;
                }
            }
        }
    });
    let start = Instant::now();
    let mut bytes = Vec::new();
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err("Collection cancelled".into());
        }
        if start.elapsed() >= timeout {
            break Err("Provider command timed out".into());
        }
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(Ok(chunk)) => {
                bytes.extend(chunk);
                if bytes.len() > 1024 * 1024 {
                    break Err("Provider command output exceeded 1 MiB".into());
                }
                if let Some(id) = reply_id {
                    if bytes
                        .split(|&b| b == b'\n')
                        .filter_map(|l| serde_json::from_slice::<serde_json::Value>(l).ok())
                        .any(|v| v.get("id").and_then(|v| v.as_u64()) == Some(id))
                    {
                        break Ok(bytes);
                    }
                }
            }
            Ok(Err(())) => break Err("Cannot read provider command output".into()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let status = child.0.try_wait()?;
                if status.is_some_and(|s| s.success()) {
                    break Ok(bytes);
                }
                if status.is_some() {
                    break Err("Provider command failed; run its CLI to check sign-in".into());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    reader_stop.store(true, Ordering::Relaxed);
    drop(child);
    drop(rx);
    #[cfg(unix)]
    let _ = reader.join();
    #[cfg(not(unix))]
    drop(reader);
    result
}
