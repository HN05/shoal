use std::{ffi::OsString, path::Path};

use anyhow::Result;

use crate::paths::Paths;

pub const HERDR: &str = "herdr-internal";
pub const HERDR_WATCH: &str = "herdr-watch-internal";
pub const LAND: &str = "land-internal";
pub const DETACHED: &str = "detached-internal";

pub enum InternalCommand<'a> {
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
}

/// Build an argv for this executable, preserving the caller's resolved globals.
pub fn internal_command(
    paths: &Paths,
    json: bool,
    command: InternalCommand<'_>,
) -> Result<Vec<OsString>> {
    let mut args = vec![
        std::env::current_exe()?.into_os_string(),
        "--state-dir".into(),
        paths.state.as_os_str().to_owned(),
    ];
    if json {
        args.push("--json".into());
    }
    match command {
        InternalCommand::HerdrWatch { workspace, tab } => {
            args.extend([HERDR_WATCH.into(), workspace.into(), tab.into()]);
        }
        InternalCommand::Land { plan, push } => {
            args.push(LAND.into());
            if push {
                args.push("--push".into());
            }
            args.push(plan.into());
        }
        InternalCommand::Detached {
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
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn parse(command: InternalCommand<'_>, json: bool) -> Command {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(root.path().join("home with spaces"));
        let args = internal_command(&paths, json, command).unwrap();
        assert_eq!(args[0], std::env::current_exe().unwrap());
        let cli = Cli::try_parse_from(args).unwrap();
        assert_eq!(cli.state_dir, Some(paths.state));
        assert_eq!(cli.json, json);
        cli.command.unwrap()
    }

    #[test]
    fn herdr_watch_round_trips_workspace_and_tab() {
        for json in [false, true] {
            let Command::HerdrWatchInternal { workspace, tab } = parse(
                InternalCommand::HerdrWatch {
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
            let Command::LandInternal {
                plan: parsed_plan,
                push: parsed_push,
            } = parse(InternalCommand::Land { plan, push }, json)
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
                let Command::DetachedInternal {
                    workspace,
                    log: parsed_log,
                    agent: parsed_agent,
                    command: parsed_command,
                } = parse(
                    InternalCommand::Detached {
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
}
