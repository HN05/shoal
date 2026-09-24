mod access;
mod agent_auth;
mod ai;
mod allocation;
mod cleanup;
mod cli;
mod client;
mod commands;
mod completion;
mod config;
mod context;
mod daemon;
mod doctor;
mod env;
mod execution;
mod forge;
mod git;
mod git_profile;
mod happy;
mod hooks;
mod model;
mod notifications;
mod output;
mod paths;
mod ports;
mod process;
mod progress;
mod protocol;
mod recovery;
mod removal;
mod resources;
mod scope;
mod service;
mod shell;
mod sim;
mod state;
mod store;
mod subprocess;
#[cfg(test)]
mod test_support;
mod ui;
mod validate;
mod workspace;

use clap::Parser;
use cli::Cli;
use serde_json::json;

fn main() {
    clap_complete::CompleteEnv::with_factory(completion::command)
        .var(env::COMPLETE)
        .complete();
    run_cli();
}

#[tokio::main]
async fn run_cli() {
    let cli = Cli::parse();
    let json_output = cli.json;
    match commands::run(cli).await {
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
                    output::Palette::stderr(false).paint(output::Style::Error, "error:")
                );
            }
            std::process::exit(1);
        }
    }
}
