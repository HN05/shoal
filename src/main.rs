mod cleanup;
mod cli;
mod client;
mod commands;
mod completion;
mod config;
mod context;
mod daemon;
mod default_branch;
mod diff;
mod env;
mod execution;
mod execution_processes;
mod existing_branch;
mod forge;
mod git;
mod git_profile;
mod happy;
mod hooks;
mod merge;
mod model;
mod named_commands;
mod notifications;
mod output;
mod paths;
mod ports;
mod pr;
mod process_identity;
mod processes;
mod protocol;
mod recovery;
mod removal;
mod repo_config;
mod repo_git;
mod repository;
mod resources;
mod scope;
mod service;
mod shell;
mod sim_audit;
mod simctl;
mod simulators;
mod state;
mod store;
mod subprocess;
mod templates;
mod ui;
mod validate;
mod workspace;
mod worktrunk;

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
