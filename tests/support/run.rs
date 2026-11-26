//! Commands run under a hang guard: one that does not end fails the test,
//! naming the command and what it printed, instead of holding its slot. A
//! `zellij action` whose server exits before it answers never ends, for one.
use std::{
    ffi::OsStr,
    io::{self, Read},
    process::{Child, Command, Output, Stdio},
    sync::{Arc, Mutex, PoisonError, mpsc},
    thread,
    time::{Duration, Instant},
};

/// What `command` printed once it ended; see [`output_within`].
#[track_caller]
pub fn output(command: &mut Command) -> Output {
    output_within(command, super::HANG_GUARD)
}

/// What `command` printed once it ended, failing the test at the caller's
/// line when it runs for `limit`; see [`try_output_within`].
#[track_caller]
pub fn output_within(command: &mut Command, limit: Duration) -> Output {
    match try_output_within(command, limit) {
        Ok(output) => output,
        Err(error) => panic!("{error}"),
    }
}

/// What `command` printed once it ended; see [`try_output_within`].
pub fn try_output(command: &mut Command) -> io::Result<Output> {
    try_output_within(command, super::HANG_GUARD)
}

/// What `command` printed once it ended, run as `Command::output` runs it
/// with stdin closed, or an error naming it and what it printed when it
/// runs for `limit`, when it is killed. A process it started that keeps its
/// output open keeps it running.
pub fn try_output_within(command: &mut Command, limit: Duration) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + limit;
    let (closing, closed) = mpsc::channel();
    let stdout = Pipe::drain(child.stdout.take(), closing.clone());
    let stderr = Pipe::drain(child.stderr.take(), closing);
    let status = ended(&mut child, &closed, deadline);
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "`{}` ran for over {limit:?}; stdout: {:?}; stderr: {:?}",
                described(command),
                String::from_utf8_lossy(&stdout.printed()),
                String::from_utf8_lossy(&stderr.printed()),
            ),
        ));
    };
    Ok(Output {
        status: status?,
        stdout: stdout.printed(),
        stderr: stderr.printed(),
    })
}

/// How `child` exited once both its pipes closed, as `closed` hears, or
/// `None` at `deadline`. Pipes close as the child exits, so only one whose
/// descendants keep them open, or that closes them before exiting, waits.
fn ended(
    child: &mut Child,
    closed: &mpsc::Receiver<()>,
    deadline: Instant,
) -> Option<io::Result<std::process::ExitStatus>> {
    for _ in 0..2 {
        closed
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()?;
    }
    // Exit follows within moments, so a short pause between checks.
    loop {
        if let Some(status) = child.try_wait().transpose() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn described(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a child printed on one pipe, read on a thread of its own until the
/// pipe closes.
struct Pipe {
    printed: Arc<Mutex<Vec<u8>>>,
}

impl Pipe {
    /// Read `pipe` until it closes, then say so on `closing`.
    fn drain(pipe: Option<impl Read + Send + 'static>, closing: mpsc::Sender<()>) -> Self {
        let printed = Arc::<Mutex<Vec<u8>>>::default();
        let kept = Arc::clone(&printed);
        thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let mut buffer = [0; 4096];
                loop {
                    match pipe.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => kept
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .extend_from_slice(&buffer[..count]),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            }
            let _ = closing.send(());
        });
        Self { printed }
    }

    fn printed(&self) -> Vec<u8> {
        let printed = self.printed.lock().unwrap_or_else(PoisonError::into_inner);
        printed.clone()
    }
}
