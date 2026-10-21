//! Where Shoal runs work for the caller's terminal. Commands say what they
//! launch; this module chooses whether it takes the terminal or runs in the
//! background, and which backend carries that out with its own primitives:
//! Herdr tabs or zmx sessions. Commands never name a backend.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use super::{
    commands::{issues::Issue, workspaces::AddPlan},
    context::Context,
    herdr, zmx,
};
use crate::{
    config::Effective,
    env,
    model::{Repository, Workspace},
    tools::Tool,
};

/// Start `plan` beside this terminal: focused when it waits for the user,
/// otherwise in the background, unless the `focus` setting says otherwise.
/// Returns whether it left this process; a focused launch without a new tab
/// stays here, where its agent runs in a session this terminal attaches to.
/// `name` is the planned workspace name.
pub(in crate::cli) async fn start_add(
    ctx: &Context,
    settings: &Effective,
    plan: &mut AddPlan,
    issue: Option<&Issue>,
    name: &str,
) -> Result<bool> {
    let Some(backend) = Backend::current(ctx, settings) else {
        return Ok(false);
    };
    let placement = Placement::choose(settings.focus, !plan.runs_unattended());
    let started = backend
        .start_add(ctx, settings, plan, issue, name, placement)
        .await?;
    if started && placement == Placement::Background {
        println!("{}", backend.background_note(name));
    }
    Ok(started)
}

/// Start an admitted queue entry's `plan` in the background where the
/// terminal that queued it would have; the daemon's worker has no terminal.
pub(in crate::cli) async fn start_queued(
    ctx: &Context,
    settings: &Effective,
    plan: &mut AddPlan,
    name: &str,
) -> Result<()> {
    Backend::queued(settings)
        .start_add(ctx, settings, plan, None, name, Placement::Background)
        .await?;
    Ok(())
}

/// The variables a later start needs to reach this terminal's backend, such
/// as a queued start the daemon runs without a terminal.
pub(in crate::cli) fn environment() -> BTreeMap<String, String> {
    herdr::environment()
}

/// Where an agent running in this process sits, as its backend names it, so
/// a later resume can restore it there.
pub(crate) fn current_place() -> Option<String> {
    herdr::current_pane()
}

/// Run Shoal with `args` where the agent of `record` stopped, when that
/// place is another one still open and idle. Returns what such restarts run
/// in, or `None` when it did not run.
pub(in crate::cli) async fn rerun_where_stopped(
    ctx: &Context,
    settings: &Effective,
    record: &Path,
    args: &[OsString],
) -> Result<Option<&'static str>> {
    let reused = match Backend::current(ctx, settings) {
        Some(backend @ Backend::Herdr) => herdr::rerun_in_recorded_pane(ctx, record, args)
            .await?
            .then(|| backend.background_places()),
        // A session ends with its agent, so nothing remains to reuse.
        Some(Backend::Zmx) | None => None,
    };
    Ok(reused)
}

/// Run Shoal with `args` for `workspace` in the background, returning what
/// such starts run in, or `None` when this terminal cannot start them.
pub(in crate::cli) async fn run_in_background(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    args: &[OsString],
) -> Result<Option<&'static str>> {
    let Some(backend) = Backend::current(ctx, settings) else {
        return Ok(None);
    };
    let started = match backend {
        Backend::Herdr => herdr::run_in_background(ctx, settings, workspace, args).await?,
        Backend::Zmx => {
            zmx::run_in_background(ctx, workspace, args).await?;
            true
        }
    };
    Ok(started.then(|| backend.background_places()))
}

/// Run a tracked agent's `command` in a new session and attach this terminal
/// to it, when this terminal keeps agents in sessions. `None` means the agent
/// runs in place in this process. `records` are recovery records the launch
/// consumes once it starts.
pub(in crate::cli) async fn run_agent(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    agent: &str,
    records: &[PathBuf],
    command: &[OsString],
) -> Result<Option<i32>> {
    if !keeps_agents_in_sessions(settings) {
        return Ok(None);
    }
    zmx::run(ctx, workspace, agent, records, command)
        .await
        .map(Some)
}

/// Run Shoal with `args` for a chat of `agent` at the repository's checkout:
/// in a new focused tab, or in a new session this terminal attaches to, as
/// `run_agent` does for tracked agents. Returns whether it ran there; `false`
/// means the chat runs in place.
pub(in crate::cli) async fn start_chat(
    ctx: &Context,
    settings: &Effective,
    repository: &Repository,
    agent: &str,
    label: &str,
    args: &[OsString],
) -> Result<bool> {
    match Backend::current(ctx, settings) {
        Some(Backend::Herdr) if settings.herdr.new_tab => {
            herdr::run_in_tab(ctx, &repository.path, label, args, true).await?;
            Ok(true)
        }
        _ if keeps_agents_in_sessions(settings) => {
            zmx::run_chat(ctx, repository, agent, args).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The place a backend opened for this process to add a workspace in,
/// hosting it in place of the caller's shell.
pub(in crate::cli) enum Place {
    Tab(herdr::Tab),
    Session,
}

/// Tell the place this process runs in that it hosts `workspace`, whose
/// `agent` starts there: a tab takes `label` and closes with the workspace,
/// and a session is labelled for `shoal attach`. A session the user opened
/// is labelled once an agent starts in it.
pub(in crate::cli) async fn host_workspace(
    ctx: &Context,
    workspace: &Workspace,
    agent: Option<&str>,
    label: &str,
) -> Result<()> {
    let agent = match &ctx.place {
        Some(Place::Tab(tab)) => {
            tab.watch_workspace(ctx, &workspace.id)?;
            tab.rename(label).await;
            return Ok(());
        }
        Some(Place::Session) => agent.unwrap_or_default(),
        None => match agent {
            Some(agent) => agent,
            None => return Ok(()),
        },
    };
    if let Err(error) = label_current(&workspace.id, agent).await {
        eprintln!("warning: {error:#}");
    }
    Ok(())
}

/// Whether the shell that started this process enters a new workspace
/// afterwards: the caller's own or that of the tab opened for it. A session
/// ends with its agent, leaving no shell.
pub(in crate::cli) fn follows_caller(ctx: &Context) -> bool {
    !matches!(ctx.place, Some(Place::Session))
}

/// Give a workspace added without an agent a shell at `cwd` in the place
/// opened for it, which stays open while the shell runs. The caller's own
/// terminal gets its shell back instead.
pub(in crate::cli) async fn open_shell(ctx: &Context, cwd: &Path) -> Result<i32> {
    if ctx.place.is_none() {
        return Ok(0);
    }
    let shell = std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let status = tokio::process::Command::new(shell)
        .arg("-i")
        .current_dir(cwd)
        .env_remove(env::SHELL_DIRECTIVE)
        .status()
        .await
        .context("open workspace shell")?;
    Ok(env::session_exit_code(crate::execution::exit_code(status)))
}

/// Label the session this process runs in, when it runs in one, so
/// `shoal attach` finds the workspace's agent there.
pub(in crate::cli) async fn label_current(workspace_id: &str, agent: &str) -> Result<()> {
    zmx::label_current(workspace_id, agent).await
}

/// The program an agent launch here needs besides the agent's own.
pub(in crate::cli) fn launcher_program() -> Option<&'static str> {
    zmx::on_terminal().then(|| Tool::Zmx.program())
}

/// The session this process runs in, where an agent's wrapper can type a
/// prompt for it.
pub(crate) fn prompt_session() -> Option<OsString> {
    zmx::current_session()
}

/// Submit `text` as a prompt in `session`, from [`prompt_session`].
pub(crate) async fn submit_prompt(session: &std::ffi::OsStr, text: &str) -> Result<()> {
    zmx::submit_prompt(session, text).await
}

/// Whether an agent launched here runs in a session its wrapper can type a
/// prompt into: a new one, or the session this process already runs in.
pub(in crate::cli) fn agents_can_be_prompted(settings: &Effective) -> bool {
    keeps_agents_in_sessions(settings) || prompt_session().is_some()
}

/// Whether agents launched here run in a session: they need a terminal to
/// attach, never nest, and leave an enabled Herdr pane to Herdr, which reads
/// the agent's state from the process in the pane.
fn keeps_agents_in_sessions(settings: &Effective) -> bool {
    zmx::on_terminal() && !(settings.herdr.enabled && herdr::current_pane().is_some())
}

/// Whether a launch takes the caller's terminal or runs in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Placement {
    Focus,
    Background,
}

impl Placement {
    /// The `focus` setting, otherwise focus a launch that waits for the user.
    fn choose(setting: Option<bool>, waits_for_user: bool) -> Self {
        if setting.unwrap_or(waits_for_user) {
            Self::Focus
        } else {
            Self::Background
        }
    }
}

/// A terminal backend that starts Shoal work beside the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Herdr,
    Zmx,
}

impl Backend {
    /// The backend of this interactive terminal under `settings`: an enabled
    /// Herdr pane, otherwise a terminal outside zmx. A Herdr pane is never
    /// handed to zmx, since Herdr reads agent state from the pane's process.
    fn current(ctx: &Context, settings: &Effective) -> Option<Self> {
        if herdr::available(ctx) {
            return settings.herdr.enabled.then_some(Self::Herdr);
        }
        (ctx.interactive() && zmx::on_terminal()).then_some(Self::Zmx)
    }

    /// The backend of the terminal that queued an entry, from the Herdr
    /// variables the daemon gave its worker: a tab where Herdr may open one,
    /// otherwise a detached session.
    fn queued(settings: &Effective) -> Self {
        if herdr::in_herdr() && settings.herdr.enabled && settings.herdr.new_tab {
            Self::Herdr
        } else {
            Self::Zmx
        }
    }

    /// Start `plan` as `placement` asks, returning whether it left this
    /// process. A focused zmx launch stays here. Herdr tabs need
    /// `herdr.new_tab`; `name` names a session.
    async fn start_add(
        self,
        ctx: &Context,
        settings: &Effective,
        plan: &mut AddPlan,
        issue: Option<&Issue>,
        name: &str,
        placement: Placement,
    ) -> Result<bool> {
        ensure!(
            !env::inherits_scope(&ctx.paths.state),
            "workspace processes cannot allocate workspaces"
        );
        match (self, placement) {
            (Self::Herdr, placement) => {
                herdr::start_add(ctx, settings, plan, issue, placement).await
            }
            (Self::Zmx, Placement::Focus) => Ok(false),
            (Self::Zmx, Placement::Background) => {
                zmx::start_add(ctx, plan, name).await?;
                Ok(true)
            }
        }
    }

    /// What several background starts run in.
    fn background_places(self) -> &'static str {
        match self {
            Self::Herdr => "Herdr tabs",
            Self::Zmx => "detached zmx sessions",
        }
    }

    /// How a background start is reported: where it runs and how to reach it.
    fn background_note(self, name: &str) -> String {
        match self {
            Self::Herdr => format!("Started {name} in a background Herdr tab"),
            Self::Zmx => format!(
                "Started {name} in a detached zmx session; attach with `shoal attach {name}`"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_follows_the_setting_then_whether_the_launch_waits() {
        assert_eq!(Placement::choose(None, true), Placement::Focus);
        assert_eq!(Placement::choose(None, false), Placement::Background);
        assert_eq!(Placement::choose(Some(false), true), Placement::Background);
        assert_eq!(Placement::choose(Some(true), false), Placement::Focus);
    }
}
