//! `shoal repo setup`: an agent writes a repository's Shoal config in a workspace.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use crate::{
    agent::Agent,
    cli::{agents, client, context::Context},
    git,
    model::{Repository, Workspace},
    protocol::ConfigTarget,
    repository,
};

/// The branch the setup agent commits the config on.
const BRANCH: &str = "shoal-setup";

/// The configuration reference for this release.
const REFERENCE: &str = concat!(
    "https://github.com/HN05/shoal/blob/v",
    env!("CARGO_PKG_VERSION"),
    "/docs/reference.md"
);

pub(super) async fn start(
    ctx: &Context,
    repository: Option<String>,
    agent: Option<Agent>,
    args: Vec<OsString>,
) -> Result<i32> {
    let selector = super::repositories::chosen(ctx, repository).await?;
    // Choose before creating work, so a refused or declined agent leaves none.
    let Some(agent) =
        agents::default_agent(ctx, ConfigTarget::Repository(selector.clone()), agent).await?
    else {
        return Ok(0);
    };
    let repos = client::repositories(&ctx.paths).await?;
    let repo = repository::select(&repos, &selector).await?;
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
    let prompt = prompt(repo, &workspace);
    agents::launch_agent(
        ctx,
        agent,
        workspace,
        Some(prompt),
        args,
        agents::LaunchOptions::default(),
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
    let creation = super::workspaces::Creation {
        path: None,
        branch: (!exists).then(|| BRANCH.to_owned()),
        existing: exists.then(|| BRANCH.to_owned()),
        base: None,
        git_profile: None,
        pr: None,
    };
    super::workspaces::add(
        ctx,
        Some(repo.id.clone()),
        creation,
        None,
        super::workspaces::AgentLaunch::Explicit(None),
        Vec::new(),
        super::workspaces::AddOptions {
            here: true,
            focus: false,
            now: false,
        },
    )
    .await
}

fn prompt(repo: &Repository, workspace: &Workspace) -> String {
    format!(
        "Set up Shoal for the {name} repository; this workspace is on branch {branch}. \
         Shoal gives each task its own worktree, and the repository config prepares \
         those worktrees and coordinates what tasks share. Study how the project is \
         built, tested and run, then create or update its config in `.shoal.toml`, or \
         in `.shoal/config.toml` when that file already exists, with only what the \
         project needs: a `setup_cmd` script that prepares a new worktree, named ports \
         for servers that several workspaces run at once, `[commands]` for frequent \
         tasks, and resource pools for services or devices that tasks must not use at \
         the same time. The options are described in {REFERENCE}.\n\n\
         Check the result with `shoal config show` and test the setup script with \
         `shoal setup`, then commit the config and any scripts it runs.",
        name = repository::name(repo),
        branch = workspace.branch,
    )
}
