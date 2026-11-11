//! The `shoal` process: parses its command line, prepares the process before
//! any thread starts and runs the command.
use futures_util::future::LocalBoxFuture;

use crate::cli::{self, Cli};
use crate::{
    WORKER_STACK, client, daemon_handoff::Handoff, env, paths::Paths, process, profile, releases,
    service, stdout,
};

/// Runs the daemon in this process, given its state, whether its service
/// manager started it and the handoff from the daemon it replaces. The
/// `shoal` command, which builds the daemon, passes it to [`run`].
pub type RunDaemon =
    fn(Paths, bool, Option<Handoff>) -> LocalBoxFuture<'static, anyhow::Result<()>>;

/// The daemon `shoal daemon run` runs in this process.
pub(crate) struct Daemon {
    pub(crate) run: RunDaemon,
    pub(crate) handoff: Option<Handoff>,
}

/// Run the command this process was started with, and exit.
pub fn run(daemon: RunDaemon) {
    clap_complete::CompleteEnv::with_factory(cli::completion::command)
        .var(env::COMPLETE)
        .complete();
    let mut cli = Cli::from_args();
    // SAFETY: no other threads exist before the runtime starts.
    unsafe { releases::take_inherited() };
    if cli
        .command
        .as_ref()
        .is_some_and(cli::Command::wraps_execution)
    {
        let releases = Paths::new(cli.state_dir.clone())
            .ok()
            .map(|paths| releases::Releases::new(paths.releases()));
        // SAFETY: no other threads exist before the runtime starts, and the
        // startup variables removed below still reach the replacement.
        unsafe { process::responsibility::claim(releases) };
    }
    cli.daemon_handoff = Handoff::take().unwrap_or_else(|error| {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    });
    if cli.daemon_handoff.is_some()
        && !matches!(
            cli.command,
            Some(cli::Command::Service(
                cli::args::service::ServiceCommand::Daemon {
                    command: cli::args::service::DaemonCommand::Run { managed: true }
                }
            ))
        )
    {
        eprintln!("error: daemon handoff requires managed daemon run");
        std::process::exit(1);
    }
    // SAFETY: no other threads exist before the runtime starts. The add plan is
    // parsed above and must not reach the worker's children.
    unsafe { std::env::remove_var(env::ADD_PLAN) };
    // SAFETY: as above; the queue entry goes to this process's own launch only.
    unsafe { env::take_queue_entry() };
    // SAFETY: as above; only this process's own launch ends its session.
    unsafe { env::take_session_end() };
    // SAFETY: as above; only the chat this process starts was shown.
    unsafe { env::take_shown_chat() };
    // SAFETY: as above; only the chat this process starts runs in the place.
    unsafe { env::take_opened_chat_place() };
    profile::start();
    client::connection::supervise(&service::Command);
    run_cli(cli, daemon);
}

fn run_cli(cli: Cli, daemon: RunDaemon) {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(WORKER_STACK)
        .build()
        .expect("build the Tokio runtime")
        .block_on(run_commands(cli, daemon));
}

async fn run_commands(cli: Cli, daemon: RunDaemon) {
    let json_output = cli.json;
    // Output without a final newline is still buffered, and the flush on exit
    // ignores a closed reader, so flush it here where that reaches the error.
    let result = cli::commands::run(cli, daemon)
        .await
        .and_then(|code| stdout::flush().map(|()| code));
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) if error.downcast_ref::<cli::ui::SelectionCanceled>().is_some() => {
            std::process::exit(130)
        }
        Err(error) if error.downcast_ref::<stdout::StdoutClosed>().is_some() => {
            std::process::exit(stdout::CLOSED_STATUS)
        }
        Err(error) => {
            if json_output {
                let printed = client::output::json(&cli::output::json_error(&error));
                eprintln!("{}", printed.unwrap_or_default());
            } else {
                eprintln!(
                    "{} {error:#}",
                    cli::output::Palette::stderr(false).paint(cli::output::Style::Error, "error:")
                );
            }
            std::process::exit(1);
        }
    }
}
