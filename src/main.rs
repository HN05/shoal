mod agent;
mod agent_auth;
mod ai;
mod caches;
mod cli;
mod config;
mod daemon;
mod env;
mod execution;
mod forge;
mod fsutil;
mod git;
mod git_profile;
mod happy;
mod hooks;
mod model;
mod paths;
mod process;
mod protocol;
mod removal;
mod secrets;
mod seed;
mod service;
mod shell;
mod sim;
mod state;
mod subprocess;
#[cfg(test)]
mod test_support;
mod time;
mod tools;
mod validate;

use cli::Cli;
use serde_json::json;

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
            Some(cli::Command::Daemon {
                command: cli::DaemonCommand::Run { managed: true }
            })
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
    run_cli(cli);
}

#[tokio::main]
async fn run_cli(cli: Cli) {
    let json_output = cli.json;
    match cli::commands::run(cli).await {
        Ok(code) => std::process::exit(code),
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
