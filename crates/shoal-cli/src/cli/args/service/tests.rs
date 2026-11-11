use clap::Parser as _;

use super::*;

#[test]
fn a_config_setup_parses_back_from_its_command_line() {
    let setup = ConfigSetupArgs {
        here: true,
        agent: Some("codex".parse().unwrap()),
        model: ModelArgs {
            model: Some("gpt".into()),
            effort: Some("high".into()),
        },
        name: Some("setup".into()),
        session: Some("s1".into()),
        restore: Some(SpaceId::from("c1")),
        handoff: Some("/tmp/shoal-chat.1".into()),
        args: vec!["--search".into()],
    };
    let line = std::iter::once(OsString::from("shoal")).chain(setup.command_line());
    let cli = crate::cli::Cli::try_parse_from(line).unwrap();
    let Some(crate::cli::Command::Service(ServiceCommand::Config {
        command: ConfigCommand::Setup(parsed),
    })) = cli.command
    else {
        panic!("expected config setup");
    };
    assert_eq!(format!("{parsed:?}"), format!("{setup:?}"));
}
