//! `shoal forge capabilities`.
use anyhow::Result;

use crate::{
    cli::{
        args::repository::RepositoryArgs, client, commands::items::select_repo_context,
        context::Context,
    },
    forge::capability::Capabilities,
};

pub(super) async fn capabilities(ctx: &Context, repository: RepositoryArgs) -> Result<i32> {
    let target = select_repo_context(ctx, repository.repo).await?;
    let read = client::capabilities(&ctx.paths, target);
    let capabilities = ctx.progress("Reading capabilities", read).await?;
    ctx.show(&capabilities, render)?;
    Ok(0)
}

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
            entry.hint.as_deref().unwrap_or_default()
        );
        outln!("{}", line.trim_end());
    }
}
