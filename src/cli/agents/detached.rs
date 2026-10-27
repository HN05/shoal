//! Agent sessions that run detached from any terminal, in an app the user
//! continues them in, such as Happy.
use std::{
    ffi::OsString,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Stdio},
};

use anyhow::{Context as _, Result};
use serde_json::json;

use super::{
    ChatLaunch, Integration,
    happy::{Happy, Opening, instruction_args, session_input, warn_without_daemon},
};
use crate::{
    cli::{client, context::Context},
    config::{Effective, templates},
    execution, happy,
    model::{Chat, Repository, Workspace},
};

/// Start a Happy session the way Happy's own daemon does, so it registers with
/// that daemon and appears in the app, but detached from this terminal and
/// tracked like any other workspace command. The CLI returns once the launch
/// is recorded; the session's output goes to a log under Shoal's state.
pub(super) async fn start(
    ctx: &Context,
    happy: &Happy,
    workspace: &Workspace,
    settings: &Effective,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let agent = happy.0;
    super::ensure_available(&format!("happy-{}", agent.as_str()), settings)?;
    let (prompt, args) = session_input(settings, workspace, agent, prompt, args);
    if let Some(trust) = happy.trust_file() {
        trust
            .trust_tracked(ctx, workspace, &Default::default())
            .await;
    }
    let daemon_recorded = warn_without_daemon(ctx);
    let stamp_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let files = ctx
        .paths
        .workspace_state(&workspace.id)
        .join(format!("happy-{}-{stamp_millis}", agent.as_str()));
    let opening = Opening::prepare(
        ctx,
        agent,
        &workspace.path,
        Some(&workspace.branch),
        &files,
        prompt.as_deref(),
    )
    .await?;
    let command = happy::command(agent, prompt.as_deref(), args);
    let launch = match execution::launch_detached(
        &ctx.paths,
        workspace,
        files.with_extension("log"),
        command,
        &happy::client::RECONNECT_ENV,
        &opening.env(),
        Some(&format!("happy {}", agent.as_str())),
    )
    .await
    {
        Ok(launch) => launch,
        Err(error) => {
            opening.discard().await;
            return Err(error);
        }
    };
    let prompt_delivered = opening.deliver(prompt.as_deref()).await;
    ctx.emit(
        &format!(
            "Started Happy {} session in {} (execution {}, pid {})\nOutput: {}{}",
            agent.as_str(),
            workspace.name,
            launch.execution_id,
            launch.pid,
            launch.log.display(),
            opening.delivery(prompt_delivered),
        ),
        json!({
            "workspace": workspace,
            "agent": agent,
            "execution_id": launch.execution_id,
            "pid": launch.pid,
            "log": launch.log,
            "prompt_file": opening.prompt_file,
            "prompt_delivered": prompt_delivered,
            "happy_session_id": opening.seeded.as_ref().map(|seeded| seeded.session.id.clone()),
            "happy_daemon_recorded": daemon_recorded,
        }),
    )?;
    Ok(0)
}

/// Start a chat as a Happy session in the repository's checkout, detached as
/// workspace sessions are, since the user continues it in the Happy app. It is
/// no tracked execution: the chat lasts while the session's process runs.
pub(super) async fn start_chat(ctx: &Context, happy: &Happy, chat: &ChatLaunch) -> Result<i32> {
    let agent = happy.0;
    let name = format!("happy-{}", agent.as_str());
    let ChatLaunch {
        repository,
        name: chat_name,
        settings,
        instructions,
        opening,
        args,
    } = chat;
    super::ensure_available(&name, settings)?;
    let (prompt, args) = match instruction_args(settings, agent, instructions) {
        Some(instruction_args) => (
            opening.clone(),
            instruction_args.into_iter().chain(args.clone()).collect(),
        ),
        None => (
            templates::chat_first_message(instructions, opening.as_deref()),
            args.clone(),
        ),
    };
    if let Some(trust) = happy.trust_file() {
        trust
            .trust_checkout(ctx, repository, &Default::default())
            .await;
    }
    let daemon_recorded = warn_without_daemon(ctx);
    // This process holds the chat until the session runs.
    let record = client::start_chat(&ctx.paths, repository, chat_name, &name).await?;
    let files = ctx.paths.chat_files(&record);
    let opening = Opening::prepare(
        ctx,
        agent,
        &repository.path,
        None,
        &files,
        prompt.as_deref(),
    )
    .await?;
    let command = happy::command(agent, prompt.as_deref(), args);
    let log = files.with_extension("log");
    let started = spawn_session(ctx, repository, &record, &log, command, &opening.env());
    let handed_over = match started {
        Ok(session) => hand_over(ctx, &record, session).await,
        Err(error) => Err(error),
    };
    let record = match handed_over {
        Ok(moved) => moved,
        Err(error) => {
            opening.discard().await;
            return Err(error);
        }
    };
    let prompt_delivered = opening.deliver(prompt.as_deref()).await;
    ctx.emit(
        &format!(
            "Started Happy {} chat {} for {} (pid {}); open it in the Happy app\nOutput: {}{}",
            agent.as_str(),
            record.name,
            crate::repository::name(repository),
            record.pid,
            log.display(),
            opening.delivery(prompt_delivered),
        ),
        json!({
            "chat": record,
            "log": log,
            "prompt_file": opening.prompt_file,
            "prompt_delivered": prompt_delivered,
            "happy_session_id": opening.seeded.as_ref().map(|seeded| seeded.session.id.clone()),
            "happy_daemon_recorded": daemon_recorded,
        }),
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

/// Start `command` in its own session at the checkout with the chat's
/// identity and `env`, writing to `log`.
fn spawn_session(
    ctx: &Context,
    repository: &Repository,
    chat: &Chat,
    log: &Path,
    command: Vec<OsString>,
    env: &[(&str, String)],
) -> Result<Child> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let output = std::fs::File::create(log).with_context(|| format!("create {}", log.display()))?;
    let mut session = std::process::Command::new(&command[0]);
    session
        .args(&command[1..])
        .current_dir(&repository.path)
        .envs(crate::env::chat_environment(chat, &ctx.paths))
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output);
    // A launch from inside another Happy session must not attach to it.
    for name in happy::client::RECONNECT_ENV {
        session.env_remove(name);
    }
    session.envs(env.iter().map(|(name, value)| (*name, value)));
    // SAFETY: setsid detaches the forked child without allocating, before exec.
    unsafe {
        session.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    session.spawn().context("start the Happy session")
}

#[cfg(test)]
mod tests;
