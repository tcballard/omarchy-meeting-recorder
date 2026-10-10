use crate::model::{truncate, ApiError};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

pub static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    TERMINATE.store(true, Ordering::SeqCst);
}

pub fn install_signals() {
    // The handler only sets a lock-free atomic. All cleanup happens in the actor.
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
}

#[derive(Clone)]
pub struct Control {
    pub cancel: Arc<AtomicBool>,
    pub shutdown: Arc<AtomicBool>,
    pub deadline: Instant,
}

impl Control {
    pub fn new(shutdown: Arc<AtomicBool>, duration: Duration) -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            shutdown,
            deadline: Instant::now() + duration,
        }
    }
    pub fn check(&self) -> Result<(), ApiError> {
        if self.cancel.load(Ordering::SeqCst)
            || self.shutdown.load(Ordering::SeqCst)
            || TERMINATE.load(Ordering::SeqCst)
        {
            return Err(ApiError::new("CANCELLED", "The operation was cancelled."));
        }
        if Instant::now() >= self.deadline {
            return Err(ApiError::new("TIMEOUT", "The local operation exceeded its time limit. Try a smaller input or a faster model."));
        }
        Ok(())
    }
}

pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub struct ManagedChild {
    pub child: Child,
}

impl ManagedChild {
    pub fn stop(&mut self, graceful: Duration) {
        terminate_group(self.child.id(), libc::SIGTERM);
        let end = Instant::now() + graceful;
        while Instant::now() < end {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                terminate_group(self.child.id(), libc::SIGKILL);
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        terminate_group(self.child.id(), libc::SIGKILL);
        let _ = self.child.wait();
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        terminate_group(self.child.id(), libc::SIGKILL);
        let _ = self.child.wait();
    }
}

pub fn terminate_group(pid: u32, signal: libc::c_int) {
    // Every managed command starts a fresh process group owned by this backend.
    unsafe {
        libc::kill(-(pid as libc::pid_t), signal);
    }
}

pub fn run(
    program: &str,
    args: &[&str],
    input: Option<&[u8]>,
    cap: usize,
    timeout: Duration,
    control: &Control,
) -> Result<Output, ApiError> {
    control.check()?;
    let mut command = Command::new(program);
    command
        .args(args)
        .process_group(0)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::new(
                "MISSING_DEPENDENCY",
                format!("Install {program} and ensure it is in PATH."),
            )
        } else {
            ApiError::new("PROCESS_START", format!("Could not start {program}: {e}"))
        }
    })?;
    let mut managed = ManagedChild { child };
    let overflow = Arc::new(AtomicBool::new(false));
    let out = managed.child.stdout.take().unwrap();
    let err = managed.child.stderr.take().unwrap();
    let out_handle = read_bounded(out, cap, overflow.clone());
    let err_handle = read_bounded(err, 64_000, overflow.clone());
    let input_handle = if let Some(data) = input {
        let data = data.to_owned();
        let mut pipe = managed.child.stdin.take().unwrap();
        Some(std::thread::spawn(move || pipe.write_all(&data)))
    } else {
        None
    };
    let end = Instant::now() + timeout;
    let status = loop {
        if let Err(e) = control.check() {
            managed.stop(Duration::ZERO);
            break Err(e);
        }
        if overflow.load(Ordering::SeqCst) {
            managed.stop(Duration::ZERO);
            break Err(ApiError::new(
                "OUTPUT_LIMIT",
                format!("{program} produced too much output."),
            ));
        }
        if Instant::now() >= end {
            managed.stop(Duration::ZERO);
            break Err(ApiError::new(
                "TIMEOUT",
                format!("{program} exceeded its time limit."),
            ));
        }
        match managed.child.try_wait() {
            Ok(Some(status)) => {
                terminate_group(managed.child.id(), libc::SIGKILL);
                break Ok(status);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                managed.stop(Duration::ZERO);
                break Err(ApiError::new(
                    "PROCESS_WAIT",
                    format!("Could not supervise {program}: {e}"),
                ));
            }
        }
    };
    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();
    if let Some(handle) = input_handle {
        let _ = handle.join();
    }
    let status = status?;
    if overflow.load(Ordering::SeqCst) {
        return Err(ApiError::new(
            "OUTPUT_LIMIT",
            format!("{program} produced too much output."),
        ));
    }
    if !status.success() {
        return Err(ApiError::new(
            "PROCESS_FAILED",
            format!(
                "{program} failed: {}",
                truncate(String::from_utf8_lossy(&stderr).trim(), 1200)
            ),
        ));
    }
    Ok(Output { stdout, stderr })
}

fn read_bounded<R: Read + Send + 'static>(
    mut reader: R,
    cap: usize,
    overflow: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut collected = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let free = cap.saturating_sub(collected.len());
                    collected.extend_from_slice(&buf[..n.min(free)]);
                    if n > free {
                        overflow.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
        collected
    })
}

pub fn available(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|dir| {
        let path = dir.join(program);
        path.is_file()
            && path
                .metadata()
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_timeout_is_bounded() {
        let c = Control::new(Arc::new(AtomicBool::new(false)), Duration::from_secs(2));
        let start = Instant::now();
        let e = run(
            "sh",
            &["-c", "sleep 10"],
            None,
            1024,
            Duration::from_millis(100),
            &c,
        )
        .err()
        .unwrap();
        assert_eq!(e.code, "TIMEOUT");
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn producer_output_is_bounded() {
        let c = Control::new(Arc::new(AtomicBool::new(false)), Duration::from_secs(2));
        let e = run(
            "sh",
            &["-c", "yes data"],
            None,
            128,
            Duration::from_secs(1),
            &c,
        )
        .err()
        .unwrap();
        assert_eq!(e.code, "OUTPUT_LIMIT");
    }
    #[test]
    fn cancellation_kills_process() {
        let c = Control::new(Arc::new(AtomicBool::new(false)), Duration::from_secs(2));
        c.cancel.store(true, Ordering::SeqCst);
        assert_eq!(
            run(
                "sh",
                &["-c", "sleep 1"],
                None,
                100,
                Duration::from_secs(1),
                &c
            )
            .err()
            .unwrap()
            .code,
            "CANCELLED"
        );
    }
}
