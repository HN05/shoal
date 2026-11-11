//! The `shoal` command: the CLI and the daemon, over [`shoal_core`] and
//! [`shoal_client`].

// Every module sees the shared crate's print macros.
#[macro_use]
extern crate shoal_core;

mod cli;
mod daemon;
mod seed;

#[cfg(test)]
use shoal_core::test_support;
use shoal_core::{
    agent_git, agents, build, caches, config_file, daemon_handoff, daemon_owner, env, execution,
    fsutil, git, git_profile, hooks, installed, paths, presets, process, proxy, push_gate, refusal,
    releases, removal, repository, secrets, service, sessions, shell, start_plan, stdout, terminal,
    tmp_dirs, tools, transfer, workers,
};

use cli::Cli;
use shoal_client as client;
use shoal_client::time;
use shoal_client::{agent, ai, config, demos, model, profile, protocol, reviews, state, validate};
use shoal_core::WORKER_STACK;
use shoal_forge as forge;
use shoal_subprocess as subprocess;

fn main() {
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
        let releases = paths::Paths::new(cli.state_dir.clone())
            .ok()
            .map(|paths| releases::Releases::new(paths.releases()));
        // SAFETY: no other threads exist before the runtime starts, and the
        // startup variables removed below still reach the replacement.
        unsafe { process::responsibility::claim(releases) };
    }
    cli.daemon_handoff = daemon_handoff::Handoff::take().unwrap_or_else(|error| {
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

#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
