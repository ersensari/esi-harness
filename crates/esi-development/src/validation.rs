use crate::{ValidationCommand, ValidationControl, ValidationTermination};
use std::path::Path;

pub(crate) struct ProcessResult {
    pub termination: ValidationTermination,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl ProcessResult {
    fn failed(termination: ValidationTermination, diagnostic: String) -> Self {
        Self {
            termination,
            exit_code: None,
            stdout: String::new(),
            stderr: diagnostic,
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }
}

pub(crate) fn execute(
    command: &ValidationCommand,
    directory: &Path,
    control: &ValidationControl,
) -> ProcessResult {
    if control.is_cancelled() {
        return ProcessResult::failed(
            ValidationTermination::Cancelled,
            "validation cancelled".into(),
        );
    }
    #[cfg(target_os = "linux")]
    {
        linux::execute(command, directory, control).unwrap_or_else(|error| {
            ProcessResult::failed(ValidationTermination::ProcessError, error.to_string())
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (command, directory);
        ProcessResult::failed(
            ValidationTermination::Unsupported,
            "bounded validation process cleanup is supported on Linux only".into(),
        )
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use nix::errno::Errno;
    use nix::fcntl::{fcntl, FcntlArg, OFlag};
    use nix::sys::signal::{killpg, Signal};
    use nix::sys::wait::{waitid, Id, WaitPidFlag, WaitStatus};
    use nix::unistd::Pid;
    use std::io::{self, Read};
    use std::os::fd::AsFd;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct OwnedChild(Child, bool);

    impl OwnedChild {
        fn finish(&mut self) -> io::Result<()> {
            self.stop_group()?;
            self.0.wait()?;
            self.1 = false;
            Ok(())
        }

        fn stop_group(&self) -> io::Result<()> {
            match killpg(Pid::from_raw(self.0.id() as i32), Signal::SIGKILL) {
                Ok(()) | Err(Errno::ESRCH) => Ok(()),
                Err(error) => Err(error.into()),
            }
        }
    }

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if !self.1 {
                return;
            }
            // Leader has not been reaped: its ID cannot refer to an unrelated group.
            let _ = self.stop_group();
            let _ = self.0.wait();
        }
    }

    struct Capture {
        bytes: Vec<u8>,
        truncated: bool,
        limit: usize,
    }

    impl Capture {
        fn read(&mut self, pipe: &mut impl Read) -> io::Result<usize> {
            let mut chunk = [0; 8192];
            match pipe.read(&mut chunk) {
                Ok(count) => {
                    let retained = count.min(self.limit.saturating_sub(self.bytes.len()));
                    self.bytes.extend_from_slice(&chunk[..retained]);
                    self.truncated |= retained < count;
                    Ok(count)
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    Ok(0)
                }
                Err(error) => Err(error),
            }
        }

        fn text(&mut self) -> String {
            let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
            self.truncated |= text.len() > self.limit;
            let mut end = text.len().min(self.limit);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text
        }
    }

    fn nonblocking(pipe: &impl AsFd) -> io::Result<()> {
        let flags = OFlag::from_bits_truncate(fcntl(pipe, FcntlArg::F_GETFL)?);
        fcntl(pipe, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        Ok(())
    }

    pub(super) fn execute(
        command: &ValidationCommand,
        directory: &Path,
        control: &ValidationControl,
    ) -> io::Result<ProcessResult> {
        let start = Instant::now();
        let mut child = OwnedChild(
            Command::new(&command.program)
                .args(&command.arguments)
                .current_dir(directory)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()?,
            true,
        );
        let mut stdout = child.0.stdout.take().expect("piped stdout");
        let mut stderr = child.0.stderr.take().expect("piped stderr");
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let mut out = Capture {
            bytes: Vec::new(),
            truncated: false,
            limit: control.output_limit_bytes,
        };
        let mut err = Capture {
            bytes: Vec::new(),
            truncated: false,
            limit: control.output_limit_bytes,
        };
        let (termination, exit_code) = loop {
            if control.is_cancelled() {
                break (ValidationTermination::Cancelled, None);
            }
            if start.elapsed() >= control.timeout {
                break (ValidationTermination::TimedOut, None);
            }
            let read = out.read(&mut stdout)? + err.read(&mut stderr)?;
            let status = waitid(
                Id::Pid(Pid::from_raw(child.0.id() as i32)),
                WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT | WaitPidFlag::WNOHANG,
            );
            match status {
                Ok(WaitStatus::Exited(_, code)) => {
                    break (ValidationTermination::Exited, Some(code))
                }
                Ok(WaitStatus::Signaled(..)) => break (ValidationTermination::Exited, None),
                Ok(_) | Err(Errno::EINTR) => {}
                Err(error) => return Err(error.into()),
            }
            if read == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        child.stop_group()?;
        // Drain only a bounded number of chunks after termination, including when
        // a detached process improperly keeps inherited pipe descriptors alive.
        for _ in 0..128 {
            if out.read(&mut stdout)? + err.read(&mut stderr)? == 0 {
                break;
            }
        }
        child.finish()?;
        Ok(ProcessResult {
            termination,
            exit_code,
            stdout: out.text(),
            stderr: err.text(),
            stdout_truncated: out.truncated,
            stderr_truncated: err.truncated,
        })
    }
}
