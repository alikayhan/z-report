use crate::lifecycle::EvaluatorGuard;
use anyhow::{bail, Result};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Supervised {
    child: Child,
    lifeline: Option<File>,
    watchdog: i32,
}

impl Drop for Supervised {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
        drop(self.lifeline.take());
        if self.watchdog > 0 {
            unsafe {
                libc::waitpid(self.watchdog, std::ptr::null_mut(), 0);
            }
        }
    }
}

fn watch(child: Child, lock_fd: Option<i32>) -> Result<Supervised> {
    let mut process = Supervised {
        child,
        lifeline: None,
        watchdog: 0,
    };
    let mut pipe = [0; 2];
    anyhow::ensure!(
        unsafe { libc::pipe(pipe.as_mut_ptr()) } == 0,
        "Cannot create evaluator lifeline"
    );
    let reader = unsafe { File::from_raw_fd(pipe[0]) };
    process.lifeline = Some(unsafe { File::from_raw_fd(pipe[1]) });
    let group = process.child.id() as i32;
    let max_fd = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) }.clamp(256, 65536) as i32;
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // Only async-signal-safe syscalls after fork: the desktop is multithreaded.
        // A separate session lets the watcher survive loss of the entire host group.
        unsafe {
            libc::setsid();
            for fd in 0..max_fd {
                if fd != reader.as_raw_fd() && Some(fd) != lock_fd {
                    libc::close(fd);
                }
            }
            let mut byte = [0u8; 1];
            loop {
                let n = libc::read(reader.as_raw_fd(), byte.as_mut_ptr().cast(), 1);
                if n <= 0 {
                    break;
                }
            }
            libc::kill(-group, libc::SIGKILL);
            libc::_exit(0);
        }
    }
    anyhow::ensure!(pid > 0, "Cannot supervise evaluator process");
    process.watchdog = pid;
    Ok(process)
}

pub fn run(
    mut command: Command,
    dir: &Path,
    timeout: Duration,
    busy: &EvaluatorGuard,
) -> Result<(Output, Duration)> {
    anyhow::ensure!(!busy.cancelled(), "Read cancelled");
    let stdout = dir.join("stdout.json");
    let stderr = dir.join("stderr.txt");
    command
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(File::create(&stdout)?)
        .stderr(File::create(&stderr)?)
        .process_group(0);
    let child = command.spawn()?;
    let mut process = watch(child, busy.lock_fd())?;
    let began = Instant::now();
    let status = loop {
        if busy.cancelled() {
            bail!("Read cancelled");
        }
        if began.elapsed() >= timeout {
            bail!("Evaluator timed out after {} seconds", timeout.as_secs());
        }
        let watchdog =
            unsafe { libc::waitpid(process.watchdog, std::ptr::null_mut(), libc::WNOHANG) };
        if watchdog != 0 {
            process.watchdog = 0;
            bail!("Evaluator supervision stopped");
        }
        if let Some(status) = process.child.try_wait()? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    drop(process);
    Ok((
        Output {
            status,
            stdout: std::fs::read(stdout)?,
            stderr: std::fs::read(stderr)?,
        },
        began.elapsed(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timeout_stops_a_real_subprocess() {
        let dir = std::env::temp_dir().join(format!("zreport-timeout-{}", crate::engine::new_id()));
        std::fs::create_dir(&dir).unwrap();
        let lifecycle = crate::lifecycle::Lifecycle::default();
        let guard = lifecycle.begin_evaluation().unwrap();
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let began = Instant::now();
        let error = run(command, &dir, Duration::from_millis(100), &guard).unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(began.elapsed() < Duration::from_secs(3));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
