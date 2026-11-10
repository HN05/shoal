//! The command lines that start Shoal's workers: the hidden `internal`
//! commands the daemon, the execution wrapper and the CLI run in their own
//! processes.
use crate::client::space_ids::SpaceId;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::{agent::ModelChoice, execution::TrackedAgent, paths::Paths, sessions::HostName};

/// The hidden command that holds every worker below.
pub const GROUP: &str = "internal";
pub const HERDR_WATCH: &str = "herdr-watch";
pub const PLACE_WATCH: &str = "place-watch";
pub const MERGE: &str = "merge";
pub const UNMERGE: &str = "unmerge";
pub const DETACHED: &str = "detached";
pub const SESSION: &str = "session";
pub const DEMO: &str = "demo";
pub const SECRET_TOOL: &str = "secret-tool";
pub const PUSH_GATE: &str = "push-gate";
pub const GIT_CREDENTIAL: &str = "git-credential";
pub const QUEUE_START: &str = "queue-start";
pub const OPERATION: &str = "operation";
pub const ADD: &str = "add";
pub const LAUNCH: &str = "launch";
pub const COPY_IGNORED: &str = "copy-ignored";
pub const MENU_ROWS: &str = "menu-rows";
pub const MENU_NEXT: &str = "menu-next";
pub const APPROVAL_ROWS: &str = "approval-rows";
pub const APPROVAL_DECIDE: &str = "approval-decide";
pub const PROXY_REPORT: &str = "proxy-report";
pub const TAKE_OVER: &str = "take-over";

/// An approval decision the picker's decision worker makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Deny,
}

pub enum Worker<'a> {
    /// Close `place` of `host` once `workspace` completes or is removed,
    /// while it is the `instance` the watch began with; the completion at
    /// `kept_at` predates the watch.
    PlaceWatch {
        host: HostName,
        workspace: &'a SpaceId,
        place: &'a str,
        instance: Option<&'a str>,
        kept_at: Option<&'a str>,
    },
    Merge {
        plan: &'a str,
        result: &'a Path,
    },
    Unmerge {
        plan: &'a str,
        result: &'a Path,
    },
    Detached {
        workspace: &'a SpaceId,
        log: &'a Path,
        agent: Option<&'a TrackedAgent>,
        command: &'a [OsString],
    },
    /// A tool run through its secret launcher in `launchers`; the launcher
    /// appends the tool's arguments.
    SecretTool {
        launchers: &'a Path,
        tool: &'a str,
    },
    /// Git's remote helper for an agent's pushes; the launcher appends the
    /// remote and URL Git gives it.
    PushGate,
    /// A tracked agent inside a new place a host opened; `records` are
    /// recovery records the launch consumes once it starts. With `close_in`,
    /// the place of that host closes once the workspace is done.
    Session {
        workspace: &'a SpaceId,
        agent: &'a TrackedAgent,
        records: &'a [PathBuf],
        command: &'a [OsString],
        /// The host that opened the place.
        host: HostName,
        close_in: Option<HostName>,
    },
    /// A demo's wrapper; `launch` is its JSON [`crate::demos::Launch`].
    Demo {
        workspace: &'a SpaceId,
        launch: &'a str,
        command: &'a [OsString],
    },
    /// Run the operation whose order the daemon writes to standard input.
    Operation,
    /// Start an admitted queue entry where its launching terminal would.
    QueueStart {
        entry: &'a str,
    },
    /// Add the workspace planned in the environment in the place `host`
    /// opened for it, `focused` when it shows the place, closing it once
    /// the workspace is done when `close_when_done` is set.
    Add {
        host: HostName,
        close_when_done: bool,
        focused: bool,
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
    /// Print the `shoal approvals` picker's rows, of `space` only when given.
    ApprovalRows {
        space: Option<&'a str>,
    },
    /// Make `decision` on the request whose picker row the picker appends.
    ApprovalDecide {
        decision: Decision,
    },
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
        workspace: &'a SpaceId,
        agent: &'a TrackedAgent,
        connection: i32,
        record: Option<&'a Path>,
        log: Option<&'a Path>,
    },
}

/// The worker options that carry `model`, as `--model` and `--effort` take
/// them.
fn model_args(model: &ModelChoice) -> Vec<OsString> {
    [("--model", &model.model), ("--effort", &model.effort)]
        .into_iter()
        .filter_map(|(flag, value)| Some([flag.into(), value.as_deref()?.into()]))
        .flatten()
        .collect()
}

/// The worker option that carries `agent`'s `--permission` narrowing, as
/// JSON; none without one.
fn narrowing_args(agent: &crate::execution::TrackedAgent) -> Vec<OsString> {
    match agent.narrowing.is_empty() {
        true => Vec::new(),
        false => vec![
            "--narrowing".into(),
            serde_json::to_string(&agent.narrowing)
                .expect("a narrowing serializes")
                .into(),
        ],
    }
}

/// This executable for a command a terminal or session runs as typed,
/// naming the state directory even when it is the default, so a
/// `SHOAL_STATE_DIR` in the place's environment cannot redirect it. It shows
/// text to the person at that terminal whatever `SHOAL_OUTPUT` it has.
pub fn shoal_argv(paths: &Paths) -> Result<Vec<OsString>> {
    Ok(vec![
        crate::fsutil::executables::invoked_executable()?.into_os_string(),
        "--state-dir".into(),
        paths.state.clone().into_os_string(),
        "--text".into(),
    ])
}

/// The command that starts the worker `argv` in a session of its own, apart
/// from the process starting it and its terminal; the caller sets its
/// standard streams, environment and directory.
pub fn detached_command(argv: &[OsString]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(&argv[0]);
    command.args(&argv[1..]);
    // SAFETY: setsid detaches the forked child without allocating, before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

/// Build an argv for this executable, preserving the caller's resolved globals.
/// The output format is explicit, so an inherited `SHOAL_OUTPUT` cannot change it.
pub fn internal_command(paths: &Paths, json: bool, worker: Worker<'_>) -> Result<Vec<OsString>> {
    internal_command_for(&paths.state, json, worker)
}

/// [`internal_command`] for the state directory `state`.
pub fn internal_command_for(state: &Path, json: bool, worker: Worker<'_>) -> Result<Vec<OsString>> {
    let mut args = vec![
        crate::fsutil::executables::invoked_executable()?.into_os_string(),
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
                host.as_str().into(),
                workspace.as_str().into(),
                place.into(),
            ]);
            if let Some(instance) = instance {
                args.extend(["--instance".into(), instance.into()]);
            }
            if let Some(head) = kept_at {
                args.extend(["--kept-at".into(), head.into()]);
            }
        }
        Worker::Merge { plan, result } => {
            args.extend([
                MERGE.into(),
                "--result".into(),
                result.as_os_str().to_owned(),
            ]);
            args.push(plan.into());
        }
        Worker::Unmerge { plan, result } => {
            args.extend([
                UNMERGE.into(),
                "--result".into(),
                result.as_os_str().to_owned(),
            ]);
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
                workspace.as_str().into(),
                "--log".into(),
                log.as_os_str().to_owned(),
            ]);
            if let Some(agent) = agent {
                args.extend(["--agent".into(), agent.name.name().into()]);
                args.extend(model_args(&agent.model));
                args.extend(["--mode".into(), agent.mode.as_str().into()]);
                args.extend(narrowing_args(agent));
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
        Worker::PushGate => args.push(PUSH_GATE.into()),
        Worker::Session {
            workspace,
            agent,
            records,
            command,
            host,
            close_in,
        } => {
            args.extend([
                SESSION.into(),
                workspace.as_str().into(),
                agent.name.name().into(),
            ]);
            args.extend(model_args(&agent.model));
            args.extend(["--mode".into(), agent.mode.as_str().into()]);
            args.extend(narrowing_args(agent));
            args.extend(["--host".into(), host.as_str().into()]);
            for record in records {
                args.extend(["--record".into(), record.as_os_str().to_owned()]);
            }
            if let Some(host) = close_in {
                args.extend(["--close-in".into(), host.as_str().into()]);
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
                workspace.as_str().into(),
                "--launch".into(),
                launch.into(),
                "--".into(),
            ]);
            args.extend_from_slice(command);
        }
        Worker::Operation => args.push(OPERATION.into()),
        Worker::QueueStart { entry } => args.extend([QUEUE_START.into(), entry.into()]),
        Worker::Add {
            host,
            close_when_done,
            focused,
        } => {
            args.extend([ADD.into(), "--host".into(), host.as_str().into()]);
            if close_when_done {
                args.push("--close-when-done".into());
            }
            if focused {
                args.push("--focused".into());
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
        Worker::ApprovalRows { space } => {
            args.push(APPROVAL_ROWS.into());
            if let Some(space) = space {
                args.extend(["--space".into(), space.into()]);
            }
        }
        Worker::ApprovalDecide { decision } => {
            let decision = match decision {
                Decision::Approve => "approve",
                Decision::Deny => "deny",
            };
            args.extend([APPROVAL_DECIDE.into(), decision.into()]);
        }
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
                workspace.as_str().into(),
                "--agent".into(),
                agent.name.name().into(),
            ]);
            args.extend(model_args(&agent.model));
            args.extend(["--mode".into(), agent.mode.as_str().into()]);
            args.extend(narrowing_args(agent));
            args.extend(["--connection".into(), connection.to_string().into()]);
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
