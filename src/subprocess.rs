//! Run external tools with argument arrays and capture their output.
use anyhow::{Context, Result, bail};
use std::{
    io,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command};

/// Stderr kept in error messages; longer output would exceed a protocol frame.
const MAX_DIAGNOSTIC_CHARS: usize = 8192;

/// Captured subprocess with optional input and a deadline for the entire exchange.
/// The child is killed if the deadline expires or the caller drops the future.
pub struct Run {
    command: Command,
    timeout: Option<Duration>,
    input: Option<Vec<u8>>,
}

impl Run {
    pub fn new(command: Command) -> Self {
        Self {
            command,
            timeout: None,
            input: None,
        }
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn input(mut self, input: impl Into<Vec<u8>>) -> Self {
        self.input = Some(input.into());
        self
    }

    /// Capture bytes, leaving exit-status interpretation to the caller.
    pub async fn capture(mut self) -> io::Result<Output> {
        let program = self
            .command
            .as_std()
            .get_program()
            .to_string_lossy()
            .into_owned();
        let capture = async {
            let mut child = self
                .command
                .stdin(if self.input.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;
            let stdin = child.stdin.take();
            // Drain output while writing input: either pipe may exceed its buffer.
            let write = async move {
                if let (Some(mut stdin), Some(input)) = (stdin, self.input) {
                    stdin.write_all(&input).await?;
                }
                Ok::<_, io::Error>(())
            };
            let (output, ()) = tokio::try_join!(child.wait_with_output(), write)?;
            Ok(output)
        };
        match self.timeout {
            Some(duration) => tokio::time::timeout(duration, capture).await.map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "{program} timed out after {} seconds",
                        duration.as_secs_f64()
                    ),
                )
            })?,
            None => capture.await,
        }
    }

    /// Check success without requiring stdout to be text.
    pub async fn checked(self) -> Result<Output> {
        let program = self
            .command
            .as_std()
            .get_program()
            .to_string_lossy()
            .into_owned();
        check(&program, self.capture().await)
    }

    pub async fn output(self) -> Result<String> {
        String::from_utf8(self.checked().await?.stdout).context("tool output is not UTF-8")
    }
}

/// Run to completion with no stdin and return stdout as UTF-8.
pub async fn output(command: Command) -> Result<String> {
    Run::new(command).output().await
}

pub async fn capture(command: Command) -> io::Result<Output> {
    Run::new(command).capture().await
}

/// Apply the standard tool diagnostics to a captured process result.
pub fn checked_output(program: &str, output: io::Result<Output>) -> Result<String> {
    String::from_utf8(check(program, output)?.stdout).context("tool output is not UTF-8")
}

fn check(program: &str, output: io::Result<Output>) -> Result<Output> {
    let output = output.with_context(|| {
        format!("run {program}; ensure it is installed and on the daemon's PATH")
    })?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{program} failed ({}): {}",
            output.status,
            diagnostic
                .chars()
                .take(MAX_DIAGNOSTIC_CHARS)
                .collect::<String>()
        );
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]).env_clear();
        command
    }

    #[tokio::test]
    async fn captures_literal_arguments_and_closes_stdin() {
        let mut command = shell("read value || printf '%s' \"$1\"");
        command.args(["--", "$(exit 9); literal"]);
        assert_eq!(
            Run::new(command).output().await.unwrap(),
            "$(exit 9); literal"
        );
    }

    #[tokio::test]
    async fn input_and_output_can_exceed_pipe_buffers() {
        let input = vec![b'x'; 256 * 1024];
        let output = Run::new(shell("head -c 262144 /dev/zero; cat"))
            .input(input.clone())
            .timeout(Duration::from_secs(5))
            .checked()
            .await
            .unwrap();
        assert_eq!(&output.stdout[..input.len()], vec![0; input.len()]);
        assert_eq!(&output.stdout[input.len()..], input);
    }

    #[tokio::test]
    async fn raw_status_and_non_utf8_stdout_are_preserved() {
        let output = Run::new(shell("printf '\\377'; exit 7"))
            .capture()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, [255]);
        assert!(
            Run::new(shell("printf '\\377'"))
                .output()
                .await
                .unwrap_err()
                .to_string()
                .contains("not UTF-8")
        );
    }

    #[tokio::test]
    async fn reports_spawn_failure_and_limits_unicode_diagnostics() {
        let error = Run::new(Command::new("/nonexistent-shoal-test-tool"))
            .output()
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("/nonexistent-shoal-test-tool"));
        let text = format!("{}END", "é".repeat(MAX_DIAGNOSTIC_CHARS));
        let error = Run::new(shell("cat >&2; exit 1"))
            .input(text.into_bytes())
            .output()
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .ends_with(&"é".repeat(MAX_DIAGNOSTIC_CHARS))
        );
        assert!(!error.to_string().contains("END"));
    }

    #[tokio::test]
    async fn deadline_kills_child_even_when_stdin_is_blocked() {
        let root = tempfile::tempdir().unwrap();
        let pid_file = root.path().join("pid");
        let mut command = shell("echo $$ > \"$1\"; exec sleep 30");
        command.arg("--").arg(&pid_file);
        let error = Run::new(command)
            .input(vec![0; 1024 * 1024])
            .timeout(Duration::from_millis(300))
            .capture()
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while unsafe { libc::kill(pid, 0) } == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed-out child survived");
    }
}
