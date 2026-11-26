//! `shoal conflicts`: a workspace's conflicts with its base.
use anyhow::Result;

use crate::cli::{
    context::Context,
    ui::{self, Acting, Fallback},
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn conflicts(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    target: Option<String>,
) -> Result<i32> {
    let checking = checking();
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory(&checking));
    let workspace = workspace.await?;
    let check = (crate::client::diffs::conflicts(workspace.clone(), target))
        .send(&ctx.paths)
        .await?;
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

fn show_files(heading: &str, files: &[String]) {
    if files.is_empty() {
        return;
    }
    outln!("{heading}:");
    for file in files {
        outln!("  {file}");
    }
}

/// The branch it checks against decides nothing of which workspaces it reaches.
pub(super) fn checking() -> Acting {
    Acting::request(|space| crate::client::diffs::conflicts(space, None))
}
