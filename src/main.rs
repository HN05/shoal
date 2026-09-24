mod agent_auth;
mod ai;
mod cli;
mod config;
mod daemon;
mod env;
mod execution;
mod forge;
mod git;
mod git_profile;
mod happy;
mod hooks;
mod model;
mod paths;
mod process;
mod protocol;
mod removal;
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

use clap::Parser;
use cli::Cli;
use serde_json::json;

fn main() {
    clap_complete::CompleteEnv::with_factory(cli::completion::command)
        .var(env::COMPLETE)
        .complete();
    run_cli();
}

#[tokio::main]
async fn run_cli() {
    let cli = Cli::parse();
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
