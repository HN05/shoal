//! `shoal repo labels`.
use anyhow::Result;

use crate::{
    cli::{
        args::repository::RepositoryArgs, client, commands::items::select_repo_context,
        context::Context,
    },
    forge::conventions::labels::Label,
};

pub(super) async fn labels(ctx: &Context, repository: RepositoryArgs) -> Result<i32> {
    let target = select_repo_context(ctx, repository.repo).await?;
    let labels = ctx
        .progress("Reading labels", client::labels(&ctx.paths, target))
        .await?;
    ctx.show(&labels, |labels| render(labels))?;
    Ok(0)
}
fn render(labels: &[Label]) {
    if labels.is_empty() {
        outln!("No labels");
        return;
    }
    let width = labels
        .iter()
        .map(|label| shoal_text::width(&label.name))
        .max()
        .unwrap_or(0);
    for label in labels {
        let exclusive = if label.exclusive { " (exclusive)" } else { "" };
        let line = format!(
            "{}  {}{exclusive}",
            shoal_text::pad(&label.name, width),
            label.description
        );
        outln!("{}", line.trim_end());
    }
}
