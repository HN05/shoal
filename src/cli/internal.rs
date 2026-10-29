use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::paths::Paths;

/// The hidden command that holds every worker below.
pub const GROUP: &str = "internal";
pub const HERDR_WATCH: &str = "herdr-watch";
pub const PLACE_WATCH: &str = "place-watch";
pub const LAND: &str = "land";
pub const DETACHED: &str = "detached";
pub const SESSION: &str = "session";
pub const DEMO: &str = "demo";
pub const SECRET_TOOL: &str = "secret-tool";
pub const QUEUE_START: &str = "queue-start";
pub const ADD: &str = "add";
pub const LAUNCH: &str = "launch";
pub const COPY_IGNORED: &str = "copy-ignored";
pub const MENU_ROWS: &str = "menu-rows";
pub const MENU_NEXT: &str = "menu-next";
pub const PROXY_REPORT: &str = "proxy-report";
pub const TAKE_OVER: &str = "take-over";

pub enum Worker<'a> {
    /// Close `place` of `host` once `workspace` completes or is removed,
    /// while it is the `instance` the watch began with; the completion at
    /// `kept_at` predates the watch.
    PlaceWatch {
        host: &'a str,
        workspace: &'a str,
        place: &'a str,
        instance: Option<&'a str>,
        kept_at: Option<&'a str>,
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
    /// A tracked agent inside a new place a host opened; `records` are
    /// recovery records the launch consumes once it starts. With `close_in`,
    /// the place of that host closes once the workspace is done.
    Session {
        workspace: &'a str,
        agent: &'a str,
        records: &'a [PathBuf],
        command: &'a [OsString],
        /// The host that opened the place.
        host: &'a str,
        close_in: Option<&'a str>,
    },
    /// A demo's wrapper; `launch` is its JSON [`crate::demos::Launch`].
    Demo {
        workspace: &'a str,
        launch: &'a str,
        command: &'a [OsString],
    },
    /// Start an admitted queue entry where its launching terminal would.
    QueueStart {
        entry: &'a str,
    },
    /// Add the workspace planned in the environment in the place `host`
    /// opened for it, closing it once the workspace is done when
    /// `close_when_done` is set.
    Add {
        host: &'a str,
        close_when_done: bool,
    },
    /// Copy the checkout's ignored files, then run setup.
    CopyIgnored {
        setup_cmd: Option<&'a Path>,
    },
    /// Print a bare `shoal` picker view's rows, by its name.
    MenuRows {
        view: &'a str,
    },
    /// Print the fzf actions that switch the picker to its next view.
    MenuNext,
    /// Send `report`, the JSON of a proxy's report, for a wrapper of an
    /// older release that the upgraded daemon no longer speaks to.
    ProxyReport {
        report: &'a str,
    },
    /// Run the command a launch file carries, for a place whose host cannot
    /// set its environment.
    Launch {
        file: &'a Path,
    },
    /// Continue an agent's wrapper whose binary an upgrade replaced: resume
    /// the agent of `workspace` from `record` over the daemon connection
    /// at descriptor `connection`, writing to `log` when detached. Wrappers
    /// of earlier releases start it, so it only gains optional arguments.
    TakeOver {
        workspace: &'a str,
        agent: &'a str,
        connection: i32,
        record: Option<&'a Path>,
        log: Option<&'a Path>,
    },
}

/// This executable for a command a terminal or session runs as typed,
/// naming the state directory even when it is the default, so a
/// `SHOAL_STATE_DIR` in the place's environment cannot redirect it. It shows
/// text to the person at that terminal whatever `SHOAL_OUTPUT` it has.
pub fn shoal_argv(paths: &Paths) -> Result<Vec<OsString>> {
    Ok(vec![
        crate::fsutil::invoked_executable()?.into_os_string(),
        "--state-dir".into(),
        paths.state.clone().into_os_string(),
        "--text".into(),
    ])
}

/// Build an argv for this executable, preserving the caller's resolved globals.
/// The output format is explicit, so an inherited `SHOAL_OUTPUT` cannot change it.
pub fn internal_command(paths: &Paths, json: bool, worker: Worker<'_>) -> Result<Vec<OsString>> {
    internal_command_for(&paths.state, json, worker)
}

/// [`internal_command`] as shell text, for a picker to run.
pub fn shell_command(paths: &Paths, worker: Worker<'_>) -> Result<String> {
    let words = internal_command(paths, false, worker)?
        .into_iter()
        .map(|word| word.into_string())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| anyhow::anyhow!("the shoal executable or state path is not UTF-8"))?;
    Ok(crate::shell::quote(&words))
}

/// [`internal_command`] for the state directory `state`.
pub fn internal_command_for(state: &Path, json: bool, worker: Worker<'_>) -> Result<Vec<OsString>> {
    let mut args = vec![
        crate::fsutil::invoked_executable()?.into_os_string(),
        "--state-dir".into(),
        state.as_os_str().to_owned(),
        if json { "--json" } else { "--text" }.into(),
    ];
    args.push(GROUP.into());
    match worker {
        Worker::PlaceWatch {
            host,
            workspace,
            place,
            instance,
            kept_at,
        } => {
            args.extend([
                PLACE_WATCH.into(),
                host.into(),
                workspace.into(),
                place.into(),
            ]);
            if let Some(instance) = instance {
                args.extend(["--instance".into(), instance.into()]);
            }
            if let Some(head) = kept_at {
                args.extend(["--kept-at".into(), head.into()]);
            }
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
            host,
            close_in,
        } => {
            args.extend([SESSION.into(), workspace.into(), agent.into()]);
            args.extend(["--host".into(), host.into()]);
            for record in records {
                args.extend(["--record".into(), record.as_os_str().to_owned()]);
            }
            if let Some(host) = close_in {
                args.extend(["--close-in".into(), host.into()]);
            }
            args.push("--".into());
            args.extend_from_slice(command);
        }
        Worker::Demo {
            workspace,
            launch,
            command,
        } => {
            args.extend([
                DEMO.into(),
                workspace.into(),
                "--launch".into(),
                launch.into(),
                "--".into(),
            ]);
            args.extend_from_slice(command);
        }
        Worker::QueueStart { entry } => args.extend([QUEUE_START.into(), entry.into()]),
        Worker::Add {
            host,
            close_when_done,
        } => {
            args.extend([ADD.into(), "--host".into(), host.into()]);
            if close_when_done {
                args.push("--close-when-done".into());
            }
        }
        Worker::CopyIgnored { setup_cmd } => {
            args.push(COPY_IGNORED.into());
            if let Some(setup_cmd) = setup_cmd {
                args.extend(["--".into(), setup_cmd.as_os_str().to_owned()]);
            }
        }
        Worker::MenuRows { view } => args.extend([MENU_ROWS.into(), view.into()]),
        Worker::MenuNext => args.push(MENU_NEXT.into()),
        Worker::ProxyReport { report } => args.extend([PROXY_REPORT.into(), report.into()]),
        Worker::Launch { file } => args.extend([LAUNCH.into(), file.as_os_str().to_owned()]),
        Worker::TakeOver {
            workspace,
            agent,
            connection,
            record,
            log,
        } => {
            args.extend([
                TAKE_OVER.into(),
                workspace.into(),
                "--agent".into(),
                agent.into(),
                "--connection".into(),
                connection.to_string().into(),
            ]);
            if let Some(record) = record {
                args.extend(["--record".into(), record.as_os_str().to_owned()]);
            }
            if let Some(log) = log {
                args.extend(["--log".into(), log.as_os_str().to_owned()]);
            }
        }
    }
    Ok(args)
}

#[cfg(test)]
mod tests;
