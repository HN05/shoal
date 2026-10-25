//! `shoal forge api`: any request to the workspace repository's forge API, for
//! what no other command reads.
use std::{io::Read, path::Path};

use anyhow::{Context as _, Result};
use serde_json::Value;

use crate::{
    cli::{ApiArgs, client, context::Context, ui},
    forge::raw::{HttpMethod, RawRequest},
};

pub(super) async fn api(ctx: &Context, args: ApiArgs) -> Result<i32> {
    let body = args.input.as_deref().map(read_json).transpose()?;
    // As with gh, a body makes the request a POST unless it names a method.
    let method = args.method.unwrap_or(match body {
        Some(_) => HttpMethod::Post,
        None => HttpMethod::Get,
    });
    let request = RawRequest {
        method,
        path: args.path,
        body,
    };
    // Refuse a bad path before the workspace picker asks for anything.
    request.check()?;
    let workspace =
        ui::select_workspace(ctx, args.workspace, ui::Fallback::CurrentDirectory).await?;
    let response = ctx
        .progress(
            "Requesting",
            client::forge_api(&ctx.paths, workspace, request),
        )
        .await?;
    ctx.show(&response, |response| {
        outln!("{response:#}");
    })?;
    Ok(0)
}

/// The JSON body in the file at `path`, or on standard input for `-`.
fn read_json(path: &Path) -> Result<Value> {
    let text = if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("read the request body from standard input")?;
        text
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    };
    serde_json::from_str(&text).context("the request body is not JSON")
}
