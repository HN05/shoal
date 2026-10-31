//! `shoal forge download`: an image or attached file on the forge of the
//! repository of the workspace or registered checkout, or of another one,
//! read with the login Shoal uses there.
use std::{io::Write as _, path::Path};

use anyhow::{Context as _, Result};
use serde::Serialize;

use crate::{
    cli::{
        args::repository::DownloadArgs, client, commands::items::select_context, context::Context,
    },
    forge::ForgeRepo,
};

/// Where a download was written, for `--json`.
#[derive(Serialize)]
struct Written {
    path: std::path::PathBuf,
    bytes: usize,
    content_type: Option<String>,
}

pub(super) async fn download(ctx: &Context, args: DownloadArgs) -> Result<i32> {
    // Refuse a bad repository before the workspace picker asks for anything.
    args.repo
        .as_deref()
        .map(ForgeRepo::check_named)
        .transpose()?;
    let context = select_context(ctx, args.workspace).await?;
    let download = ctx
        .progress(
            "Downloading",
            client::forge_download(&ctx.paths, context, args.url, args.repo),
        )
        .await?;
    if args.output == Path::new("-") {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(&download.bytes)
            .and_then(|()| stdout.flush())
            .context("write the file to standard output")?;
        return Ok(0);
    }
    let path = std::path::absolute(&args.output)
        .with_context(|| format!("resolve {}", args.output.display()))?;
    std::fs::write(&path, &download.bytes).with_context(|| format!("write {}", path.display()))?;
    let written = Written {
        path,
        bytes: download.bytes.len(),
        content_type: download.content_type,
    };
    ctx.show(&written, |written| {
        outln!("Saved {} ({} bytes)", written.path.display(), written.bytes);
    })?;
    Ok(0)
}
