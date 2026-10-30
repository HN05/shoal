//! Agent sessions that run detached from any terminal, in an app the user
//! continues them in, such as Happy. Shoal records and starts each session;
//! the [`App`] says what it runs and delivers its first message.
use std::{
    ffi::OsString,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Stdio},
};

use anyhow::{Context as _, Result};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::{ChatLaunch, Integration, Launch};
use crate::{
    cli::{client, context::Context},
    config::{Effective, templates},
    execution::{self, DetachedLaunch},
    model::{Chat, Repository, Workspace},
    paths::Paths,
};

/// An app that runs agent sessions apart from any terminal.
pub(in crate::cli) trait App: Sync {
    /// The app's name in output.
    fn name(&self) -> &'static str;

    /// The agent the app runs.
    fn agent(&self) -> &'static str;

    /// What a workspace session's execution records as its agent.
    fn execution_agent(&self) -> String;

    /// Where its sessions run, for a user looking for one.
    fn place(&self) -> &'static str;

    /// Whether the agent takes the arguments it is configured with, such as
    /// its instructions apart from its first message and other working
    /// directories.
    fn takes_agent_args(&self) -> bool;

    /// Variables a session must not inherit from this process.
    fn not_inherited(&self) -> &'static [&'static str];

    /// What `session` runs and how its first message reaches it.
    fn prepare<'a>(&'a self, session: Session<'a>) -> BoxFuture<'a, Result<Box<dyn Prepared>>>;
}

/// A session an [`App`] is to run, as Shoal sets it up. Only optional
/// integrations read it.
#[cfg_attr(not(feature = "happy"), allow(dead_code))]
pub(in crate::cli) struct Session<'a> {
    pub paths: &'a Paths,
    /// Where the session runs.
    pub directory: &'a Path,
    /// The branch checked out there, for a workspace's session.
    pub branch: Option<&'a str>,
    /// Where the session keeps its files, without an extension.
    pub files: &'a Path,
    /// The first message.
    pub prompt: Option<String>,
    pub args: Vec<OsString>,
}

/// A session an [`App`] prepared.
pub(in crate::cli) trait Prepared: Send + Sync {
    /// The command the session runs.
    fn command(&self) -> Vec<OsString>;

    /// Variables the session runs with.
    fn env(&self) -> Vec<(&'static str, String)>;

    /// Undo the preparation of a session that never started.
    fn discard(self: Box<Self>) -> BoxFuture<'static, ()>;

    /// Deliver the first message to the started session.
    fn deliver(self: Box<Self>) -> BoxFuture<'static, Delivered>;
}

/// What output says of a first message once the session started.
pub(in crate::cli) struct Delivered {
    /// The line the text ends with, if any.
    pub note: &'static str,
    /// The object whose fields the JSON adds.
    pub fields: Value,
}

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
        prompt,
        args,
        ..
    } = launch;
    super::ensure_available(&name, &settings)?;
    let directories = workspace_dir_args(ctx, app, &settings, &workspace).await;
    let (prompt, args) = workspace_input(
        integration,
        app,
        &settings,
        &workspace,
        directories,
        prompt,
        args,
    );
    if let Some(trust) = integration.trust_file() {
        trust
            .trust_tracked(ctx, &workspace, &Default::default())
            .await;
    }
    let files = session_files(&ctx.paths, &workspace, &name);
    let session = Session {
        paths: &ctx.paths,
        directory: &workspace.path,
        branch: Some(&workspace.branch),
        files: &files,
        prompt,
        args,
    };
    let prepared = app.prepare(session).await?;
    let (launch, prepared) = launch_session(ctx, app, &workspace, &files, prepared).await?;
    let delivered = prepared.deliver().await;
    report_session(ctx, app, &workspace, &launch, delivered)?;
    Ok(0)
}

/// Launch the `prepared` session of `app` in `workspace` as a detached
/// execution logging beside its other `files`, discarding the preparation
/// when it cannot start.
async fn launch_session(
    ctx: &Context,
    app: &dyn App,
    workspace: &Workspace,
    files: &Path,
    prepared: Box<dyn Prepared>,
) -> Result<(DetachedLaunch, Box<dyn Prepared>)> {
    let launched = execution::launch_detached(
        &ctx.paths,
        workspace,
        files.with_extension(crate::paths::SESSION_LOG),
        prepared.command(),
        app.not_inherited(),
        &prepared.env(),
        Some(&app.execution_agent()),
    )
    .await;
    match launched {
        Ok(launch) => Ok((launch, prepared)),
        Err(error) => {
            prepared.discard().await;
            Err(error)
        }
    }
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
    let mut report = json!({
        "workspace": workspace,
        "agent": app.agent(),
        "execution_id": launch.execution_id,
        "pid": launch.pid,
        "log": launch.log,
    });
    report_fields(&mut report, delivered.fields);
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
fn session_files(paths: &Paths, workspace: &Workspace, name: &str) -> PathBuf {
    let stamp_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    paths
        .workspace_state(&workspace.id)
        .join(format!("{name}-{stamp_millis}"))
}

/// The first message and arguments of a workspace session, which runs
/// without Shoal's hooks, as a tracked launch separates them, with the
/// arguments that give it `directories`.
fn workspace_input(
    integration: &dyn Integration,
    app: &dyn App,
    settings: &Effective,
    workspace: &Workspace,
    directories: Vec<OsString>,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> (Option<String>, Vec<OsString>) {
    let instructions = templates::instructions(
        settings.agent.templates.worker.as_deref(),
        workspace,
        super::launch::delivery(integration, settings),
    );
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

/// The arguments that give `app`'s agent `instructions` apart from its first
/// message, when it takes them.
fn instruction_args(
    app: &dyn App,
    settings: &Effective,
    instructions: &str,
) -> Option<Vec<OsString>> {
    settings
        .launch(app.agent())
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
        super::workspace_dir_args(&ctx.paths, settings, app.agent(), workspace).await
    } else {
        Vec::new()
    }
}

/// The arguments that give `app`'s agent the configured agent directories
/// and `chat`'s temporary directory, when it takes arguments.
fn chat_dir_args(app: &dyn App, settings: &Effective, home: &Path, chat: &Chat) -> Vec<OsString> {
    if app.takes_agent_args() {
        super::chat_dir_args(settings, app.agent(), home, chat)
    } else {
        Vec::new()
    }
}

/// Add the fields of an app's object `fields` to the JSON `report`.
fn report_fields(report: &mut Value, fields: Value) {
    if let (Value::Object(report), Value::Object(fields)) = (report, fields) {
        report.extend(fields);
    }
}

/// Start `chat` as a session of `app`, agent `name`, in the repository's
/// checkout, detached as workspace sessions are, since the user continues
/// it in the app. It is no tracked execution: the chat lasts while the
/// session's process runs.
pub(super) async fn start_chat(
    ctx: &Context,
    integration: &dyn Integration,
    app: &dyn App,
    name: &str,
    chat: &ChatLaunch,
) -> Result<i32> {
    let ChatLaunch {
        repository,
        name: chat_name,
        settings,
        instructions,
        opening,
        args,
    } = chat;
    super::ensure_available(name, settings)?;
    if let Some(trust) = integration.trust_file() {
        trust
            .trust_checkout(ctx, repository, &Default::default())
            .await;
    }
    // This process holds the chat until the session runs, which is not in
    // this terminal.
    let record = client::start_chat(&ctx.paths, repository, chat_name, name, None, true).await?;
    let (prompt, args) = chat_input(
        app,
        settings,
        &ctx.paths.home,
        &record,
        instructions,
        opening,
        args,
    );
    let files = ctx.paths.chat_files(&record);
    let session = Session {
        paths: &ctx.paths,
        directory: &repository.path,
        branch: None,
        files: &files,
        prompt,
        args,
    };
    let prepared = app.prepare(session).await?;
    let log = files.with_extension(crate::paths::SESSION_LOG);
    let (record, prepared) = hand_to_session(ctx, app, repository, &record, &log, prepared).await?;
    let delivered = prepared.deliver().await;
    report_chat(ctx, app, repository, &record, &log, delivered)?;
    Ok(0)
}

/// The first message and arguments of a chat session: the instructions as
/// arguments with the chat's `opening`, where the agent takes them apart,
/// otherwise as the first message, which starts a turn before the user
/// speaks. The agent directories and `chat`'s temporary directory come
/// before the chat's own arguments.
fn chat_input(
    app: &dyn App,
    settings: &Effective,
    home: &Path,
    chat: &Chat,
    instructions: &str,
    opening: &Option<String>,
    args: &[OsString],
) -> (Option<String>, Vec<OsString>) {
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

/// Start the `prepared` session of `app` for `chat`, logging to `log`, and
/// hand the chat to it, discarding the preparation when either fails.
/// Returns the chat as the daemon now records it.
async fn hand_to_session(
    ctx: &Context,
    app: &dyn App,
    repository: &Repository,
    chat: &Chat,
    log: &Path,
    prepared: Box<dyn Prepared>,
) -> Result<(Chat, Box<dyn Prepared>)> {
    let spawned = spawn_session(ctx, app, repository, chat, log, prepared.as_ref());
    let handed_over = match spawned {
        Ok(session) => hand_over(ctx, chat, session).await,
        Err(error) => Err(error),
    };
    match handed_over {
        Ok(moved) => Ok((moved, prepared)),
        Err(error) => {
            prepared.discard().await;
            Err(error)
        }
    }
}

/// Report the `chat` session started with its output in `log`, with what
/// `delivered` says of its first message.
fn report_chat(
    ctx: &Context,
    app: &dyn App,
    repository: &Repository,
    chat: &Chat,
    log: &Path,
    delivered: Delivered,
) -> Result<()> {
    let mut report = json!({
        "chat": chat,
        "log": log,
    });
    report_fields(&mut report, delivered.fields);
    ctx.emit(
        &format!(
            "Started {} {} chat {} for {} (pid {}); open it in {}\nOutput: {}{}",
            app.name(),
            app.agent(),
            chat.name,
            crate::repository::name(repository),
            chat.pid,
            app.place(),
            log.display(),
            delivered.note,
        ),
        report,
    )
}

/// Hand `chat` to `session`'s process, stopping the session when the daemon
/// cannot record it, since the chat would end with this process.
async fn hand_over(ctx: &Context, chat: &Chat, session: Child) -> Result<Chat> {
    let moved = client::move_chat(&ctx.paths, chat, session.id()).await;
    if moved.is_err() {
        stop_session(session);
    }
    moved
}

/// Stop a session started by [`spawn_session`] with the agent it started:
/// the session leads its own process group.
fn stop_session(mut session: Child) {
    // SAFETY: killpg takes no pointers. The leader is not reaped yet, so its
    // ID still names this group alone.
    unsafe { libc::killpg(session.id() as libc::pid_t, libc::SIGKILL) };
    let _ = session.wait();
}

/// Start the `prepared` session of `app` in a session of its own at the
/// checkout with the chat's identity, writing to `log`.
fn spawn_session(
    ctx: &Context,
    app: &dyn App,
    repository: &Repository,
    chat: &Chat,
    log: &Path,
    prepared: &dyn Prepared,
) -> Result<Child> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let output = std::fs::File::create(log).with_context(|| format!("create {}", log.display()))?;
    let command = prepared.command();
    let mut session = std::process::Command::new(&command[0]);
    session
        .args(&command[1..])
        .current_dir(&repository.path)
        .envs(crate::env::chat_environment(chat, &ctx.paths))
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output);
    for name in app.not_inherited() {
        session.env_remove(name);
    }
    session.envs(prepared.env());
    // SAFETY: setsid detaches the forked child without allocating, before exec.
    unsafe {
        session.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    session
        .spawn()
        .with_context(|| format!("start the {} session", app.name()))
}

#[cfg(test)]
mod tests;
