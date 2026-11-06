//! Shoal's crate. The `shoal` binary runs [`main`]; everything else stays
//! private to the crate.

// First, so that every module below sees its print macros.
#[macro_use]
#[cfg(feature = "cli")]
mod stdout;

pub mod client;

#[cfg(feature = "cli")]
mod agent_git;
#[cfg(feature = "cli")]
mod agents;
#[cfg(feature = "cli")]
mod build;
#[cfg(feature = "cli")]
mod caches;
#[cfg(feature = "cli")]
mod cli;
#[cfg(feature = "cli")]
mod config_file;
#[cfg(feature = "cli")]
mod daemon;
#[cfg(feature = "cli")]
mod env;
#[cfg(feature = "cli")]
mod execution;
#[cfg(feature = "cli")]
mod fsutil;
#[cfg(feature = "cli")]
mod git;
#[cfg(feature = "cli")]
mod git_profile;
#[cfg(feature = "happy")]
mod happy;
#[cfg(feature = "cli")]
mod hooks;
#[cfg(feature = "cli")]
mod installed;
#[cfg(feature = "cli")]
mod paths;
#[cfg(feature = "cli")]
mod presets;
#[cfg(feature = "cli")]
mod process;
#[cfg(feature = "cli")]
mod proxy;
#[cfg(feature = "cli")]
mod push_gate;
#[cfg(feature = "cli")]
mod refusal;
#[cfg(feature = "cli")]
mod removal;
#[cfg(feature = "cli")]
mod repository;
#[cfg(feature = "cli")]
mod secrets;
#[cfg(feature = "cli")]
mod seed;
#[cfg(feature = "cli")]
mod service;
#[cfg(feature = "cli")]
mod sessions;
#[cfg(feature = "cli")]
mod shell;
#[cfg(feature = "cli")]
mod terminal;
#[cfg(all(test, feature = "cli"))]
mod test_support;
#[cfg(feature = "cli")]
mod tmp_dirs;
#[cfg(feature = "cli")]
mod tools;
#[cfg(feature = "cli")]
mod transfer;
#[cfg(feature = "cli")]
mod workers;

#[cfg(feature = "cli")]
use cli::Cli;
#[cfg(feature = "cli")]
use client::time;
use client::{agent, ai, config, demos, model, profile, protocol, reviews, state, validate};
use shoal_forge as forge;
#[cfg(feature = "cli")]
use shoal_subprocess as subprocess;

/// The `shoal` command.
#[cfg(feature = "cli")]
#[doc(hidden)]
pub fn main() {
    clap_complete::CompleteEnv::with_factory(cli::completion::command)
        .var(env::COMPLETE)
        .complete();
    let mut cli = Cli::from_args();
    if cli
        .command
        .as_ref()
        .is_some_and(cli::Command::wraps_execution)
    {
        // SAFETY: no other threads exist before the runtime starts, and the
        // startup variables removed below still reach the replacement.
        unsafe { process::responsibility::claim() };
    }
    cli.daemon_handoff = daemon::handoff::Handoff::take().unwrap_or_else(|error| {
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
    profile::start();
    client::connection::supervise(&service::Command);
    run_cli(cli);
}

/// Tokio worker threads get more stack than its 2 MiB default: the daemon
/// polls deep async paths, such as manual cleanup's removal, whose debug
/// builds overflow that on Linux. Unused stack is reserved, not committed.
#[cfg(feature = "cli")]
pub(crate) const WORKER_STACK: usize = 8 << 20;

#[cfg(feature = "cli")]
fn run_cli(cli: Cli) {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(WORKER_STACK)
        .build()
        .expect("build the Tokio runtime")
        .block_on(run_commands(cli));
}

#[cfg(feature = "cli")]
async fn run_commands(cli: Cli) {
    let json_output = cli.json;
    // Output without a final newline is still buffered, and the flush on exit
    // ignores a closed reader, so flush it here where that reaches the error.
    let result = cli::commands::run(cli)
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
