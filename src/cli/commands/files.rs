//! `shoal files` lists the files attached to a workspace.
use anyhow::Result;

use crate::{
    cli::{
        client::request,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::workspace_files::{AttachedFile, FileSource},
    protocol::FileMethod,
};

/// Print the workspace's files, or with `paths` only where Shoal keeps them.
pub(super) async fn list(ctx: &Context, workspace: Option<String>, paths: bool) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let files: Vec<AttachedFile> = request(&ctx.paths, FileMethod::ListFiles { workspace }).await?;
    ctx.show(&files, |files| {
        if paths {
            for file in files {
                outln!("{}", file.path.display());
            }
            return;
        }
        let now = crate::time::unix_seconds() as i64;
        let rows = files.iter().map(|file| cells(file, now)).collect();
        for line in ui::aligned(rows, Palette::stdout(ctx.json)) {
            outln!("{line}");
        }
        if files.is_empty() {
            outln!("No files");
        }
    })?;
    Ok(0)
}

/// The file's name, size, how it came, when, read at `now`, and Shoal's copy.
fn cells(file: &AttachedFile, now: i64) -> [ui::Cell; 5] {
    [
        (file.name.clone(), Some(Style::Heading)),
        (ui::size_label(file.bytes), None),
        (source_label(file.source).into(), None),
        (
            crate::time::local_ago(file.created_at, now),
            Some(Style::Muted),
        ),
        (file.path.display().to_string(), Some(Style::Muted)),
    ]
}

fn source_label(source: FileSource) -> &'static str {
    match source {
        FileSource::Message => "message",
    }
}
