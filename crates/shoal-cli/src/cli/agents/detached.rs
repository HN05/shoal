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

use super::{ChatLaunch, Launch};
use crate::client::output::agents::{DetachedChat, DetachedSession};
use crate::{
    agent::{Agent, ModelChoice},
    agents::{
        Integration,
        detached::{App, Delivered, Prepared, Session, model_args},
    },
    cli::context::Context,
    config::{Effective, templates},
    execution::{self, DetachedExecution, DetachedLaunch},
    model::{Chat, Repository, Workspace},
    paths::Paths,
};

/// Start `launch` as a session of `app`, detached from this terminal and
/// tracked like any other workspace command. The CLI returns once the launch
/// is recorded; the session's output goes to a log under Shoal's state.
pub(super) async fn start(
    ctx: &Context,
    integration: &dyn Integration,
    app: &dyn App,
    launch: Launch,
) -> Result<i32> {
    let Launch {
        name,
        workspace,
        settings,
        instructions,
        prompt,
        args,
        model,
        mode,
        narrowing,
        ..
    } = launch;
    crate::agents::ensure_available(&name, &settings)?;
    let model_args = model_args(app, &settings, &name, &model)?;
    let directories = workspace_dir_args(ctx, app, &settings, &workspace).await;
    let directories = [model_args, directories].concat();
    let (prompt, args) = workspace_input(app, &settings, instructions, directories, prompt, args);
    if let Some(trust) = integration.trust_file() {
        super::trust::tracked(trust, ctx, &workspace, &Default::default()).await;
    }
    let files = session_files(&ctx.paths, &workspace, &name);
    let session = Session {
        paths: &ctx.paths,
        directory: &workspace.path,
        branch: workspace.branch.as_ref().map(BranchName::as_str),
        prompt,
        args,
    };
    let prepared = with_effort(app.prepare(session).await?, app, &settings, &model);
    let agent = execution::TrackedAgent {
        name: app.execution_agent(),
        model,
        mode,
        narrowing,
    };
    let (execution, prepared) = launch_session(
        ctx,
        &agent,
        &(&workspace).into(),
        &files,
        prepared,
        app.not_inherited(),
    )
    .await?;
    let (launch, delivered) = deliver_first_message(execution, prepared).await?;
    report_session(ctx, app, &workspace, &launch, delivered)?;
    Ok(0)
}

/// Launch the `prepared` session of `agent` in `space` as a detached
/// execution logging beside its other `files`, without the variables of
/// this process its app must not inherit, discarding the preparation when
/// it cannot start.
async fn launch_session(
    ctx: &Context,
    agent: &execution::TrackedAgent,
    space: &crate::model::ExecutionSpace,
    files: &Path,
    prepared: Box<dyn Prepared>,
    not_inherited: &[&str],
) -> Result<(DetachedExecution, Box<dyn Prepared>)> {
    let launched = execution::launch_detached(
        &ctx.paths,
        space,
        files.with_extension(crate::paths::SESSION_LOG),
        prepared.command(),
        not_inherited,
        &prepared.env(),
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

/// Report the workspace session `launch` started, with what `delivered`
/// says of its first message.
fn report_session(
    ctx: &Context,
    app: &dyn App,
    workspace: &Workspace,
    launch: &DetachedLaunch,
    delivered: Delivered,
) -> Result<()> {
    let report = DetachedSession {
        workspace: workspace.clone(),
        agent: app.agent(),
        execution_id: launch.execution_id.clone(),
        pid: launch.pid,
        log: launch.log.clone(),
        delivery: delivered.fields,
    };
    ctx.emit(
        &format!(
            "Started {} {} session in {} (execution {}, pid {})\nOutput: {}{}",
            app.name(),
            app.agent(),
            workspace.name,
            launch.execution_id,
            launch.pid,
            launch.log.display(),
            delivered.note,
        ),
        report,
    )
}

/// Where a session of agent `name` keeps its files: under the workspace's
/// state, named after the time it starts.
fn session_files(paths: &Paths, workspace: &Workspace, name: &Agent) -> PathBuf {
    let stamp_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    paths
        .space_state(&workspace.id)
        .join(format!("{name}-{stamp_millis}"))
}

/// The first message and arguments of a workspace session with
/// `instructions`, which runs without Shoal's hooks, as a tracked launch
/// separates them, with the arguments that give it `directories`.
fn workspace_input(
    app: &dyn App,
    settings: &Effective,
    instructions: String,
    directories: Vec<OsString>,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> (Option<String>, Vec<OsString>) {
    let instruction_args = instruction_args(app, settings, &instructions);
    let (instruction_args, prompt) =
        super::launch::separate_instructions(instruction_args, instructions, prompt);
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

/// The arguments that give `app`'s agent the directories a workspace's agent
/// works in, when it takes arguments.
async fn workspace_dir_args(
    ctx: &Context,
    app: &dyn App,
    settings: &Effective,
    workspace: &Workspace,
) -> Vec<OsString> {
    if app.takes_agent_args() {
        crate::agents::workspace_dir_args(&ctx.paths, settings, &app.agent(), workspace).await
    } else {
        Vec::new()
    }
}

/// The arguments that give `app`'s agent the directories `chat`'s agent
/// works in, when it takes arguments.
fn chat_dir_args(app: &dyn App, settings: &Effective, home: &Path, chat: &Chat) -> Vec<OsString> {
    if app.takes_agent_args() {
        crate::agents::chat_dir_args(settings, &app.agent(), home, chat)
    } else {
        Vec::new()
    }
}

/// Start `chat` as a session of `app`, agent `name`, in its checkout or
/// worktree, detached and tracked as workspace sessions are, as an
/// execution of the chat, since the user continues it in the app.
pub(super) async fn start_chat(
    ctx: &Context,
    integration: &dyn Integration,
    app: &dyn App,
    name: &Agent,
    chat: &ChatLaunch,
) -> Result<i32> {
    let ChatLaunch {
        repository,
        name: chat_name,
        mode,
        narrowing,
        parent,
        priority,
        settings,
        model,
        links,
        ..
    } = chat;
    crate::agents::ensure_available(name, settings)?;
    let model_args = model_args(app, settings, name, model)?;
    let home = chat.home();
    if let Some(trust) = integration.trust_file() {
        super::trust::chat(
            trust,
            ctx,
            repository.registered(),
            home,
            &Default::default(),
        )
        .await;
    }
    let new = crate::client::chats::NewChat {
        priority: *priority,
        name: chat_name,
        agent: name,
        mode,
        narrowing: *narrowing,
        parent: parent.as_ref(),
        queue_entry: None,
    };
    let record = crate::client::chats::start(&ctx.paths, home, new, None).await?;
    super::link_chat(ctx, &record, links).await;
    let (prompt, args) = chat_input(app, &ctx.paths.home, &record, chat);
    let args = [model_args, args].concat();
    let files = ctx.paths.chat_files(&record);
    let session = Session {
        paths: &ctx.paths,
        directory: home.path(),
        branch: None,
        prompt,
        args,
    };
    let prepared = with_effort(app.prepare(session).await?, app, settings, model);
    let agent = execution::TrackedAgent {
        name: app.execution_agent(),
        model: model.clone(),
        mode: mode.clone(),
        narrowing: *narrowing,
    };
    let space = crate::model::ExecutionSpace {
        id: record.id.clone(),
        number: record.number,
        kind: crate::model::SpaceKind::Chat,
        path: home.path().to_owned(),
    };
    let (execution, prepared) =
        launch_session(ctx, &agent, &space, &files, prepared, app.not_inherited()).await?;
    let (launch, delivered) = deliver_first_message(execution, prepared).await?;
    report_chat(
        ctx,
        app,
        repository.registered(),
        &record,
        &launch,
        delivered,
    )?;
    Ok(0)
}

/// The first message and arguments of the session of `launch`, recorded as
/// `chat`: the instructions as arguments with its opening, where the agent
/// takes them apart, otherwise as the first message, which starts a turn
/// before the user speaks. The directories its agent works in come before
/// its own arguments.
fn chat_input(
    app: &dyn App,
    home: &Path,
    chat: &Chat,
    launch: &ChatLaunch,
) -> (Option<String>, Vec<OsString>) {
    let ChatLaunch {
        settings,
        instructions,
        opening,
        args,
        ..
    } = launch;
    let args = chat_dir_args(app, settings, home, chat)
        .into_iter()
        .chain(args.to_vec());
    match instruction_args(app, settings, instructions) {
        Some(instruction_args) => (
            opening.clone(),
            instruction_args.into_iter().chain(args).collect(),
        ),
        None => (
            templates::chat_first_message(instructions, opening.as_deref()),
            args.collect(),
        ),
    }
}

/// Report the `chat` session `launch` started, with what `delivered` says
/// of its first message.
fn report_chat(
    ctx: &Context,
    app: &dyn App,
    repository: Option<&Repository>,
    chat: &Chat,
    launch: &DetachedLaunch,
    delivered: Delivered,
) -> Result<()> {
    let log = &launch.log;
    let report = DetachedChat {
        chat: chat.clone(),
        execution_id: launch.execution_id.clone(),
        pid: launch.pid,
        log: log.to_owned(),
        delivery: delivered.fields,
    };
    ctx.emit(
        &format!(
            "Started {} {} chat {} for {} (pid {}); open it in {}\nOutput: {}{}",
            app.name(),
            app.agent(),
            chat.name,
            repository.map_or("every repository", crate::repository::name),
            launch.pid,
            app.place(),
            log.display(),
            delivered.note,
        ),
        report,
    )
}

#[cfg(test)]
mod tests;
