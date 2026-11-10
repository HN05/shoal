use super::*;
use crate::{
    agent::Agent,
    cli::args::{create::CreateCommand, items::ItemsCommand},
    model::Relation,
};
use std::collections::{BTreeMap, BTreeSet};

/// Whether `args` resolve to JSON output under the `SHOAL_OUTPUT` `setting`.
fn resolves_json(args: &[&str], setting: Option<&str>) -> anyhow::Result<bool> {
    let mut cli = Cli::try_parse_from(["shoal"].iter().chain(args)).unwrap();
    cli.resolve_output(setting.map(std::ffi::OsStr::new))?;
    Ok(cli.json)
}

#[test]
fn the_command_line_selects_output_before_the_environment() {
    for setting in [None, Some(""), Some("text")] {
        assert!(!resolves_json(&["status"], setting).unwrap());
    }
    assert!(resolves_json(&["status"], Some("json")).unwrap());
    for setting in [None, Some("text"), Some("json")] {
        assert!(resolves_json(&["--json", "status"], setting).unwrap());
        assert!(resolves_json(&["status", "--json"], setting).unwrap());
        assert!(!resolves_json(&["status", "--text"], setting).unwrap());
    }
    assert!(resolves_json(&["--json", "status", "--text"], None).is_err());
    assert!(resolves_json(&["--text", "status", "--json"], None).is_err());
}

#[test]
fn an_invalid_output_setting_fails_unless_the_command_line_selects_one() {
    assert!(resolves_json(&["status"], Some("JSON")).is_err());
    assert!(resolves_json(&["--json", "status"], Some("yaml")).unwrap());
    assert!(!resolves_json(&["--text", "status"], Some("yaml")).unwrap());
}

/// Builds each command's deferred arguments and checks that its
/// description stays the one its parent lists.
fn assert_descriptions_survive_deferral(command: &clap::Command) {
    for listed in command.get_subcommands() {
        let mut built = listed.clone();
        // Builds this command alone, leaving its subcommands deferred.
        let _ = built.render_usage();
        let name = built.get_name();
        assert_eq!(built.get_about(), listed.get_about(), "{name}");
        assert_eq!(built.get_long_about(), listed.get_long_about(), "{name}");
        assert_descriptions_survive_deferral(&built);
    }
}

#[test]
fn deferred_arguments_keep_each_command_description() {
    let mut root = <Cli as clap::CommandFactory>::command();
    let _ = root.render_usage();
    assert_descriptions_survive_deferral(&root);
}

#[test]
fn agent_cli_and_config_spellings_remain_compatible() {
    for spelling in [
        "codex",
        "claude",
        "happy-claude",
        "happy-codex",
        "my-agent_2",
        "t3",
    ] {
        let expected: Agent = spelling.parse().unwrap();
        assert_eq!(expected.name(), spelling);
        let parsed = Cli::try_parse_from(["shoal", "start", "--agent", spelling]).unwrap();
        let Some(Command::Create(CreateCommand::Start(start))) = parsed.command else {
            panic!("wrong command")
        };
        let agent = start.agent;
        assert_eq!(agent, Some(expected.clone()));
        let text = format!("agent.default = {spelling:?}");
        let global = crate::config::Config::from_toml(&text).unwrap();
        let repo: crate::config::repo::RepoConfig = toml::from_str(&text).unwrap();
        assert_eq!(global.repository.agent.default, agent);
        assert_eq!(repo.agent.default, agent);
        assert_eq!(serde_json::to_value(&expected).unwrap(), spelling);
        assert_eq!(
            serde_json::from_value::<Agent>(spelling.into()).unwrap(),
            expected
        );
    }
    for invalid in ["Codex", "happy-Codex", "", "two words", "-agent"] {
        assert!(Cli::try_parse_from(["shoal", "start", "--agent", invalid]).is_err());
        let text = format!("agent.default = {invalid:?}");
        assert!(crate::config::Config::from_toml(&text).is_err());
        assert!(toml::from_str::<crate::config::repo::RepoConfig>(&text).is_err());
        let recorded = serde_json::from_value::<Agent>(invalid.into()).unwrap();
        assert_eq!(recorded.name(), invalid);
    }
}

#[test]
fn link_arguments_end_with_a_relation_and_accept_hash_numbers() {
    let items = |args: &[&str]| {
        let parsed = Cli::try_parse_from(args).unwrap();
        let Some(Command::Items(ItemsCommand::Link { items })) = parsed.command else {
            panic!("wrong command")
        };
        let (items, relation) = items.split().unwrap();
        (items.kind_or_url, items.item, relation)
    };
    assert_eq!(
        items(&["shoal", "link", "#42", "assigned"]),
        (Some("42".into()), None, Relation::Assigned)
    );
    assert_eq!(
        items(&["shoal", "link", "pr", "#42", "related"]),
        (Some("pr".into()), Some("42".into()), Relation::Related)
    );
    assert_eq!(
        items(&["shoal", "link", "opened"]),
        (None, None, Relation::Opened)
    );
    for unchanged in [
        "#",
        "#x",
        "##42",
        "https://forge.example/team/repo/issues/1",
    ] {
        assert_eq!(
            items(&["shoal", "link", unchanged, "assigned"]),
            (Some(unchanged.into()), None, Relation::Assigned)
        );
    }
    assert!(Cli::try_parse_from(["shoal", "link"]).is_err());
    assert!(Cli::try_parse_from(["shoal", "link", "pr", "1", "2", "assigned"]).is_err());
    let Some(Command::Items(ItemsCommand::Unlink { items })) =
        Cli::try_parse_from(["shoal", "unlink", "42"])
            .unwrap()
            .command
    else {
        panic!("wrong command")
    };
    assert!(items.split().is_err());
}

#[test]
fn done_remains_available_to_agents() {
    for args in [vec!["shoal", "done"], vec!["shoal", "done", "--cleanup"]] {
        let command = Cli::try_parse_from(args).unwrap().command.unwrap();
        assert!(!command.is_administrative());
    }
}

#[test]
fn config_show_and_reference_are_available_to_workspace_processes() {
    let show = Cli::try_parse_from(["shoal", "config", "show", "workspace"]).unwrap();
    assert!(!show.command.unwrap().is_administrative());
    let reference = Cli::try_parse_from(["shoal", "config", "reference"]).unwrap();
    assert!(!reference.command.unwrap().is_administrative());
    let reset = Cli::try_parse_from(["shoal", "config", "reset"]).unwrap();
    assert!(reset.command.unwrap().is_administrative());
}

#[test]
fn only_reloading_the_daemon_is_available_to_workspace_processes() {
    let reload = Cli::try_parse_from(["shoal", "daemon", "reload"]).unwrap();
    assert!(!reload.command.unwrap().is_administrative());
    for verb in ["start", "stop", "restart"] {
        let command = Cli::try_parse_from(["shoal", "daemon", verb]).unwrap();
        assert!(command.command.unwrap().is_administrative(), "{verb}");
    }
}

#[test]
fn only_the_user_sets_up_config() {
    let setup = Cli::try_parse_from(["shoal", "repo", "setup", "app"]).unwrap();
    assert!(setup.command.unwrap().is_administrative());
    let global = Cli::try_parse_from(["shoal", "config", "setup"]).unwrap();
    assert!(global.command.unwrap().is_administrative());
}

#[test]
fn skipping_setup_is_administrative() {
    let skip = Cli::try_parse_from(["shoal", "setup", "--skip"]).unwrap();
    assert!(skip.command.unwrap().is_administrative());
    let retry = Cli::try_parse_from(["shoal", "setup"]).unwrap();
    assert!(!retry.command.unwrap().is_administrative());
}

#[test]
fn cleanup_commands_are_administrative() {
    for args in [
        vec!["shoal", "cleanup"],
        vec!["shoal", "cleanup", "--dry-run"],
        vec!["shoal", "cleanup", "spaces", "--repo", "project"],
        vec!["shoal", "cleanup", "branches", "--all", "--yes"],
        vec!["shoal", "cleanup", "caches", "--dry-run"],
    ] {
        let command = Cli::try_parse_from(args).unwrap().command.unwrap();
        assert!(command.is_administrative());
    }
}

#[test]
fn registering_a_contribution_is_administrative() {
    let args = ["shoal", "contribute", "https://github.com/owner/repo"];
    let command = Cli::try_parse_from(args).unwrap().command.unwrap();
    assert!(command.is_administrative());
}

#[test]
fn update_is_administrative() {
    let update = Cli::try_parse_from(["shoal", "update"]).unwrap();
    assert!(update.command.unwrap().is_administrative());
}

#[test]
fn removed_built_ins_remain_available_as_configured_commands() {
    // Removed built-ins remain available as user-configured command names.
    for name in ["approve", "deny", "escalate", "answer"] {
        assert!(matches!(
            Cli::try_parse_from(["shoal", name]).unwrap().command,
            Some(Command::Custom(_))
        ));
    }
}

/// Each built-in command below `command`, with its deferred arguments built,
/// and its path below `shoal`.
fn built_in_commands(
    command: &clap::Command,
    path: &[String],
) -> Vec<(Vec<String>, clap::Command)> {
    let mut commands = Vec::new();
    for listed in command.get_subcommands() {
        let mut built = listed.clone();
        let _ = built.render_usage();
        let mut below = path.to_vec();
        below.push(built.get_name().to_owned());
        commands.extend(built_in_commands(&built, &below));
        commands.push((below, built));
    }
    commands
}

#[test]
fn unknown_input_fails_at_every_built_in_level() {
    use clap::error::ErrorKind;

    let mut root = <Cli as clap::CommandFactory>::command();
    let _ = root.render_usage();
    let mut commands = built_in_commands(&root, &[]);
    commands.push((Vec::new(), root));
    let mut failures = Vec::new();
    for (path, command) in &commands {
        let mut expected = vec![
            ("--bogus-flag", ErrorKind::UnknownArgument),
            ("-h", ErrorKind::DisplayHelp),
        ];
        // A word a positional argument takes is a value, not a subcommand;
        // the top level runs it as a configured command.
        if command.has_subcommands()
            && command.get_positionals().next().is_none()
            && !path.is_empty()
        {
            expected.push(("bogus-subcommand", ErrorKind::InvalidSubcommand));
        }
        for (word, kind) in expected {
            let words = std::iter::once("shoal")
                .chain(path.iter().map(String::as_str))
                .chain([word]);
            let result = <Cli as clap::CommandFactory>::command().try_get_matches_from(words);
            if result.as_ref().err().map(clap::Error::kind) != Some(kind) {
                failures.push(format!("shoal {} {word}", path.join(" ")));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn only_wrappers_that_never_replace_themselves_claim_privacy_grants() {
    let wraps = |args: &[&str]| {
        let cli = Cli::try_parse_from(["shoal", "internal"].iter().chain(args)).unwrap();
        cli.command.as_ref().is_some_and(Command::wraps_execution)
    };
    assert!(wraps(&["add", "--host", "zmx", "{}"]));
    assert!(wraps(&[
        "session", "w", "claude", "--host", "zmx", "--", "claude"
    ]));
    assert!(wraps(&["detached", "w", "--log", "l", "--", "claude"]));
    assert!(wraps(&["demo", "w", "--launch", "{}", "--", "npm"]));
    // `launch` and `secret-tool` replace themselves with the program they
    // start; `queue-start` ends once the agent it starts runs elsewhere.
    assert!(!wraps(&["launch", "file"]));
    assert!(!wraps(&["secret-tool", "launchers", "aws", "--", "s3"]));
    assert!(!wraps(&["queue-start", "entry"]));
    // `take-over` continues a wrapper in its process, which keeps the
    // responsibility it had.
    assert!(!wraps(&[
        "take-over",
        "w",
        "--agent",
        "claude",
        "--connection",
        "3"
    ]));
    assert!(
        !Cli::try_parse_from(["shoal", "status"])
            .unwrap()
            .command
            .as_ref()
            .is_some_and(Command::wraps_execution)
    );
}

#[test]
fn ai_tool_validation_knows_every_built_in_command() {
    // Building the CLI takes more stack in a debug build than a test thread has.
    let names = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            <Cli as clap::CommandFactory>::command()
                .get_subcommands()
                .map(|command| command.get_name().to_owned())
                .collect::<std::collections::BTreeSet<_>>()
        })
        .expect("spawn a thread to build the CLI")
        .join()
        .expect("building the CLI does not panic");
    let listed = crate::ai::validate::BUILT_IN_COMMANDS
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(names, listed);
}

#[test]
fn start_takes_an_optional_in_and_issue_and_refuses_flags_its_space_cannot_use() {
    let start = |args: &[&str]| {
        let cli = Cli::try_parse_from(["shoal", "start"].iter().chain(args))?;
        let Some(Command::Create(CreateCommand::Start(start))) = cli.command else {
            panic!("expected start");
        };
        Ok::<_, clap::Error>(*start)
    };
    // Without a value, --issue in plan mode offers the issues needing a plan.
    let plan = start(&["--mode", "plan", "--issue"]).unwrap();
    assert_eq!(plan.issue, Some(None));
    let inside = start(&["--in", "--mode", "chat"]).unwrap();
    assert_eq!(inside.in_workspace, Some(None));
    // A positional issue reaches the start, which refuses a repository.
    let on_issue = start(&["37", "--in", "worker"]).unwrap();
    assert_eq!(on_issue.repository.as_deref(), Some("37"));
    assert_eq!(on_issue.in_workspace, Some(Some("worker".into())));
    // A chat has no worktree to create, and a workspace no chat name.
    for refused in [
        &["--chat", "--worktree"][..],
        &["--chat", "repo", "topic"],
        &["--chat", "--now"],
        &["--chat", "--fork"],
        &["--in", "worker", "--base", "main"],
        &["--worktree", "--name", "talk"],
        &["--restore", "c1", "--name", "talk"],
    ] {
        assert!(start(refused).is_err(), "{refused:?}");
    }
}

#[test]
fn a_kept_export_narrows_to_a_checkpoint_of_one_space() {
    let parse = |args: &[&str]| Cli::try_parse_from(["shoal", "move", "export"].iter().chain(args));
    assert!(parse(&["topic", "--keep", "--checkpoint", "3", "-o", "f"]).is_ok());
    assert!(parse(&["topic", "--checkpoint", "3", "-o", "f"]).is_err());
    let all = ["--repo", "app", "--all", "--keep", "-o", "d"];
    assert!(parse(&all).is_ok());
    assert!(parse(&[&all[..], &["--checkpoint", "3"]].concat()).is_err());
}

#[test]
fn each_short_flag_stands_for_one_long_flag_everywhere() {
    let mut root = <Cli as clap::CommandFactory>::command();
    // Checks every command, deferred arguments included: two flags of one
    // command never share a letter.
    root.clone().debug_assert();
    let _ = root.render_usage();
    let mut commands = built_in_commands(&root, &[]);
    commands.push((Vec::new(), root));
    // Shoal runs the hidden commands and flags; people do not type them.
    let hidden: BTreeSet<_> = (commands.iter())
        .filter(|(_, command)| command.is_hide_set())
        .map(|(path, _)| path.clone())
        .collect();
    let mut longs_of = BTreeMap::<char, BTreeSet<&str>>::new();
    let mut shorts_of = BTreeMap::<&str, BTreeSet<Option<char>>>::new();
    for (path, command) in &commands {
        let typed = !(1..=path.len()).any(|end| hidden.contains(&path[..end]));
        for arg in command.get_arguments() {
            let Some(long) = arg.get_long() else { continue };
            if let Some(short) = arg.get_short() {
                longs_of.entry(short).or_default().insert(long);
            }
            if typed && !arg.is_hide_set() {
                shorts_of.entry(long).or_default().insert(arg.get_short());
            }
        }
    }
    let shared: Vec<_> = longs_of
        .iter()
        .filter(|(_, longs)| longs.len() > 1)
        .collect();
    assert!(
        shared.is_empty(),
        "letters with several long flags: {shared:?}"
    );
    let partial: Vec<_> = (shorts_of.iter())
        .filter(|(_, shorts)| shorts.len() > 1)
        .collect();
    assert!(
        partial.is_empty(),
        "flags without their letter in some commands: {partial:?}"
    );
}
