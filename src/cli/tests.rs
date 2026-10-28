use clap::ValueEnum;

use super::*;
use crate::{
    agent::Agent,
    cli::args::{
        AgentParser, ConfirmationArgs,
        agents::AgentsCommand,
        changes::ChangesCommand,
        coordination::CoordinationCommand,
        create::CreateCommand,
        items::ItemsCommand,
        lifecycle::LifecycleCommand,
        repository::{RepoCommand, RepositoryCommand},
        service::{ConfigCommand, ServiceCommand},
        workspace::WorkspaceCommand,
    },
    model::Relation,
};

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
    let error = resolves_json(&["status"], Some("JSON")).unwrap_err();
    assert_eq!(
        error.to_string(),
        "SHOAL_OUTPUT must be `json` or `text`, not \"JSON\""
    );
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
        let parsed = Cli::try_parse_from(["shoal", "add", "--agent", spelling]).unwrap();
        let Some(Command::Create(CreateCommand::Add { agent, .. })) = parsed.command else {
            panic!("wrong command")
        };
        assert_eq!(agent, Some(expected.clone()));
        let text = format!("default_agent = {spelling:?}");
        let global: crate::config::Config = toml::from_str(&text).unwrap();
        let repo: crate::config::repo::RepoConfig = toml::from_str(&text).unwrap();
        assert_eq!(global.default_agent, agent);
        assert_eq!(repo.default_agent, agent);
        assert_eq!(serde_json::to_value(&expected).unwrap(), spelling);
        assert_eq!(
            serde_json::from_value::<Agent>(spelling.into()).unwrap(),
            expected
        );
    }
    for invalid in ["Codex", "happy-Codex", "", "two words", "-agent"] {
        assert!(Cli::try_parse_from(["shoal", "add", "--agent", invalid]).is_err());
        let text = format!("default_agent = {invalid:?}");
        assert!(toml::from_str::<crate::config::Config>(&text).is_err());
        assert!(toml::from_str::<crate::config::repo::RepoConfig>(&text).is_err());
        assert!(serde_json::from_value::<Agent>(invalid.into()).is_err());
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
fn agent_parsers_preserve_completion_order() {
    use clap::builder::TypedValueParser;
    let names: Vec<_> = AgentParser
        .possible_values()
        .unwrap()
        .map(|value| value.get_name().to_owned())
        .collect();
    assert_eq!(names[..5], ["codex", "claude", "opencode", "pi", "grok"]);
    #[cfg(feature = "happy")]
    assert_eq!(names[5..], ["happy-claude", "happy-codex"]);
}

#[cfg(feature = "happy")]
#[test]
fn happy_takes_the_agents_happy_runs() {
    let names: Vec<_> = crate::happy::Agent::value_variants()
        .iter()
        .map(|agent| agent.to_possible_value().unwrap().get_name().to_owned())
        .collect();
    assert_eq!(names, ["claude", "codex"]);
    for (name, expected) in [
        ("claude", crate::happy::Agent::Claude),
        ("codex", crate::happy::Agent::Codex),
    ] {
        let parsed = Cli::try_parse_from(["shoal", "happy", name]).unwrap();
        assert!(
            matches!(parsed.command, Some(Command::Agents(AgentsCommand::Happy { agent, .. })) if agent == expected)
        );
    }
    for invalid in ["Claude", "Codex", "happy-codex", "custom"] {
        assert!(Cli::try_parse_from(["shoal", "happy", invalid]).is_err());
    }
}

#[test]
fn doctor_is_gone_without_an_alias() {
    use clap::CommandFactory;
    let command = Cli::command();
    assert!(command.find_subcommand("doctor").is_none());
    assert!(command.find_subcommand("reconcile").is_none());
}

#[test]
fn removal_commands_share_confirmation_flags() {
    for flag in ["-y", "--yes"] {
        let workspace = Cli::try_parse_from(["shoal", "rm", "workspace", flag]).unwrap();
        assert!(matches!(
            workspace.command,
            Some(Command::Lifecycle(LifecycleCommand::Rm {
                confirmation: ConfirmationArgs { yes: true },
                ..
            }))
        ));

        let repository = Cli::try_parse_from(["shoal", "repo", "rm", "repository", flag]).unwrap();
        assert!(matches!(
            repository.command,
            Some(Command::Repository(RepositoryCommand::Repo {
                command: Some(RepoCommand::Rm {
                    confirmation: ConfirmationArgs { yes: true },
                    ..
                })
            }))
        ));
    }
}

#[test]
fn work_setup_accepts_a_workspace_without_listing_filters() {
    for workspace in [None, Some("worker")] {
        let mut args = vec!["shoal", "work", "setup"];
        args.extend(workspace);
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Workspace(WorkspaceCommand::Work {
                command: Some(WorkCommand::Setup { workspace: target, skip: false }),
                ..
            })) if target.as_deref() == workspace
        ));
    }
    for filters in [
        vec!["--chats"],
        vec!["--repo", "project"],
        vec!["--status", "ready"],
        vec!["--print"],
    ] {
        let mut args = vec!["shoal", "work"];
        args.extend(filters);
        args.extend(["setup", "worker"]);
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn repository_setup_names_its_agent_and_forwards_arguments() {
    let cli = Cli::try_parse_from([
        "shoal", "repo", "setup", "project", "--agent", "codex", "--", "--model", "fast",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Some(Command::Repository(RepositoryCommand::Repo {
            command: Some(RepoCommand::Setup {
                repository: Some(repository),
                agent: Some(agent),
                args,
            })
        })) if repository == "project" && agent.name() == "codex" && args == ["--model", "fast"]
    ));
}

#[test]
fn agent_mode_flags_do_not_reserve_workspace_names() {
    for agent in ["claude", "codex"] {
        for (args, expected_workspace, expected_cli, expected_app) in [
            (["shoal", agent, "cli", "--app"], "cli", false, true),
            (["shoal", agent, "--cli", "app"], "app", true, false),
        ] {
            let parsed = Cli::try_parse_from(args).unwrap();
            let (workspace, cli, app) = match parsed.command {
                Some(Command::Agents(
                    AgentsCommand::Claude {
                        workspace,
                        cli,
                        app,
                        ..
                    }
                    | AgentsCommand::Codex {
                        workspace,
                        cli,
                        app,
                        ..
                    },
                )) => (workspace, cli, app),
                _ => panic!("wrong command"),
            };
            assert_eq!(workspace.as_deref(), Some(expected_workspace));
            assert_eq!(cli, expected_cli);
            assert_eq!(app, expected_app);
        }

        assert!(Cli::try_parse_from(["shoal", agent, "--cli", "--app"]).is_err());
        assert!(Cli::try_parse_from(["shoal", agent, "cli", "workspace"]).is_err());
    }
}

#[test]
fn chat_relaunches_pass_the_chosen_name() {
    let parsed = Cli::try_parse_from([
        "shoal", "chat", "repo", "--here", "--name", "talk", "--agent", "claude", "--", "-x",
    ])
    .unwrap();
    let Some(Command::Agents(AgentsCommand::Chat {
        here, name, args, ..
    })) = parsed.command
    else {
        panic!("wrong command")
    };
    assert!(here);
    assert_eq!(name.as_deref(), Some("talk"));
    assert_eq!(args, ["-x"]);
    let parsed = Cli::try_parse_from([
        "shoal",
        "plan",
        "https://forge.example/o/r/issues/1",
        "--repo",
        "r",
        "--here",
        "--name",
        "r-plan-1",
    ])
    .unwrap();
    assert!(matches!(
        parsed.command,
        Some(Command::Agents(AgentsCommand::Plan { name: Some(name), .. })) if name == "r-plan-1"
    ));
}

#[test]
fn add_uses_a_positional_new_branch_and_existing_branch_flag() {
    let new = Cli::try_parse_from(["shoal", "add", "repo", "feature/topic"]).unwrap();
    assert!(matches!(
        new.command,
        Some(Command::Create(CreateCommand::Add {
            repository,
            branch,
            existing: None,
            ..
        })) if repository.as_deref() == Some("repo")
            && branch.as_deref() == Some("feature/topic")
    ));

    let existing =
        Cli::try_parse_from(["shoal", "add", "repo", "--existing", "origin/topic"]).unwrap();
    assert!(matches!(
        existing.command,
        Some(Command::Create(CreateCommand::Add {
            repository,
            branch: None,
            existing,
            ..
        })) if repository.as_deref() == Some("repo")
            && existing.as_deref() == Some("origin/topic")
    ));
}

#[test]
fn creation_accepts_base_refs() {
    for args in [
        vec!["shoal", "add", "repo", "topic", "--base", "release/v1"],
        vec!["shoal", "add", "repo", "--issue", "122", "--base", "v1.0"],
        vec!["shoal", "add", "122", "--base", "HEAD~1"],
        vec![
            "shoal",
            "add",
            "repo",
            "--existing",
            "topic",
            "--base",
            "main",
        ],
    ] {
        let expected = *args.last().unwrap();
        let parsed = Cli::try_parse_from(args).unwrap();
        let base = match parsed.command.unwrap() {
            Command::Create(CreateCommand::Add { base, .. }) => base,
            _ => panic!("wrong command"),
        };
        assert_eq!(base.as_deref(), Some(expected));
    }
}

#[test]
fn done_flags_override_the_default_and_remain_available_to_agents() {
    for (args, expected) in [
        (vec!["shoal", "done"], false),
        (vec!["shoal", "done", "--cleanup"], true),
    ] {
        let command = Cli::try_parse_from(args).unwrap().command.unwrap();
        assert!(!command.is_administrative());
        let Command::Coordination(CoordinationCommand::Done {
            cleanup,
            workspace,
            undo,
            keep_demos,
        }) = command
        else {
            panic!("wrong command");
        };
        assert_eq!(cleanup, expected);
        assert!(!undo && !keep_demos);
        assert!(workspace.is_none());
    }
    assert!(Cli::try_parse_from(["shoal", "done", "--keep"]).is_err());
    assert!(Cli::try_parse_from(["shoal", "done", "--undo", "--cleanup"]).is_err());
    assert!(Cli::try_parse_from(["shoal", "done", "--undo", "--keep-demos"]).is_err());
    assert!(matches!(
        Cli::try_parse_from(["shoal", "done", "--keep-demos"])
            .unwrap()
            .command,
        Some(Command::Coordination(CoordinationCommand::Done {
            keep_demos: true,
            ..
        }))
    ));
    assert!(matches!(
        Cli::try_parse_from(["shoal", "done", "--undo"])
            .unwrap()
            .command,
        Some(Command::Coordination(CoordinationCommand::Done {
            undo: true,
            ..
        }))
    ));
    for flag in ["--draft", "--no-draft"] {
        assert!(Cli::try_parse_from(["shoal", "ready", flag]).is_err());
        assert!(Cli::try_parse_from(["shoal", "ready", "--undo", flag]).is_ok());
    }
    assert!(
        matches!(Cli::try_parse_from(["shoal", "done", "review", "--cleanup"]).unwrap().command,
            Some(Command::Coordination(CoordinationCommand::Done { workspace: Some(name), cleanup: true, undo: false, .. })) if name == "review")
    );
}

#[test]
fn review_selects_a_workspace_or_a_pr() {
    let review = Cli::try_parse_from(["shoal", "review", "--pr", "7", "--agent", "claude"]);
    assert!(matches!(review.unwrap().command,
            Some(Command::Changes(ChangesCommand::Review { workspace: None, pr: Some(pr), agent: Some(agent), .. })) if pr == "7" && agent.name() == "claude"));
    for args in [
        vec![
            "shoal", "review", "--pr", "7", "--manual", "--agent", "claude",
        ],
        vec!["shoal", "review", "workspace", "--pr", "7"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn config_show_is_available_to_workspace_processes() {
    let show = Cli::try_parse_from(["shoal", "config", "show", "workspace"]).unwrap();
    assert!(matches!(
        &show.command,
        Some(Command::Service(ServiceCommand::Config {
            command: ConfigCommand::Show { workspace }
        })) if workspace.as_deref() == Some("workspace")
    ));
    assert!(!show.command.unwrap().is_administrative());
    let reset = Cli::try_parse_from(["shoal", "config", "reset"]).unwrap();
    assert!(reset.command.unwrap().is_administrative());
}

#[test]
fn skipping_setup_is_administrative() {
    let skip = Cli::try_parse_from(["shoal", "work", "setup", "--skip"]).unwrap();
    assert!(skip.command.unwrap().is_administrative());
    let retry = Cli::try_parse_from(["shoal", "work", "setup"]).unwrap();
    assert!(!retry.command.unwrap().is_administrative());
}

#[test]
fn cleanup_commands_are_administrative() {
    for args in [
        vec!["shoal", "cleanup"],
        vec!["shoal", "cleanup", "--dry-run"],
        vec!["shoal", "cleanup", "workspaces", "--repo", "project"],
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
fn numbered_requests_keep_inspection_and_actions_distinct() {
    for args in [
        vec!["shoal", "approvals", "42"],
        vec!["shoal", "approvals", "42", "--approve"],
        vec!["shoal", "questions", "17"],
        vec!["shoal", "questions", "17", "--answer", "SQLite"],
    ] {
        assert!(Cli::try_parse_from(args).is_ok());
    }
    for args in [
        vec!["shoal", "approvals", "42", "--approve", "--deny"],
        vec![
            "shoal",
            "questions",
            "17",
            "--answer",
            "SQLite",
            "--withdraw",
        ],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?}");
    }
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
