use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsString,
    io::IsTerminal,
    os::unix::process::ExitStatusExt,
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::{
    net::UnixStream,
    process::{Child, Command},
    signal::unix::{SignalKind, signal},
    time::timeout,
};

use crate::{
    paths::Paths,
    protocol::{self, Body, Control, ExecutionResult, Method, Request, Response},
};

pub async fn run(paths: &Paths, workspace: String, command: Vec<OsString>) -> Result<i32> {
    ensure!(!command.is_empty(), "a command is required after --");
    let mut stream = UnixStream::connect(&paths.socket)
        .await
        .context("connect to daemon")?;
    protocol::write(
        &mut stream,
        &Request {
            protocol: protocol::VERSION,
            id: 1,
            method: Method::Execute { workspace },
        },
    )
    .await?;
    let response: Response = timeout(Duration::from_secs(5), protocol::read(&mut stream)).await??;
    ensure!(
        response.protocol == protocol::VERSION && response.id == 1,
        "daemon protocol mismatch"
    );
    let plan = match response.body {
        Body::Execution(plan) => plan,
        Body::Error { message, .. } => bail!("{message}"),
        _ => bail!("unexpected execution response"),
    };
    let result = async {
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut quit = signal(SignalKind::quit())?;
        let mut child = Command::new(&command[0]).args(&command[1..])
            .current_dir(&plan.workspace.path)
            .env("SHOAL_WORKSPACE_ID", &plan.workspace.id)
            .env("SHOAL_RUN_ID", &plan.workspace.id)
            .env("SHOAL_WORKSPACE", &plan.workspace.name)
            .env("SHOAL_STATE_DIR", &paths.state)
            .env_remove("SHOAL_SHELL_DIRECTIVE")
            .stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit())
            .process_group(0).kill_on_drop(true).spawn().context("launch workspace command")?;
        let group = ProcessGroup(child.id().context("child PID unavailable")? as i32);
        let _terminal = Terminal::give_to(group.0)?;
        group.send(libc::SIGCONT);
        let control = protocol::read::<Control>(&mut stream);
        tokio::pin!(control);
        let status = tokio::select! {
            status = child.wait() => status?,
            result = &mut control => {
                let status = stop(&mut child, &group, libc::SIGTERM).await?;
                result.context("daemon disconnected; command stopped, execution requires reconciliation")?;
                status
            }
            _ = terminate.recv() => stop(&mut child, &group, libc::SIGTERM).await?,
            _ = interrupt.recv() => stop(&mut child, &group, libc::SIGINT).await?,
            _ = quit.recv() => stop(&mut child, &group, libc::SIGQUIT).await?,
        };
        // A command owns its process group; descendants do not outlive its lease.
        drop(group);
        Ok::<i32, anyhow::Error>(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
    }.await;
    let code = result.as_ref().copied().unwrap_or(1);
    // Report only after child/process-group cleanup. A lost connection never
    // grants the daemon permission to assume processes stopped.
    let report = timeout(Duration::from_secs(5), async {
        protocol::write(&mut stream, &ExecutionResult { exit_code: code }).await?;
        loop {
            if matches!(
                protocol::read::<Control>(&mut stream).await?,
                Control::Finished
            ) {
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("execution completion acknowledgement timed out")
    .and_then(|r| r);
    if let Err(error) = report {
        eprintln!("warning: unable to report execution completion: {error:#}");
    }
    result
}

struct ProcessGroup(i32);
impl ProcessGroup {
    fn send(&self, signal: i32) {
        // SAFETY: this is the process group of the child just spawned here.
        unsafe {
            libc::kill(-self.0, signal);
        }
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.send(libc::SIGKILL);
    }
}

async fn stop(child: &mut Child, group: &ProcessGroup, signal: i32) -> Result<ExitStatus> {
    group.send(signal);
    match timeout(Duration::from_secs(2), child.wait()).await {
        Ok(status) => Ok(status?),
        Err(_) => {
            group.send(libc::SIGKILL);
            Ok(child.wait().await?)
        }
    }
}

struct Terminal {
    previous_group: i32,
    previous_handler: libc::sighandler_t,
}
impl Terminal {
    fn give_to(group: i32) -> Result<Option<Self>> {
        if !std::io::stdin().is_terminal() {
            return Ok(None);
        }
        // SAFETY: tcgetpgrp/getpgrp inspect the calling process and stdin.
        let previous_group = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        ensure!(
            previous_group == unsafe { libc::getpgrp() },
            "shoal exec must be a foreground terminal job"
        );
        // Ignore SIGTTOU only in the wrapper, after spawning the child, so it can
        // restore the terminal once its process group is in the background.
        let previous_handler = unsafe { libc::signal(libc::SIGTTOU, libc::SIG_IGN) };
        let terminal = Self {
            previous_group,
            previous_handler,
        };
        ensure!(
            unsafe { libc::tcsetpgrp(libc::STDIN_FILENO, group) } == 0,
            "cannot hand terminal to command"
        );
        Ok(Some(terminal))
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: restore the foreground group and handler captured above.
        unsafe {
            libc::tcsetpgrp(libc::STDIN_FILENO, self.previous_group);
            libc::signal(libc::SIGTTOU, self.previous_handler);
        }
    }
}
