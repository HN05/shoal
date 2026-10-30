//! `shoal forge capabilities`: what the forge of the workspace's repository, or
//! another one, supports, so agents can check before they ask for something it
//! lacks.
use anyhow::{Context as _, Result};

use crate::{
    cli::{args::repository::RepositoryArgs, client, context::Context, ui},
    forge::{ForgeRepo, capability::Capabilities},
    model::Workspace,
    protocol::WorkspaceMethod,
    repository,
};

pub(super) async fn capabilities(ctx: &Context, repository: RepositoryArgs) -> Result<i32> {
    let forge = repository_forge(ctx, repository).await?;
    ctx.show(&forge.capabilities(), render)?;
    Ok(0)
}

/// The repository `--repo` names, or the one of the selected workspace.
pub(super) async fn repository_forge(ctx: &Context, args: RepositoryArgs) -> Result<ForgeRepo> {
    let own = workspace_forge(ctx, args.workspace).await?;
    match args.repo {
        Some(named) => own.resolve_named(&named, &ctx.forge_hosts()?).await,
        None => Ok(own),
    }
}

/// The forge of the selected workspace's `origin` remote.
async fn workspace_forge(ctx: &Context, workspace: Option<String>) -> Result<ForgeRepo> {
    let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory).await?;
    let workspace: Workspace = client::request(
        &ctx.paths,
        WorkspaceMethod::VerifyWorkspace {
            workspace,
            on_branch: false,
        },
    )
    .await?;
    forge_of(ctx, &workspace).await
}

/// The forge of `workspace`'s `origin` remote.
pub(super) async fn forge_of(ctx: &Context, workspace: &Workspace) -> Result<ForgeRepo> {
    let remote = repository::remote_url_from_path(&workspace.path)
        .await?
        .context("the workspace has no origin remote")?;
    ForgeRepo::resolve(&remote, &ctx.forge_hosts()?).await
}

/// The forge's name, then one line per capability: its name, whether the
/// forge has it, and what to do instead when it does not.
fn render(capabilities: &Capabilities) {
    outln!("Forge: {}", capabilities.forge);
    let width = capabilities
        .capabilities
        .iter()
        .map(|entry| entry.capability.as_str().len())
        .max()
        .unwrap_or(0);
    for entry in &capabilities.capabilities {
        let status = if entry.supported {
            "Supported"
        } else {
            "Not supported"
        };
        let line = format!(
            "{:width$}  {status:13}  {}",
            entry.capability.as_str(),
            entry.hint.unwrap_or_default()
        );
        outln!("{}", line.trim_end());
    }
}
