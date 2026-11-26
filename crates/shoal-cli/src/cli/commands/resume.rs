//! `shoal resume`. Each space's daemon decides and carries out its resume, one
//! request per space, which never waits for an offline machine. What needs this
//! terminal stays here: `--focus` resumes one agent in it.
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

pub struct Options {
    pub execution: Option<ExecutionId>,
    pub now: bool,
    pub focus: bool,
    /// What changes in the permissions the resumed agents were given;
    /// without it they keep them.
    pub change: Option<PermissionChange>,
}

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

struct Resuming {
    spaces: Vec<Target>,
    chosen: Chosen,
    skipped: Vec<Skipped>,
    reopened: Vec<Reopened>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Chosen {
    /// The command line named them, or the user picked them, so one with
    /// nothing to resume fails.
    Named,
    All,
    Reopened,
}

async fn resume(ctx: &Context, resuming: Resuming, options: Options) -> Result<i32> {
    ensure_resumable(ctx, &resuming, &options).await?;
    if options.focus && !crate::process::scoped::inherits_scope(&ctx.paths.state) {
        return focus::here(ctx, resuming, options).await;
    }
    let resume = agent_resume(&resuming, options);
    let report = outcomes::resume_each(ctx, resuming, &resume).await?;
    reports::resumed(ctx, &report)
}

/// `--execution` and `--focus` serve only a space of this machine, whose daemon
/// and terminal they need. Another machine checks the permissions given to its
/// own spaces.
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

/// A scoped process's `--focus` starts at once in the background. A space named
/// alone continues its last session at once.
fn agent_resume(resuming: &Resuming, options: Options) -> AgentResume {
    AgentResume {
        executions: options.execution.into_iter().collect(),
        now: options.now || options.focus,
        alone: resuming.chosen == Chosen::Named && resuming.spaces.len() == 1,
        change: options.change,
        env: crate::terminal::start_environment(),
    }
}

/// A scoped process only queues the restores of the agents of the spaces its
/// scope reaches, which the daemon starts.
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
