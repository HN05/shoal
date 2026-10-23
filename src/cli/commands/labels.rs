//! `shoal labels`: the workspace repository's labels as the forge lists
//! them, so agents can pick the ones its scheme asks for.
use anyhow::Result;

use crate::{
    cli::{commands::capabilities::workspace_forge, context::Context},
    forge::conventions::labels::Label,
};

pub(super) async fn labels(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let forge = workspace_forge(ctx, workspace).await?;
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
