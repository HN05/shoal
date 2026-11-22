//! Agent sessions that run detached from any terminal, in an app the user
//! continues them in, such as Happy. Shoal records and starts each session;
//! the [`App`] says what it runs and delivers its first message.
use crate::paths::StateFiles;
use shoal_refs::BranchName;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::Result;
use futures_util::future::BoxFuture;

use super::Launch;
use shoal_core::execution::{self, DetachedExecution, DetachedLaunch};

use crate::{
    agent::{Agent, ModelChoice},
    agents::{
        AgentHome, Integration,
        detached::{App, Delivered, Prepared, Session, model_args},
    },
    client::output,
    config::Effective,
    hosting::Caller,
    model::Space,
    paths::Paths,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::protocol::{DetachedRun, Launched};

/// Start `launch` as a session of `app`, detached from this terminal and
/// tracked like any other command of its space. The CLI returns once the
/// launch is recorded; the session's output goes to a log under Shoal's
/// state.
pub(super) async fn start(
    caller: Caller<'_>,
    integration: &dyn Integration,
    app: &dyn App,
    launch: Launch,
) -> Result<i32> {
    let Launch {
        name,
        space,
        settings,
        instructions,
        prompt,
        args,
        model,
        mode,
        narrowing,
        new_session,
        ..
    } = launch;
    crate::agents::ensure_available(&name, &settings)?;
    let home = space.home()?;
    let model_args = model_args(app, &settings, &name, &model)?;
    let directories = dir_args(caller, app, &settings, home).await;
    let directories = [model_args, directories].concat();
    let (prompt, args) = session_input(app, &settings, instructions, directories, prompt, args);
    if let Some(trust) = integration.trust_file() {
        super::trust::space(trust, caller.paths, &space, &Default::default()).await;
    }
    let files = session_files(caller.paths, home.id(), &name);
    let session = Session {
        paths: caller.paths,
        directory: home.path(),
        branch: match home {
            AgentHome::Workspace(workspace) => workspace.branch().map(BranchName::as_str),
            AgentHome::Chat { .. } => None,
        },
        prompt,
        args,
    };
    let prepared = with_effort(app.prepare(session).await?, app, &settings, &model);
    let agent = shoal_protocol::executions::TrackedAgent {
        name: app.execution_agent(),
        model,
        mode,
        narrowing,
        new_session,
    };
    let (execution, prepared) = launch_session(
        caller,
        &agent,
        &execution_space(home),
        &files,
        prepared,
        app.not_inherited(),
    )
    .await?;
    let (launch, delivered) = deliver_first_message(execution, prepared).await?;
    report_session(caller, app, home, &launch, delivered)?;
    Ok(0)
}

/// The space an execution at `home` runs for.
fn execution_space(home: AgentHome<'_>) -> crate::model::ExecutionSpace {
    match home {
        AgentHome::Workspace(workspace) => workspace.into(),
        AgentHome::Chat { chat, .. } => chat.into(),
    }
}

/// Launch the `prepared` session of `agent` in `space` as a detached
/// execution logging beside its other `files`, without the variables of
/// this process its app must not inherit, discarding the preparation when
/// it cannot start.
async fn launch_session(
    caller: Caller<'_>,
    agent: &shoal_protocol::executions::TrackedAgent,
    space: &crate::model::ExecutionSpace,
    files: &Path,
    prepared: Box<dyn Prepared>,
    not_inherited: &[&str],
) -> Result<(DetachedExecution, Box<dyn Prepared>)> {
    let env = prepared.env();
    let wrapper = execution::Wrapper {
        clear: not_inherited,
        env: &env,
    };
    let launched = execution::launch_detached(
        caller.paths,
        space,
        files.with_extension(crate::paths::SESSION_LOG),
        prepared.command(),
        &wrapper,
        Some(agent),
    )
    .await;
    match launched {
        Ok(execution) => Ok((execution, prepared)),
        Err(error) => {
            prepared.discard().await;
            Err(error)
        }
    }
}

/// Deliver the first message of the `prepared` session `execution` runs. A
/// session that does not receive it is no working launch: it is stopped and
/// its preparation discarded, and the launch fails with the reason.
async fn deliver_first_message(
    execution: DetachedExecution,
    prepared: Box<dyn Prepared>,
) -> Result<(DetachedLaunch, Delivered)> {
    let undelivered = match prepared.deliver().await {
        Ok(delivered) => return Ok((execution.launch, delivered)),
        Err(error) => error,
    };
    if let Err(error) = execution.interrupt().await {
        eprintln!(
            "warning: could not stop the session that did not receive its first message: {error:#}"
        );
    }
    prepared.discard().await;
    Err(undelivered)
}

/// Report the session `launch` started at `home`, with what `delivered`
/// says of its first message.
fn report_session(
    caller: Caller<'_>,
    app: &dyn App,
    home: AgentHome<'_>,
    launch: &DetachedLaunch,
    delivered: Delivered,
) -> Result<()> {
    let space: Space = match home {
        AgentHome::Workspace(workspace) => workspace.clone().into(),
        AgentHome::Chat { chat, .. } => chat.clone().into(),
    };
    let name = space.name.clone();
    let report = Launched {
        space: Box::new(space),
        entry: None,
        warnings: Vec::new(),
        start: shoal_protocol::protocol::Started::Detached(DetachedRun {
            execution_id: launch.execution_id.clone(),
            pid: launch.pid,
            log: launch.log.clone(),
            delivery: delivered.fields,
        }),
        machine: None,
    };
    let message = format!(
        "Started {} {} session in {} (execution {}, pid {}); open it in {}\nOutput: {}{}",
        app.name(),
        app.agent(),
        name,
        launch.execution_id,
        launch.pid,
        app.place(),
        launch.log.display(),
        delivered.note,
    );
    shoal_core::outln!("{}", output::line(caller.json, &message, &report)?);
    Ok(())
}

/// Where a session of agent `name` keeps its files: under its space's
/// state, named after the time it starts.
fn session_files(paths: &Paths, space: &SpaceId, name: &Agent) -> PathBuf {
    let stamp_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    paths
        .space_state(space)
        .join(format!("{name}-{stamp_millis}"))
}

/// The first message and arguments of a session with `instructions`, which
/// runs without Shoal's hooks, as a tracked launch separates them, with the
/// arguments that give it `directories`.
fn session_input(
    app: &dyn App,
    settings: &Effective,
    instructions: String,
    directories: Vec<OsString>,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> (Option<String>, Vec<OsString>) {
    let instruction_args = instruction_args(app, settings, &instructions);
    let (instruction_args, prompt) =
        super::tracked::separate_instructions(instruction_args, instructions, prompt);
    let prompt = Some(prompt).filter(|prompt| !prompt.is_empty());
    let args = instruction_args
        .into_iter()
        .chain(directories)
        .chain(args)
        .collect();
    (prompt, args)
}

/// `prepared` with the variable that sets the effort of `app`'s agent to
/// `model`'s, which overrides its effort argument, when it reads one and
/// the app passes it arguments.
fn with_effort(
    prepared: Box<dyn Prepared>,
    app: &dyn App,
    settings: &Effective,
    model: &ModelChoice,
) -> Box<dyn Prepared> {
    let variable = app
        .takes_agent_args()
        .then(|| settings.launch(&app.agent()).effort_environment(model))
        .flatten();
    match variable {
        Some(variable) => Box::new(WithEnv {
            prepared,
            env: vec![variable],
        }),
        None => prepared,
    }
}

/// A prepared session that runs with more variables than its app gives it.
struct WithEnv {
    prepared: Box<dyn Prepared>,
    env: Vec<(&'static str, String)>,
}

impl Prepared for WithEnv {
    fn command(&self) -> Vec<OsString> {
        self.prepared.command()
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        [self.prepared.env(), self.env.clone()].concat()
    }

    fn discard(self: Box<Self>) -> BoxFuture<'static, ()> {
        self.prepared.discard()
    }

    fn deliver(&self) -> BoxFuture<'_, Result<Delivered>> {
        self.prepared.deliver()
    }
}

/// The arguments that give `app`'s agent `instructions` apart from its first
/// message, when it takes them.
fn instruction_args(
    app: &dyn App,
    settings: &Effective,
    instructions: &str,
) -> Option<Vec<OsString>> {
    settings
        .launch(&app.agent())
        .arguments
        .instruction_args(instructions)
        .filter(|_| app.takes_agent_args())
}

/// Whether `app`'s agent takes its instructions apart from its first
/// message under `settings`.
pub(super) fn takes_instructions(app: &dyn App, settings: &Effective) -> bool {
    instruction_args(app, settings, "").is_some()
}

/// The arguments that give `app`'s agent the directories its agent works
/// in at `home`, when it takes arguments.
async fn dir_args(
    caller: Caller<'_>,
    app: &dyn App,
    settings: &Effective,
    home: AgentHome<'_>,
) -> Vec<OsString> {
    match app.takes_agent_args() {
        true => crate::homes::dir_args(caller.paths, home, settings, &app.agent()).await,
        false => Vec::new(),
    }
}

#[cfg(test)]
mod tests;
