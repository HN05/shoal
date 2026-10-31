//! Committed setup: the agent commits `.shoal.toml` in the repository's
//! `shoal-setup` workspace.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use crate::{
    agent::{Agent, ModelChoice},
    cli::{agents, client, commands::workspaces, context::Context},
    git,
    model::{Repository, Workspace},
    protocol::ConfigTarget,
    repository,
};

/// The branch the setup agent commits the config on.
pub(super) const BRANCH: &str = "shoal-setup";

pub(super) async fn start(
    ctx: &Context,
    repo: &Repository,
    agent: Option<Agent>,
    model: ModelChoice,
    args: Vec<OsString>,
) -> Result<i32> {
    // Choose before creating work, so a declined agent leaves none.
    let target = ConfigTarget::Repository(repo.id.clone().into());
    let Some(agent) = agents::default_agent(ctx, target, agent).await? else {
        return Ok(0);
    };
    let workspace = match setup_workspace(ctx, repo).await? {
        Some(workspace) => workspace,
        None => {
            let code = open(ctx, repo).await?;
            if code != 0 {
                return Ok(code);
            }
            setup_workspace(ctx, repo)
                .await?
                .context("the opened setup workspace is missing")?
        }
    };
    eprintln!("Setting up Shoal in workspace {}", workspace.name);
    let prompt = prompt(repository::name(repo), &workspace.branch);
    agents::launch_agent(
        ctx,
        agent,
        workspace,
        Some(prompt),
        args,
        agents::LaunchOptions {
            model: agents::ModelRequest::Override(model),
            ..Default::default()
        },
    )
    .await
}

/// The repository's workspace on the setup branch.
async fn setup_workspace(ctx: &Context, repo: &Repository) -> Result<Option<Workspace>> {
    Ok(client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .find(|workspace| workspace.repository_id == repo.id && workspace.branch == BRANCH))
}

/// Open a workspace on the setup branch, reusing the branch an earlier setup left.
async fn open(ctx: &Context, repo: &Repository) -> Result<i32> {
    let exists =
        git::ref_exists(&repo.path, &git::local_ref(BRANCH), git::isolated_command).await?;
    let creation = workspaces::Creation {
        branch: (!exists).then(|| BRANCH.to_owned()),
        existing: exists.then(|| BRANCH.to_owned()),
        ..Default::default()
    };
    workspaces::add(
        ctx,
        Some(repo.id.clone()),
        creation,
        None,
        workspaces::AgentLaunch::Explicit(None),
        Vec::new(),
        workspaces::AddOptions {
            here: true,
            focus: false,
            now: false,
            after: Vec::new(),
            model: Default::default(),
        },
    )
    .await
}

/// The setup agent's instructions for repository `name` in a workspace on
/// `branch`.
pub(super) fn prompt(name: &str, branch: &str) -> String {
    super::prompt(
        name,
        &format!("this workspace is on branch {branch}"),
        "its config in `.shoal.toml`, or in `.shoal/config.toml` when that file \
         already exists,",
        "Check the result with `shoal config show` and test the setup script with \
         `shoal work setup`, then commit the config and any scripts it runs.",
    )
}
