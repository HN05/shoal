//! `shoal contribute`: a workspace in someone else's repository for a PR,
//! pushed to your fork when you cannot push to the repository, whose clone
//! Shoal removes once nothing of it is left.
use anyhow::Result;

use super::{
    links,
    workspaces::{AddOptions, Creation},
};
use crate::{
    agent::Agent,
    cli::{
        agents, args::create::ContributeArgs, client::request, context::Context, output::Palette,
        ui,
    },
    config::{Config, repo::ConfigLayers},
    forge::items::link::Link,
    git,
    model::Repository,
    protocol::RepositoryMethod,
};

/// Register the repository a repository URL, or an issue, PR or branch link
/// names, then add the workspace as `shoal add` would.
pub(super) async fn contribute(ctx: &Context, args: ContributeArgs) -> Result<i32> {
    let ContributeArgs {
        here,
        focus,
        target,
        branch,
        agent,
        now,
        args,
    } = args;
    let options = AddOptions {
        here,
        focus,
        now,
        after: Vec::new(),
    };
    ensure_agent_available(ctx, agent.as_ref())?;
    let link = Link::parse(&target).ok();
    let source = match &link {
        Some(link) => link.repository.repository_url(),
        None => target.clone(),
    };
    let repo = register(ctx, source).await?;
    let input = match link {
        Some(_) => links::AddInput {
            positional: Some(target),
            repository: Some(repo.id),
            issue: None,
        },
        None => links::AddInput {
            positional: Some(repo.id),
            repository: None,
            issue: None,
        },
    };
    let creation = Creation {
        path: None,
        branch,
        existing: None,
        base: None,
        git_profile: None,
        pr: None,
    };
    links::add(ctx, input, creation, agent, args, options).await
}

/// Refuse an unavailable agent before registration, which may fork and clone.
/// Only the global settings can name it before the repository is cloned;
/// `add` checks it again against the repository's.
fn ensure_agent_available(ctx: &Context, agent: Option<&Agent>) -> Result<()> {
    let Some(agent) = agent else {
        return Ok(());
    };
    let settings = Config::load(&ctx.paths)?.resolve(&ConfigLayers::default())?;
    agents::ensure_available(agent.name(), &settings)
}

/// Register `source` as a contribution and say where its pushes go.
async fn register(ctx: &Context, source: String) -> Result<Repository> {
    let repo = ctx
        .progress(
            "Registering contribution",
            request::<Repository>(
                &ctx.paths,
                RepositoryMethod::RegisterContribution { source, name: None },
            ),
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
