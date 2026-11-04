//! `shoal repo labels`: the labels of the workspace's repository, or another
//! one, as the forge lists them, so agents can pick the ones its scheme asks
//! for.
use anyhow::Result;

use crate::{
    cli::{
        args::repository::RepositoryArgs, client, commands::items::select_repo_context,
        context::Context,
    },
    forge::conventions::labels::Label,
};

pub(super) async fn labels(ctx: &Context, repository: RepositoryArgs) -> Result<i32> {
    let (context, named) = select_repo_context(ctx, repository.repo).await?;
    let labels = ctx
        .progress("Reading labels", client::labels(&ctx.paths, context, named))
        .await?;
    ctx.show(&labels, |labels| render(labels))?;
    Ok(0)
}
/// One line per label: its name, description, and whether it excludes the
/// other labels of its scope.
fn render(labels: &[Label]) {
    if labels.is_empty() {
        outln!("No labels");
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
        outln!("{}", line.trim_end());
    }
}
