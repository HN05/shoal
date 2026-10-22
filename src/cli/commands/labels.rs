//! `shoal labels`: the workspace repository's labels as the forge lists
//! them, so agents can pick the ones its scheme asks for.
use anyhow::{Context as _, Result};

use crate::{
    cli::{client, context::Context, ui},
    forge::{ForgeRepo, labels::Label},
    model::Workspace,
    protocol::Method,
    repository,
};

pub(super) async fn labels(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory).await?;
    let workspace: Workspace = client::request(
        &ctx.paths,
        Method::VerifyWorkspace {
            workspace,
            on_branch: false,
        },
    )
    .await?;
    let remote = repository::remote_url_from_path(&workspace.path)
        .await?
        .context("labels need an origin remote")?;
    let forge = ForgeRepo::parse(&remote)?;
    let labels = ctx
        .progress("Reading labels", forge.labels(&ctx.forge_account()?))
        .await?;
    ctx.show(&labels, |labels| render(labels))?;
    Ok(0)
}

/// One line per label: its name, description, and whether it excludes the
/// other labels of its scope.
fn render(labels: &[Label]) {
    if labels.is_empty() {
        println!("No labels");
        return;
    }
    let width = labels
        .iter()
        .map(|label| label.name.len())
        .max()
        .unwrap_or(0);
    for label in labels {
        let exclusive = if label.exclusive { " (exclusive)" } else { "" };
        let line = format!("{:width$}  {}{exclusive}", label.name, label.description);
        println!("{}", line.trim_end());
    }
}
