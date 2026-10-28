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
        let described = describe(self.command.as_std());
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
                        "{described} did not finish within {} seconds and was stopped",
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

/// The whole command line, and the directory it runs in when it sets one, so
/// an error names what hung and where.
fn describe(command: &std::process::Command) -> String {
    let line = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    match command.get_current_dir() {
        Some(dir) => format!("`{line}` in {}", dir.display()),
        None => format!("`{line}`"),
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
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("tool output exceeded {limit} bytes"),
            ));
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
    let output = match output {
        // The error names the command; it was found and ran.
        Err(error) if error.kind() == io::ErrorKind::TimedOut => return Err(error.into()),
        output => output.with_context(|| {
            format!("run {program}; ensure it is installed and on the daemon's PATH")
        })?,
    };
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
mod tests;
