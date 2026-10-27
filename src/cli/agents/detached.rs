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
    execution,
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

    /// Whether the agent takes instructions apart from its first message,
    /// as the arguments it is configured with.
    fn takes_instruction_args(&self) -> bool;

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
    let (prompt, args) = workspace_input(integration, app, &settings, &workspace, prompt, args);
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
    let launched = execution::launch_detached(
        &ctx.paths,
        &workspace,
        files.with_extension("log"),
        prepared.command(),
        app.not_inherited(),
        &prepared.env(),
        Some(&app.execution_agent()),
    )
    .await;
    let launch = match launched {
        Ok(launch) => launch,
        Err(error) => {
            prepared.discard().await;
            return Err(error);
        }
    };
    let delivered = prepared.deliver().await;
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
    )?;
    Ok(0)
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
/// without Shoal's hooks, as a tracked launch separates them.
fn workspace_input(
    integration: &dyn Integration,
    app: &dyn App,
    settings: &Effective,
    workspace: &Workspace,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> (Option<String>, Vec<OsString>) {
    let instructions = templates::instructions(
        settings.worker_template.as_deref(),
        workspace,
        super::launch::delivery(integration, settings),
    );
    let instruction_args = instruction_args(app, settings, &instructions);
    let (instruction_args, prompt) =
        super::launch::separate_instructions(instruction_args, instructions, prompt);
    let prompt = Some(prompt).filter(|prompt| !prompt.is_empty());
    (prompt, instruction_args.into_iter().chain(args).collect())
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
        .filter(|_| app.takes_instruction_args())
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
    let (prompt, args) = match instruction_args(app, settings, instructions) {
        Some(instruction_args) => (
            opening.clone(),
            instruction_args.into_iter().chain(args.clone()).collect(),
        ),
        None => (
            templates::chat_first_message(instructions, opening.as_deref()),
            args.clone(),
        ),
    };
    if let Some(trust) = integration.trust_file() {
        trust
            .trust_checkout(ctx, repository, &Default::default())
            .await;
    }
    // This process holds the chat until the session runs.
    let record = client::start_chat(&ctx.paths, repository, chat_name, name).await?;
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
    let log = files.with_extension("log");
    let spawned = spawn_session(ctx, app, repository, &record, &log, prepared.as_ref());
    let handed_over = match spawned {
        Ok(session) => hand_over(ctx, &record, session).await,
        Err(error) => Err(error),
    };
    let record = match handed_over {
        Ok(moved) => moved,
        Err(error) => {
            prepared.discard().await;
            return Err(error);
        }
    };
    let delivered = prepared.deliver().await;
    let mut report = json!({
        "chat": record,
        "log": log,
    });
    report_fields(&mut report, delivered.fields);
    ctx.emit(
        &format!(
            "Started {} {} chat {} for {} (pid {}); open it in {}\nOutput: {}{}",
            app.name(),
            app.agent(),
            record.name,
            crate::repository::name(repository),
            record.pid,
            app.place(),
            log.display(),
            delivered.note,
        ),
        report,
    )?;
    Ok(0)
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
