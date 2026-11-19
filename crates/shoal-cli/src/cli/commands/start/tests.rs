use clap::Parser;

use super::*;
use crate::cli::{Cli, Command, args::create::CreateCommand};

fn start(args: &[&str]) -> StartArgs {
    let cli = Cli::try_parse_from(["shoal", "start"].iter().chain(args)).unwrap();
    let Some(Command::Create(CreateCommand::Start(start))) = cli.command else {
        panic!("expected start");
    };
    *start
}

#[tokio::test]
async fn the_setup_mode_starts_only_with_its_own_commands() {
    let home = tempfile::tempdir().unwrap();
    let ctx = Context::new(crate::paths::Paths::for_test(home.path()), true);
    for args in [&["--mode", "setup"][..], &["--mode", "setup", "--here"]] {
        let error = run(&ctx, start(args)).await.unwrap_err().to_string();
        assert!(error.contains("shoal config setup"), "{error}");
    }
}
