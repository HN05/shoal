//! `shoal contribute`.
use anyhow::Result;

use crate::{
    agent::Agent,
    agents,
    cli::{
        args::create::{ContributeArgs, StartArgs},
        context::Context,
        output::Palette,
        ui,
    },
    client::repositories::{self, ContributionHold},
    config::{Config, modes::ModeName, repo::ConfigLayers},
    git,
    model::Repository,
};

pub(super) async fn contribute(ctx: &Context, args: ContributeArgs) -> Result<i32> {
    let ContributeArgs {
        here,
        focus,
        wait,
        target,
        branch,
        agent,
        now,
        model,
        args,
    } = args;
    super::start::ensure_scope_token(ctx)?;
    ensure_agent_available(ctx, agent.as_ref())?;
    let source = match target.linked_repository() {
        Some(linked) => linked.repository_url(),
        None => target.to_string(),
    };
    // Held until the command ends, so cleanup cannot remove the clone while
    // a picker is open.
    let (repo, _hold) = register(ctx, source).await?;
    let start = StartArgs {
        here,
        focus,
        wait,
        now,
        agent,
        model,
        args,
        branch,
        repository: target.linked_repository().is_some().then_some(target),
        repository_override: Some(repo.id.to_string()),
        mode: Some(ModeName::DEFAULT),
        ..Default::default()
    };
    super::start::run(ctx, start).await
}

/// Refuse an unavailable agent before registration, which may fork and clone.
/// Only the global settings can name it before the clone exists; `start` checks
/// it again against the repository's.
fn ensure_agent_available(ctx: &Context, agent: Option<&Agent>) -> Result<()> {
    let Some(agent) = agent else {
        return Ok(());
    };
    let settings = Config::load(&ctx.paths)?.resolve(&ConfigLayers::default())?;
    agents::ensure_available(agent, &settings)
}

/// A repository registered otherwise needs no hold.
async fn register(ctx: &Context, source: String) -> Result<(Repository, Option<ContributionHold>)> {
    let (repo, hold) = ctx
        .progress(
            "Registering contribution",
            repositories::register_contribution(&ctx.paths, source),
        )
        .await?;
    let label = ui::repository_label(&repo, Palette::stderr(ctx.json));
    if !repo.contribution {
        eprintln!("Using registered repository {label}; it stays registered");
        return Ok((repo, None));
    }
    match git::remote_url(&repo.path, git::FORK_REMOTE).await {
        Ok(fork) => eprintln!(
            "Registered contribution {label}; branches push to your fork {}",
            fork.trim()
        ),
        Err(_) => eprintln!("Registered contribution {label}; branches push to it directly"),
    }
    Ok((repo, Some(hold)))
}
