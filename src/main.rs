// First, so that every module below sees its print macros.
#[macro_use]
mod stdout;

mod agent;
mod agent_auth;
mod ai;
mod build;
mod caches;
mod cli;
mod config;
mod daemon;
mod demos;
mod env;
mod execution;
mod fsutil;
mod git;
mod git_profile;
#[cfg(feature = "happy")]
mod happy;
mod hooks;
mod model;
mod paths;
mod presets;
mod process;
mod profile;
mod protocol;
mod proxy;
mod removal;
mod repository;
mod secrets;
mod seed;
mod service;
mod sessions;
mod shell;
mod sim;
mod state;
#[cfg(test)]
mod test_support;
mod time;
mod tools;
mod transfer;
mod validate;

use cli::Cli;
use serde_json::json;
use shoal_forge as forge;
use shoal_subprocess as subprocess;

fn main() {
    clap_complete::CompleteEnv::with_factory(cli::completion::command)
        .var(env::COMPLETE)
        .complete();
    let mut cli = Cli::from_args();
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
    profile::start();
    run_cli(cli);
}

/// Tokio worker threads get more stack than its 2 MiB default: the daemon
/// polls deep async paths, such as manual cleanup's removal, whose debug
/// builds overflow that on Linux. Unused stack is reserved, not committed.
const WORKER_STACK: usize = 8 << 20;

fn run_cli(cli: Cli) {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(WORKER_STACK)
        .build()
        .expect("build the Tokio runtime")
        .block_on(run_commands(cli));
}

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
                eprintln!(
                    "{}",
                    json!({"error": {"code": "command_failed", "message": format!("{error:#}")}})
                );
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
