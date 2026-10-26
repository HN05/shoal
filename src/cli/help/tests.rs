use clap::CommandFactory;

#[test]
fn grouped_help_shows_every_public_command_once_and_no_internal_workers() {
    let mut command = crate::cli::Cli::command();
    let help = command.render_help().to_string();
    let listed: Vec<_> = help
        .lines()
        .filter(|line| line.starts_with("  ") && !line.starts_with("  shoal "))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !name.starts_with('-'))
        .collect();
    let expected: Vec<_> = command
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
        .map(|command| command.get_name())
        .collect();
    assert_eq!(listed.len(), expected.len(), "{help}");
    for name in expected {
        assert_eq!(listed.iter().filter(|listed| **listed == name).count(), 1);
    }
    assert!(help.find("Workspaces:").unwrap() < help.find("Options:").unwrap());
}
