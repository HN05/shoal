//! `shoal resume`: bringing back the agents of the selected spaces,
//! workspaces and chats alike, on this machine or another joined machine.
//! Each space's daemon decides and carries out its resume, one request per
//! space, which never waits for an offline machine: it continues a turn an interrupt
//! ended, queues a launch that failed to start again, or queues the
//! restores of the stopped agents and last sessions, starting them at once
//! with `--now`; a workspace whose setup failed is `shoal setup`'s. What
//! needs this terminal stays here: `--focus` resumes one agent in it.
use crate::client::output::agents::Skipped;
use crate::client::output::restore::Reopened;
use crate::{
    cli::{args::SpaceTargets, context::Context},
    config::permissions::PermissionChange,
};
use anyhow::{Result, bail, ensure};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::model::AgentResume;
use targets::{Selected, Taking, Target};

/// How `shoal resume` brings back the agents of the spaces it selects.
pub struct Options {
    /// The one stopped execution of a space to resume.
    pub execution: Option<ExecutionId>,
    /// Start the agents at once, regardless of the agent limits.
    pub now: bool,
    /// Resume one agent in this terminal.
    pub focus: bool,
    /// What changes in the permissions the resumed agents were given;
    /// without it they keep them.
    pub change: Option<PermissionChange>,
}

/// `shoal resume`: bring back the agents of the spaces `targets` selects,
/// once each, as `options` says; without a terminal or anything to pick,
/// the spaces whose agent can resume are listed instead.
pub(super) async fn run(ctx: &Context, targets: SpaceTargets, options: Options) -> Result<i32> {
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        ensure_queued()?;
    }
    let Some(selected) = Selected::load(ctx, targets, Taking::Resumes).await? else {
        return Ok(0);
    };
    let (spaces, chosen, skipped) = match selected {
        Selected::All => (
            Taking::Resumes.in_reach(ctx).await?,
            Chosen::All,
            Vec::new(),
        ),
        Selected::Named(spaces) => (spaces, Chosen::Named, Vec::new()),
        Selected::OnMachines { spaces, skipped } => (spaces, Chosen::All, skipped),
    };
    let resuming = Resuming {
        spaces,
        chosen,
        skipped,
        reopened: Vec::new(),
    };
    resume(ctx, resuming, options).await
}

/// Resume the agents of the spaces `restore` opened again, the `reopened`
/// ones whose removal saved any, as `options` says, reporting the reopened
/// spaces with them.
pub(in crate::cli::commands) async fn reopened(
    ctx: &Context,
    mut reopened: Vec<Reopened>,
    options: Options,
) -> Result<i32> {
    let mut spaces = Vec::new();
    for opened in reopened.iter_mut().filter(|opened| opened.agents_saved) {
        spaces.push(Target::here(&opened.space));
        opened.agents_saved = false;
    }
    let resuming = Resuming {
        spaces,
        chosen: Chosen::Reopened,
        skipped: Vec::new(),
        reopened,
    };
    resume(ctx, resuming, options).await
}

/// The spaces a resume acts on, and what it reports with them.
struct Resuming {
    spaces: Vec<Target>,
    chosen: Chosen,
    /// The spaces it leaves, as those of an offline machine.
    skipped: Vec<Skipped>,
    /// The removed spaces `shoal restore` opened again for it.
    reopened: Vec<Reopened>,
}

/// How a resume chose its spaces.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Chosen {
    /// The command line named them, or the user picked them, so one with
    /// nothing to resume fails.
    Named,
    /// `--all` or `--machine`, every space with something to resume.
    All,
    /// `shoal restore` opened them again with the agents their removal
    /// saved.
    Reopened,
}

/// Resume the agents of the spaces `resuming` lists as `options` says: one
/// in this terminal for `--focus`, otherwise each through its daemon, once
/// nothing refuses the request.
async fn resume(ctx: &Context, resuming: Resuming, options: Options) -> Result<i32> {
    ensure_resumable(ctx, &resuming, &options).await?;
    if options.focus && !crate::process::scoped::inherits_scope(&ctx.paths.state) {
        return focus::here(ctx, resuming, options).await;
    }
    let resume = agent_resume(&resuming, options);
    let report = outcomes::resume_each(ctx, resuming, &resume).await?;
    reports::resumed(ctx, &report)
}

/// Refuse `options` for the spaces `resuming` lists before any resumes:
/// `--execution` beyond one space, `--execution` and `--focus` for a space
/// of another machine, which only this machine's daemon and terminal serve,
/// and a change to the permissions given that this process does not hold
/// for one of this machine's; another machine checks its own.
async fn ensure_resumable(ctx: &Context, resuming: &Resuming, options: &Options) -> Result<()> {
    ensure!(
        options.execution.is_none() || resuming.spaces.len() <= 1,
        "--execution selects a stopped execution of one space"
    );
    let elsewhere = resuming.spaces.iter().find(|space| space.machine.is_some());
    if let Some(space) = elsewhere {
        let refused = match (options.focus, &options.execution) {
            (true, _) => Some("--focus resumes an agent in this terminal"),
            (false, Some(_)) => Some("--execution names an execution of this machine"),
            (false, None) => None,
        };
        if let Some(refused) = refused {
            bail!(
                "{refused}, and {} runs on another machine; leave it out",
                space.shown()
            );
        }
    }
    if let Some(change) = &options.change {
        let all = resuming.chosen == Chosen::All;
        let here: Vec<_> = (resuming.spaces.iter())
            .filter(|space| space.machine.is_none())
            .map(|space| space.selector.clone())
            .collect();
        permission_change::check(ctx, &here, change, all).await?;
    }
    Ok(())
}

/// The request to resume each space's agent of `resuming` as `options`
/// say, which a scoped process's `--focus` starts at once in the
/// background; its restores open on the host serving this terminal, and a
/// space named alone continues its last session at once.
fn agent_resume(resuming: &Resuming, options: Options) -> AgentResume {
    AgentResume {
        executions: options.execution.into_iter().collect(),
        now: options.now || options.focus,
        alone: resuming.chosen == Chosen::Named && resuming.spaces.len() == 1,
        change: options.change,
        env: crate::terminal::start_environment(),
    }
}

/// Refuse a resume in a process below a scoped one that cleared its scope
/// token: a scoped process only queues the restores of the agents of the
/// spaces its scope reaches, which the daemon starts, or discards their
/// stopped agents.
pub(in crate::cli::commands) fn ensure_queued() -> Result<()> {
    // The daemon checks that each space is in the caller's scope, which
    // needs the scope token: a process that cleared it stays refused.
    ensure!(
        crate::env::is_scoped(),
        "scoped processes queue the restores of the spaces their scope reaches only with their scope token"
    );
    Ok(())
}

pub(in crate::cli::commands) mod changed;
mod failed_starts;
mod focus;
mod outcomes;
mod permission_change;
pub(in crate::cli::commands) mod queue;
pub(in crate::cli::commands) mod queued;
pub(in crate::cli::commands) mod reports;
mod space;
pub(in crate::cli::commands) mod targets;
