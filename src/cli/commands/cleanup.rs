//! Removing idle cleanup candidates without waiting for their idle delay.
use anyhow::Result;

use crate::{
    cli::{
        client::request,
        context::Context,
        output::{Palette, Style},
    },
    daemon::cleanup::ManualCleanup,
    protocol::Method,
};

pub(super) async fn run(ctx: &Context, dry_run: bool) -> Result<i32> {
    let report = ctx
        .progress(
            "Cleaning up workspaces",
            request::<ManualCleanup>(&ctx.paths, Method::Cleanup { dry_run }),
        )
        .await?;
    ctx.show(&report, |report| {
        if report.removed.is_empty() && report.retained.is_empty() {
            println!("No workspaces to clean up");
        }
        let palette = Palette::stdout(ctx.json);
        for name in &report.removed {
            if report.dry_run {
                println!("Would remove {name}");
            } else {
                println!(
                    "{}",
                    palette.paint(Style::Success, format!("Removed {name}"))
                );
            }
        }
        let palette = Palette::stderr(ctx.json);
        for retained in &report.retained {
            eprintln!(
                "{}",
                palette.paint(
                    Style::Error,
                    format!("Retained {}: {}", retained.workspace, retained.error)
                )
            );
        }
    })?;
    Ok(if report.retained.is_empty() { 0 } else { 1 })
}
