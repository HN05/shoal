//! `shoal forge capabilities`: what the forge of the workspace's repository, or
//! another one, supports, so agents can check before they ask for something it
//! lacks. Where a forge's versions differ, the server is asked.
use anyhow::Result;

use crate::{
    cli::{
        args::repository::RepositoryArgs, client, commands::items::select_repo_context,
        context::Context,
    },
    forge::capability::Capabilities,
};

pub(super) async fn capabilities(ctx: &Context, repository: RepositoryArgs) -> Result<i32> {
    let (context, named) = select_repo_context(ctx, repository.repo).await?;
    let read = client::capabilities(&ctx.paths, context, named);
    let capabilities = ctx.progress("Reading capabilities", read).await?;
    ctx.show(&capabilities, render)?;
    Ok(0)
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
            entry.hint.as_deref().unwrap_or_default()
        );
        outln!("{}", line.trim_end());
    }
}
