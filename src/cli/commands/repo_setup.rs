//! `shoal repo setup`: an agent writes a repository's Shoal config in a
//! workspace, committing it or saving it as the repository's local config.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

use crate::{
    agent::Agent,
    cli::{agents, client, context::Context, ui},
    git,
    model::{Repository, Workspace},
    protocol::ConfigTarget,
    repository,
};

/// Where the setup agent puts the config it writes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    /// `.shoal.toml`, committed on the setup branch.
    Commit,
    /// The repository's local config file, outside every Git tree.
    Local(PathBuf),
}

impl Destination {
    /// The branch of the workspace the setup agent works in, which only the
    /// committed config changes.
    fn branch(&self) -> &'static str {
        match self {
            Self::Commit => "shoal-setup",
            Self::Local(_) => "shoal-setup-local",
        }
    }
}

pub(super) async fn start(
    ctx: &Context,
    repository: Option<String>,
    local: bool,
    agent: Option<Agent>,
    args: Vec<OsString>,
) -> Result<i32> {
    let selector = super::repositories::chosen(ctx, repository).await?;
    // Choose before creating work, so a refused or declined agent leaves none.
    let destination = destination(ctx, &selector, local).await?;
    let Some(agent) =
        agents::default_agent(ctx, ConfigTarget::Repository(selector.clone()), agent).await?
    else {
        return Ok(0);
    };
    let repos = client::repositories(&ctx.paths).await?;
    let repo = repository::select(&repos, &selector).await?;
    let branch = destination.branch();
    let workspace = match setup_workspace(ctx, repo, branch).await? {
        Some(workspace) => workspace,
        None => {
            let code = open(ctx, repo, branch).await?;
            if code != 0 {
                return Ok(code);
            }
            setup_workspace(ctx, repo, branch)
                .await?
                .context("the opened setup workspace is missing")?
        }
    };
    eprintln!("Setting up Shoal in workspace {}", workspace.name);
    let prompt = prompt(repository::name(repo), &workspace.branch, &destination);
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

/// Where the config goes: locally with `local` or when the user chooses it,
/// otherwise committed, as it is without a terminal.
async fn destination(
    ctx: &Context,
    selector: &repository::Selector,
    local: bool,
) -> Result<Destination> {
    let local = local
        || (ctx.interactive()
            && ui::pick_choice(
                ctx,
                "Setup> ",
                &[(false, "Commit to repository"), (true, "Save locally")],
            )?);
    Ok(if local {
        Destination::Local(super::repositories::local_config_path(ctx, selector.clone()).await?)
    } else {
        Destination::Commit
    })
}

/// The repository's workspace on `branch`.
async fn setup_workspace(
    ctx: &Context,
    repo: &Repository,
    branch: &str,
) -> Result<Option<Workspace>> {
    Ok(client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .find(|workspace| workspace.repository_id == repo.id && workspace.branch == branch))
}

/// Open a workspace on `branch`, reusing the branch an earlier setup left.
async fn open(ctx: &Context, repo: &Repository, branch: &str) -> Result<i32> {
    let exists =
        git::ref_exists(&repo.path, &git::local_ref(branch), git::isolated_command).await?;
    let creation = super::workspaces::Creation {
        branch: (!exists).then(|| branch.to_owned()),
        existing: exists.then(|| branch.to_owned()),
        ..Default::default()
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
            after: Vec::new(),
        },
    )
    .await
}

/// The setup agent's instructions for repository `name` in a workspace on
/// `branch`.
fn prompt(name: &str, branch: &str, destination: &Destination) -> String {
    let (file, finish) = match destination {
        Destination::Commit => (
            "its config in `.shoal.toml`, or in `.shoal/config.toml` when that file \
             already exists,"
                .to_owned(),
            "Check the result with `shoal config show` and test the setup script with \
             `shoal work setup`, then commit the config and any scripts it runs."
                .to_owned(),
        ),
        Destination::Local(path) => (local_file(path), local_finish(path, name)),
    };
    format!(
        "Set up Shoal for the {name} repository; this workspace is on branch {branch}. \
         Shoal gives each task its own worktree, and the repository config prepares \
         those worktrees and coordinates what tasks share. Study how the project is \
         built, tested and run, then create or update {file} with only what the \
         project needs: a `setup.command` script that prepares a new worktree, named ports \
         for servers that several workspaces run at once, `[commands]` for frequent \
         tasks, and resource pools for services or devices that tasks must not use at \
         the same time. `shoal config reference --text` prints the reference that \
         describes the options.\n\n{finish}",
    )
}

fn local_file(path: &Path) -> String {
    format!(
        "its local config, `{}`, which Shoal reads in place outside the repository's \
         Git tree,",
        path.display()
    )
}

fn local_finish(path: &Path, name: &str) -> String {
    let directory = path.parent().unwrap_or(path);
    format!(
        "Save the scripts the config runs in `{directory}` beside it and name them by \
         paths relative to that directory; `setup.seed` stays relative to the worktree. \
         The repository's own `.shoal.toml` or `.shoal/config.toml`, when it has one, \
         still applies beneath the local config option by option, so write only what it \
         lacks or must change. Leave the repository's files unchanged and commit \
         nothing: the local config applies to every workspace of the repository as soon \
         as you save it. Check the result with `shoal config show` and test the setup \
         script with `shoal work setup`, then tell the user to check it with \
         `shoal config show --repo {name}`.",
        directory = directory.display(),
    )
}

#[cfg(test)]
mod tests;
