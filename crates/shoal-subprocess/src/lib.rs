//! Run external tools with argument arrays and capture their output.
use anyhow::{Context, Result, bail};
use std::{
    io,
    process::{Output, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};

/// Stderr kept in error messages; longer output would exceed a protocol frame.
pub const MAX_DIAGNOSTIC_CHARS: usize = 8192;

/// Arguments longer than this are cut to their end in the command an
/// observer receives.
const OBSERVED_ARGUMENT_CHARS: usize = 48;

static OBSERVER: OnceLock<fn(&str, Duration)> = OnceLock::new();

/// Report every later run to `observer` with its shortened command line and
/// duration, for profiling. The first observer set stays.
pub fn observe(observer: fn(&str, Duration)) {
    let _ = OBSERVER.set(observer);
}

/// Captured subprocess with optional input and a deadline for the entire exchange.
/// The child is killed if the deadline expires or the caller drops the future.
pub struct Run {
    command: Command,
    timeout: Option<Duration>,
    input: Option<Vec<u8>>,
    output_limit: Option<usize>,
}

impl Run {
    pub fn new(command: Command) -> Self {
        Self {
            command,
            timeout: None,
            input: None,
            output_limit: None,
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

    /// Stop and kill the child when either output pipe exceeds this byte limit.
    pub fn output_limit(mut self, bytes: usize) -> Self {
        self.output_limit = Some(bytes);
        self
    }

    /// Capture bytes, leaving exit-status interpretation to the caller.
    pub async fn capture(self) -> io::Result<Output> {
        let Some(observer) = OBSERVER.get() else {
            return self.run().await;
        };
        let command = summary(self.command.as_std());
        let started = Instant::now();
        let output = self.run().await;
        observer(&command, started.elapsed());
        output
    }

    async fn run(mut self) -> io::Result<Output> {
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
                if let (Some(mut stdin), Some(input)) = (stdin, self.input)
                    && let Err(error) = stdin.write_all(&input).await
                {
                    // A child may reject input early; retain its status and stderr.
                    if error.kind() != io::ErrorKind::BrokenPipe {
                        return Err(error);
                    }
                }
                Ok::<_, io::Error>(())
            };
            let (output, ()) = tokio::try_join!(capture_output(child, self.output_limit), write)?;
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

/// The program and its arguments, long ones cut to their end.
fn summary(command: &std::process::Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|part| {
            let part = part.to_string_lossy();
            if part.chars().count() <= OBSERVED_ARGUMENT_CHARS {
                return part.into_owned();
            }
            // The end of a path names the worktree or file.
            let skipped = part.chars().count() - (OBSERVED_ARGUMENT_CHARS - 1);
            let kept: String = part.chars().skip(skipped).collect();
            format!("…{kept}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn capture_output(mut child: Child, limit: Option<usize>) -> io::Result<Output> {
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let (status, stdout, stderr) = tokio::try_join!(
        child.wait(),
        read_output(stdout, limit),
        read_output(stderr, limit)
    )?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

async fn read_output(
    mut pipe: impl AsyncRead + Unpin,
    limit: Option<usize>,
) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Some(limit) = limit {
        pipe.take(limit.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > limit {
            return Err(io::Error::other(format!(
                "tool output exceeded {limit} bytes"
            )));
        }
    } else {
        pipe.read_to_end(&mut bytes).await?;
    }
    Ok(bytes)
}

/// Run to completion with no stdin and return stdout as UTF-8.
pub async fn output(command: Command) -> Result<String> {
    Run::new(command).output().await
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
        bail!(
            "{program} failed ({}): {}",
            output.status,
            diagnostic(&output.stderr)
        );
    }
    Ok(output)
}

/// Bound diagnostics even when callers interpret exit status themselves.
pub fn diagnostic(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .chars()
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script])
            .env_clear()
            .env("SHOAL_TEST_PROCESS", "1");
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
            .timeout(Duration::from_secs(60))
            .checked()
            .await
            .unwrap();
        assert_eq!(&output.stdout[..input.len()], vec![0; input.len()]);
        assert_eq!(&output.stdout[input.len()..], input);
    }

    #[tokio::test]
    async fn output_limits_cover_both_pipes_and_interrupt_nonterminating_children() {
        let output = Run::new(shell("printf 1234; printf 5678 >&2"))
            .output_limit(4)
            .capture()
            .await
            .unwrap();
        assert_eq!(output.stdout, b"1234");
        assert_eq!(output.stderr, b"5678");
        for redirect in ["", ">&2"] {
            let error = Run::new(shell(&format!("while :; do printf 12345 {redirect}; done")))
                .output_limit(4)
                .input(vec![0; 256 * 1024])
                .timeout(Duration::from_secs(60))
                .capture()
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains("output exceeded 4 bytes"),
                "{error}"
            );
        }
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
    async fn early_stdin_close_preserves_exit_status_and_diagnostics() {
        let output = Run::new(shell("exec 0<&-; printf 'early exit' >&2; exit 7"))
            .input(vec![0; 1024 * 1024])
            .timeout(Duration::from_secs(60))
            .capture()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stderr, b"early exit");
        let error = checked_output("test tool", Ok(output)).unwrap_err();
        assert!(error.to_string().ends_with("early exit"));
        assert_eq!(
            Run::new(shell("exec 0<&-; printf done"))
                .input(vec![0; 1024 * 1024])
                .timeout(Duration::from_secs(60))
                .output()
                .await
                .unwrap(),
            "done"
        );
    }

    #[tokio::test]
    async fn deadline_kills_child_even_when_stdin_is_blocked() {
        let root = tempfile::tempdir().unwrap();
        let pid_file = root.path().join("pid");
        let mut command = shell("echo $$ > \"$1\"; exec tail -f /dev/null");
        command.arg("--").arg(&pid_file);
        let exchange = tokio::spawn(
            Run::new(command)
                .input(vec![0; 1024 * 1024])
                .timeout(Duration::from_secs(60))
                .capture(),
        );
        let pid: i32 = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = text.trim().parse()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child did not publish readiness");
        // Advance the deadline only once the nonterminating child has started.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(60)).await;
        let error = exchange.await.unwrap().unwrap_err();
        tokio::time::resume();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        tokio::time::timeout(Duration::from_secs(60), async {
            while unsafe { libc::kill(pid, 0) } == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed-out child survived");
    }

    static OBSERVED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    #[tokio::test]
    async fn observer_receives_each_run_with_long_arguments_cut_to_their_end() {
        observe(|command, _| OBSERVED.lock().unwrap().push(command.to_owned()));
        let long = format!("{}{}", "x".repeat(10), "y".repeat(OBSERVED_ARGUMENT_CHARS));
        let mut command = shell("exit 0");
        command.args(["observed-run", &long]);
        Run::new(command).capture().await.unwrap();
        let cut = format!("…{}", "y".repeat(OBSERVED_ARGUMENT_CHARS - 1));
        let expected = format!("/bin/sh -c exit 0 observed-run {cut}");
        assert!(OBSERVED.lock().unwrap().contains(&expected));
    }
}
