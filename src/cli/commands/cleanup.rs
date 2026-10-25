//! Removing idle cleanup candidates without waiting for their idle delay.
use anyhow::Result;

use crate::{
    cli::{
        client::request,
        context::Context,
        output::{Palette, Style},
    },
    daemon::{
        caches::Emptying,
        cleanup::{ManualCleanup, Pick, Selection},
    },
    protocol::Method,
};

pub(super) async fn run(ctx: &Context, dry_run: bool, caches: bool) -> Result<i32> {
    let report = ctx
        .progress(
            "Cleaning up workspaces",
            request::<ManualCleanup>(
                &ctx.paths,
                Method::Cleanup {
                    dry_run,
                    selection: Selection {
                        workspaces: Pick::All,
                        caches: if caches { Pick::All } else { Pick::None },
                        repository: None,
                    },
                },
            ),
        )
        .await?;
    let failed = report
        .caches
        .iter()
        .any(|cleanup| matches!(cleanup.emptying, Emptying::Failed { .. }));
    ctx.show(&report, |report| {
        if report.removed.is_empty() && report.retained.is_empty() {
            outln!("No workspaces to clean up");
        }
        let palette = Palette::stdout(ctx.json);
        for name in &report.removed {
            if report.dry_run {
                outln!("Would remove {name}");
            } else {
                outln!(
                    "{}",
                    palette.paint(Style::Success, format!("Removed {name}"))
                );
            }
        }
        if !report.kept.is_empty() {
            outln!("Kept:");
            let width = report
                .kept
                .iter()
                .map(|kept| kept.workspace.chars().count())
                .max()
                .unwrap_or(0);
            for kept in &report.kept {
                outln!("  {:width$}  {}", kept.workspace, kept.reason);
            }
        }
        show_caches(report, ctx.json);
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
    Ok(if report.retained.is_empty() && !failed {
        0
    } else {
        1
    })
}

fn show_caches(report: &ManualCleanup, json: bool) {
    let palette = Palette::stdout(json);
    for cleanup in &report.caches {
        let repository = &cleanup.repository;
        match &cleanup.emptying {
            Emptying::Emptied { caches } if report.dry_run => {
                outln!("Would empty caches of {repository}: {}", caches.join(", "));
            }
            Emptying::Emptied { caches } => outln!(
                "{}",
                palette.paint(
                    Style::Success,
                    format!("Emptied caches of {repository}: {}", caches.join(", "))
                )
            ),
            Emptying::Busy { caches } => outln!(
                "Kept caches of {repository}: {} (an execution or seed build is running)",
                caches.join(", ")
            ),
            Emptying::Failed { error } => eprintln!(
                "{}",
                Palette::stderr(json).paint(
                    Style::Error,
                    format!("Could not empty caches of {repository}: {error}")
                )
            ),
        }
    }
}
