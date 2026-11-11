//! Commands run under a hang guard: one that does not end fails the test,
//! naming the command and what it printed, instead of holding its slot. A
//! `zellij action` whose server exits before it answers never ends, for one.
use std::{
    ffi::OsStr,
    io::{self, Read},
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex, PoisonError},
    thread,
    time::Duration,
};

/// What `command` printed once it ended; see [`output_within`].
pub fn output(command: &mut Command) -> Output {
    output_within(command, super::HANG_GUARD)
}

/// What `command` printed once it ended, failing the test when it runs for
/// `limit`; see [`try_output_within`].
pub fn output_within(command: &mut Command, limit: Duration) -> Output {
    try_output_within(command, limit).unwrap_or_else(|error| panic!("{error}"))
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
    let stdout = Pipe::drain(child.stdout.take());
    let stderr = Pipe::drain(child.stderr.take());
    let status = super::wait::try_wait_within(limit, || {
        let status = child.try_wait().transpose()?;
        (stdout.closed() && stderr.closed()).then_some(status)
    });
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

/// `command`'s program and arguments, as a shell would show them.
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
    reader: thread::JoinHandle<()>,
}

impl Pipe {
    fn drain(pipe: Option<impl Read + Send + 'static>) -> Self {
        let printed = Arc::<Mutex<Vec<u8>>>::default();
        let kept = Arc::clone(&printed);
        let reader = thread::spawn(move || {
            let Some(mut pipe) = pipe else {
                return;
            };
            let mut buffer = [0; 4096];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(count) => kept
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .extend_from_slice(&buffer[..count]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => return,
                }
            }
        });
        Self { printed, reader }
    }

    fn closed(&self) -> bool {
        self.reader.is_finished()
    }

    fn printed(&self) -> Vec<u8> {
        let printed = self.printed.lock().unwrap_or_else(PoisonError::into_inner);
        printed.clone()
    }
}
