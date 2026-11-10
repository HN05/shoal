//! `shoal contribute`: a workspace in someone else's repository for a PR,
//! pushed to your fork when you cannot push to the repository, whose clone
//! Shoal removes once nothing of it is left.
use anyhow::Result;

use super::workspaces::{
    AddOptions,
    link::{self, AddInput},
};
use crate::{
    agent::Agent,
    agents,
    cli::{args::create::ContributeArgs, context::Context, output::Palette, ui},
    config::{Config, repo::ConfigLayers},
    forge::items::link::Link,
    git,
    model::Repository,
    start_plan::Creation,
};

/// Register the repository a repository URL, or an issue, PR or branch link
/// names, then add the workspace as `shoal start` would.
pub(super) async fn contribute(ctx: &Context, args: ContributeArgs) -> Result<i32> {
    let ContributeArgs {
        here,
        focus,
        target,
        branch,
        agent,
        now,
        model,
        args,
    } = args;
    let options = AddOptions {
        here,
        focus,
        now,
        after: Vec::new(),
        model: model.into(),
        ..Default::default()
    };
    ensure_agent_available(ctx, agent.as_ref())?;
    let link = Link::parse(&target).ok();
    let source = match &link {
        Some(link) => link.repository.repository_url(),
        None => target.clone(),
    };
    let repo = register(ctx, source).await?;
    let input = match link {
        Some(_) => AddInput {
            positional: Some(target),
            repository: Some(repo.id.into()),
            issue: None,
        },
        None => AddInput {
            positional: None,
            repository: Some(repo.id.into()),
            issue: None,
        },
    };
    let creation = Creation {
        branch,
        ..Default::default()
    };
    link::add(ctx, input, creation, agent, args, options).await
}

/// Refuse an unavailable agent before registration, which may fork and clone.
/// Only the global settings can name it before the repository is cloned;
/// `start` checks it again against the repository's.
fn ensure_agent_available(ctx: &Context, agent: Option<&Agent>) -> Result<()> {
    let Some(agent) = agent else {
        return Ok(());
    };
    let settings = Config::load(&ctx.paths)?.resolve(&ConfigLayers::default())?;
    agents::ensure_available(agent, &settings)
}

/// Register `source` as a contribution and say where its pushes go.
async fn register(ctx: &Context, source: String) -> Result<Repository> {
    let repo = ctx
        .progress(
            "Registering contribution",
            crate::client::repositories::register_contribution(&ctx.paths, source),
        )
        .await?;
    let label = ui::repository_label(&repo, Palette::stderr(ctx.json));
    if !repo.contribution {
        eprintln!("Using registered repository {label}; it stays registered");
        return Ok(repo);
    }
    match git::remote_url(&repo.path, git::FORK_REMOTE).await {
        Ok(fork) => eprintln!(
            "Registered contribution {label}; branches push to your fork {}",
            fork.trim()
        ),
        Err(_) => eprintln!("Registered contribution {label}; branches push to it directly"),
    }
    Ok(repo)
}
