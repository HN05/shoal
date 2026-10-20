use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::paths::Paths;

/// The hidden command that holds every worker below.
pub const GROUP: &str = "internal";
pub const HERDR: &str = "herdr";
pub const HERDR_WATCH: &str = "herdr-watch";
pub const LAND: &str = "land";
pub const DETACHED: &str = "detached";
pub const SESSION: &str = "session";
pub const SECRET_TOOL: &str = "secret-tool";
pub const QUEUE_START: &str = "queue-start";
pub const ADD: &str = "add";

pub enum Worker<'a> {
    HerdrWatch {
        workspace: &'a str,
        tab: &'a str,
    },
    Land {
        plan: &'a str,
        push: bool,
    },
    Detached {
        workspace: &'a str,
        log: &'a Path,
        agent: Option<&'a str>,
        command: &'a [OsString],
    },
    /// A tool run through its secret launcher in `launchers`; the launcher
    /// appends the tool's arguments.
    SecretTool {
        launchers: &'a Path,
        tool: &'a str,
    },
    /// A tracked agent inside a zmx session; `records` are recovery records
    /// the launch consumes once it starts.
    Session {
        workspace: &'a str,
        agent: &'a str,
        records: &'a [PathBuf],
        command: &'a [OsString],
    },
    /// Start an admitted queue entry where its launching terminal would.
    QueueStart {
        entry: &'a str,
    },
    /// Add the workspace planned in the environment in this detached session.
    Add,
}

/// This executable for a command a terminal runs as typed. The terminal
/// inherits HOME, so only a non-default state directory needs naming.
pub fn shoal_argv(paths: &Paths) -> Result<Vec<OsString>> {
    let mut argv = vec![crate::fsutil::invoked_executable()?.into_os_string()];
    if !paths.is_default_state() {
        argv.extend(["--state-dir".into(), paths.state.clone().into_os_string()]);
    }
    Ok(argv)
}

/// This executable for a command a session runs in the caller's environment,
/// naming the state directory even when it is the default, so an inherited
/// `SHOAL_STATE_DIR` cannot redirect it.
pub fn session_argv(paths: &Paths) -> Result<Vec<OsString>> {
    Ok(vec![
        crate::fsutil::invoked_executable()?.into_os_string(),
        "--state-dir".into(),
        paths.state.clone().into_os_string(),
    ])
}

/// Build an argv for this executable, preserving the caller's resolved globals.
pub fn internal_command(paths: &Paths, json: bool, worker: Worker<'_>) -> Result<Vec<OsString>> {
    let mut args = vec![
        crate::fsutil::invoked_executable()?.into_os_string(),
        "--state-dir".into(),
        paths.state.as_os_str().to_owned(),
    ];
    if json {
        args.push("--json".into());
    }
    args.push(GROUP.into());
    match worker {
        Worker::HerdrWatch { workspace, tab } => {
            args.extend([HERDR_WATCH.into(), workspace.into(), tab.into()]);
        }
        Worker::Land { plan, push } => {
            args.push(LAND.into());
            if push {
                args.push("--push".into());
            }
            args.push(plan.into());
        }
        Worker::Detached {
            workspace,
            log,
            agent,
            command,
        } => {
            args.extend([
                DETACHED.into(),
                workspace.into(),
                "--log".into(),
                log.as_os_str().to_owned(),
            ]);
            if let Some(agent) = agent {
                args.extend(["--agent".into(), agent.into()]);
            }
            args.push("--".into());
            args.extend_from_slice(command);
        }
        Worker::SecretTool { launchers, tool } => {
            args.extend([
                SECRET_TOOL.into(),
                launchers.as_os_str().to_owned(),
                tool.into(),
                "--".into(),
            ]);
        }
        Worker::Session {
            workspace,
            agent,
            records,
            command,
        } => {
            args.extend([SESSION.into(), workspace.into(), agent.into()]);
            for record in records {
                args.extend(["--record".into(), record.as_os_str().to_owned()]);
            }
            args.push("--".into());
            args.extend_from_slice(command);
        }
        Worker::QueueStart { entry } => args.extend([QUEUE_START.into(), entry.into()]),
        Worker::Add => args.push(ADD.into()),
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, InternalCommand};

    #[test]
    fn shoal_argv_names_only_a_non_default_state_directory() {
        let custom = Paths::for_test("/home");
        let default = Paths {
            state: "/home/.local/state/shoal".into(),
            ..custom.clone()
        };
        assert_eq!(shoal_argv(&default).unwrap().len(), 1);
        assert_eq!(
            shoal_argv(&custom).unwrap()[1..],
            ["--state-dir", "/home/state"]
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
            ["--state-dir", "/home/.local/state/shoal"]
        );
    }

    fn parse(worker: Worker<'_>, json: bool) -> InternalCommand {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(root.path().join("home with spaces"));
        let args = internal_command(&paths, json, worker).unwrap();
        assert_eq!(args[0], std::env::current_exe().unwrap());
        let cli = Cli::try_parse_from(args).unwrap();
        assert_eq!(cli.state_dir, Some(paths.state));
        assert_eq!(cli.json, json);
        let Some(Command::Internal { command }) = cli.command else {
            panic!("expected an internal command");
        };
        command
    }

    #[test]
    fn herdr_watch_round_trips_workspace_and_tab() {
        for json in [false, true] {
            let InternalCommand::HerdrWatch { workspace, tab } = parse(
                Worker::HerdrWatch {
                    workspace: "workspace-id",
                    tab: "w1:t9",
                },
                json,
            ) else {
                panic!("expected Herdr watcher");
            };
            assert_eq!(workspace, "workspace-id");
            assert_eq!(tab, "w1:t9");
        }
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
        for records in [
            vec![],
            vec![PathBuf::from("/state/a record.json"), "/b".into()],
        ] {
            let InternalCommand::Session {
                workspace,
                agent,
                records: parsed_records,
                command: parsed_command,
            } = parse(
                Worker::Session {
                    workspace: "workspace-id",
                    agent: "claude",
                    records: &records,
                    command: &command,
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
}
