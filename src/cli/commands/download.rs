//! `shoal forge download`: an image or attached file on the forge of the
//! repository of the workspace or registered checkout, or of another one,
//! read with the login Shoal uses there.
use std::{io::Write as _, path::Path};

use anyhow::{Context as _, Result};
use serde::Serialize;

use crate::{
    cli::{
        args::repository::DownloadArgs, client, commands::items::select_repo_context,
        context::Context,
    },
    forge::download::Download,
};

/// Where a download was written, for `--json`.
#[derive(Serialize)]
struct Written {
    path: std::path::PathBuf,
    bytes: usize,
    content_type: Option<String>,
}

pub(super) async fn download(ctx: &Context, args: DownloadArgs) -> Result<i32> {
    // Refuse a bad URL before the workspace picker asks for anything.
    crate::forge::download::check_url(&args.url)?;
    let (context, repository) = select_repo_context(ctx, args.workspace, args.repo).await?;
    let download = ctx
        .progress(
            "Downloading",
            client::forge_download(&ctx.paths, context, args.url, repository),
        )
        .await?;
    if args.output == Path::new("-") {
        write_stdout(&download.bytes)?;
        return Ok(0);
    }
    let written = save(&args.output, download)?;
    ctx.show(&written, |written| {
        outln!("Saved {} ({} bytes)", written.path.display(), written.bytes);
    })?;
    Ok(0)
}

/// Write `bytes` to standard output as they are.
fn write_stdout(bytes: &[u8]) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(bytes)
        .and_then(|()| stdout.flush())
        .context("write the file to standard output")
}

/// Write `download` to `output`, relative to the current directory.
fn save(output: &Path, download: Download) -> Result<Written> {
    let path =
        std::path::absolute(output).with_context(|| format!("resolve {}", output.display()))?;
    std::fs::write(&path, &download.bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(Written {
        path,
        bytes: download.bytes.len(),
        content_type: download.content_type,
    })
}
