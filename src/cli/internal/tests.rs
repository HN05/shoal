use std::os::unix::ffi::OsStringExt;

use clap::Parser;

use super::*;
use crate::cli::{Cli, Command, args::internal::InternalCommand};

#[test]
fn shoal_argv_names_only_a_non_default_state_directory() {
    let custom = Paths::for_test("/home");
    let default = Paths {
        state: "/home/.local/state/shoal".into(),
        ..custom.clone()
    };
    assert_eq!(shoal_argv(&default).unwrap()[1..], ["--text"]);
    assert_eq!(
        shoal_argv(&custom).unwrap()[1..],
        ["--state-dir", "/home/state", "--text"]
    );
}

#[test]
fn session_argv_names_even_the_default_state_directory() {
    let default = Paths {
        state: "/home/.local/state/shoal".into(),
        ..Paths::for_test("/home")
    };
    assert_eq!(
        session_argv(&default).unwrap()[1..],
        ["--state-dir", "/home/.local/state/shoal", "--text"]
    );
}

fn parse(worker: Worker<'_>, json: bool) -> InternalCommand {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path().join("home with spaces"));
    let args = internal_command(&paths, json, worker).unwrap();
    assert_eq!(args[0], std::env::current_exe().unwrap());
    let cli = Cli::try_parse_from(args).unwrap();
    assert_eq!(cli.state_dir, Some(paths.state));
    assert_eq!((cli.json, cli.text), (json, !json));
    let Some(Command::Internal { command }) = cli.command else {
        panic!("expected an internal command");
    };
    command
}

#[test]
fn add_round_trips_host_and_close_when_done() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    for close_when_done in [false, true] {
        let worker = Worker::Add {
            host: "herdr",
            close_when_done,
        };
        // The plan normally arrives in the place's environment.
        let mut args = internal_command(&paths, false, worker).unwrap();
        args.push("plan".into());
        let cli = Cli::try_parse_from(args).unwrap();
        assert_eq!(cli.state_dir.as_ref(), Some(&paths.state));
        let Some(Command::Internal {
            command:
                InternalCommand::Add {
                    host,
                    close_when_done: parsed,
                    plan,
                },
        }) = cli.command
        else {
            panic!("expected the add worker");
        };
        assert_eq!(host, "herdr");
        assert_eq!(parsed, close_when_done);
        assert_eq!(plan, "plan");
    }
}

#[test]
fn place_watch_round_trips_host_workspace_and_place() {
    for json in [false, true] {
        let InternalCommand::PlaceWatch {
            host,
            workspace,
            place,
        } = parse(
            Worker::PlaceWatch {
                host: "herdr",
                workspace: "workspace-id",
                place: "w1:t9",
            },
            json,
        )
        else {
            panic!("expected a place watcher");
        };
        assert_eq!(host, "herdr");
        assert_eq!(workspace, "workspace-id");
        assert_eq!(place, "w1:t9");
    }
}

#[test]
fn the_herdr_watcher_of_earlier_releases_still_parses() {
    let cli =
        Cli::try_parse_from(["shoal", "internal", "herdr-watch", "workspace-id", "w1:t9"]).unwrap();
    let Some(Command::Internal {
        command: InternalCommand::HerdrWatch { workspace, tab },
    }) = cli.command
    else {
        panic!("expected the Herdr watcher");
    };
    assert_eq!(
        (workspace.as_str(), tab.as_str()),
        ("workspace-id", "w1:t9")
    );
}

#[test]
fn land_round_trips_serialized_plan_and_globals() {
    let plan = r#"{"path":"/a path/with \"quotes\""}"#;
    for (json, push) in [(false, false), (true, true)] {
        let InternalCommand::Land {
            plan: parsed_plan,
            push: parsed_push,
        } = parse(Worker::Land { plan, push }, json)
        else {
            panic!("expected land worker");
        };
        assert_eq!(parsed_plan, plan);
        assert_eq!(parsed_push, push);
    }
}

#[test]
fn demo_round_trips_its_launch_and_literal_command() {
    let launch = r#"{"name":"web","host":"zmx","place":"a b"}"#;
    let command = vec!["sh".into(), "-c".into(), "echo $PORT; --json".into()];
    let InternalCommand::Demo {
        workspace,
        launch: parsed_launch,
        command: parsed_command,
    } = parse(
        Worker::Demo {
            workspace: "workspace-id",
            launch,
            command: &command,
        },
        false,
    )
    else {
        panic!("expected demo worker");
    };
    assert_eq!(workspace, "workspace-id");
    assert_eq!(parsed_launch, launch);
    assert_eq!(parsed_command, command);
}

#[test]
fn menu_workers_round_trip_their_view() {
    let InternalCommand::MenuRows { view } = parse(Worker::MenuRows { view: "inbox" }, false)
    else {
        panic!("expected menu rows worker");
    };
    assert_eq!(view, crate::cli::commands::menu::View::Inbox);
    assert!(matches!(
        parse(Worker::MenuNext, false),
        InternalCommand::MenuNext
    ));
}

#[test]
fn copy_ignored_round_trips_optional_setup_command() {
    for setup_cmd in [None, Some(Path::new("/repo/--setup 'literal'.sh"))] {
        let InternalCommand::CopyIgnored {
            setup_cmd: parsed_setup_cmd,
        } = parse(Worker::CopyIgnored { setup_cmd }, false)
        else {
            panic!("expected copy-ignored worker");
        };
        assert_eq!(parsed_setup_cmd.as_deref(), setup_cmd);
    }
}

#[test]
fn detached_round_trips_literal_arguments_and_globals() {
    let log = Path::new("/logs/session log");
    let command = vec![
        "tool".into(),
        "--json".into(),
        "--state-dir".into(),
        "literal ; $value".into(),
        OsString::from_vec(vec![0xff]),
    ];
    for json in [false, true] {
        for agent in [None, Some("happy-codex")] {
            let InternalCommand::Detached {
                workspace,
                log: parsed_log,
                agent: parsed_agent,
                command: parsed_command,
            } = parse(
                Worker::Detached {
                    workspace: "workspace-id",
                    log,
                    agent,
                    command: &command,
                },
                json,
            )
            else {
                panic!("expected detached worker");
            };
            assert_eq!(workspace, "workspace-id");
            assert_eq!(parsed_log, log);
            assert_eq!(parsed_agent.as_deref(), agent);
            assert_eq!(parsed_command, command);
        }
    }
}

#[test]
fn secret_tool_round_trips_launchers_and_tool() {
    let launchers = Path::new("/state/secret tools");
    let InternalCommand::SecretTool {
        launchers: parsed_launchers,
        tool,
        args,
    } = parse(
        Worker::SecretTool {
            launchers,
            tool: "agent-browser",
        },
        false,
    )
    else {
        panic!("expected secret tool worker");
    };
    assert_eq!(parsed_launchers, launchers);
    assert_eq!(tool, "agent-browser");
    assert!(args.is_empty());
}

#[test]
fn session_round_trips_records_and_literal_arguments() {
    let command = vec![
        "claude".into(),
        "--".into(),
        "literal ; $value".into(),
        OsString::from_vec(vec![0xff]),
    ];
    for (records, close_in) in [
        (vec![], None),
        (
            vec![PathBuf::from("/state/a record.json"), "/b".into()],
            Some("zmx"),
        ),
    ] {
        let InternalCommand::Session {
            workspace,
            agent,
            records: parsed_records,
            command: parsed_command,
            close_in: parsed_close_in,
        } = parse(
            Worker::Session {
                workspace: "workspace-id",
                agent: "claude",
                records: &records,
                command: &command,
                close_in,
            },
            false,
        )
        else {
            panic!("expected session worker");
        };
        assert_eq!(workspace, "workspace-id");
        assert_eq!(agent, "claude");
        assert_eq!(parsed_records, records);
        assert_eq!(parsed_command, command);
        assert_eq!(parsed_close_in.as_deref(), close_in);
    }
}

#[test]
fn queue_start_round_trips_its_entry() {
    let InternalCommand::QueueStart { entry } =
        parse(Worker::QueueStart { entry: "entry-id" }, false)
    else {
        panic!("expected queue start worker");
    };
    assert_eq!(entry, "entry-id");
}
