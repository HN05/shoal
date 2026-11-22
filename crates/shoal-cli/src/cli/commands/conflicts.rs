//! `shoal conflicts`: a workspace's conflicts with its base.
use anyhow::Result;

use crate::cli::{
    context::Context,
    ui::{self, Fallback},
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn conflicts(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    target: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(
        ctx,
        workspace,
        Fallback::CurrentDirectory,
        &crate::cli::ui::Acting::Unconverted,
    )
    .await?;
    let check = crate::client::diffs::conflicts(&ctx.paths, workspace.clone(), target).await?;
    ctx.show(&check, |check| {
        if check.clean() {
            outln!("No conflicts with {}", check.target);
        }
        show_files(&format!("Conflicts with {}", check.target), &check.files);
        show_files(
            &format!("Changed on both sides with {}", check.target),
            &check.changed_on_both_sides,
        );
    })?;
    Ok(i32::from(!check.clean()))
}

/// `files` under `heading`, unless there are none.
fn show_files(heading: &str, files: &[String]) {
    if files.is_empty() {
        return;
    }
    outln!("{heading}:");
    for file in files {
        outln!("  {file}");
    }
}
