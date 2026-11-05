//! CLI access to workspace holds.
use anyhow::{Result, ensure};

use super::acquisition::Finish;
use crate::client::leases;
use crate::client::output::acquisition::{NotAcquired, Released};
use crate::{
    cli::{
        args::WorkspaceScope,
        context::{Context, optional},
        output::{Palette, Style},
        ui,
    },
    model::{HoldPlacer, WorkspaceHold},
};

/// Exit 2 means a pending or denied approval: a workspace process holding
/// its own workspace asks its coordinator or the user first.
pub(super) async fn acquire(
    ctx: &Context,
    workspace: String,
    name: String,
    reason: Option<String>,
) -> Result<i32> {
    let outcome = leases::acquire_hold(&ctx.paths, workspace, name, reason).await?;
    outcome.finish(
        ctx,
        |hold| {
            ctx.emit(
                &format!(
                    "Hold acquired: {}{}",
                    hold.name,
                    optional(hold.reason.as_deref(), |r| format!(" ({r})"))
                ),
                &hold,
            )
        },
        |message| NotAcquired::busy(None, message),
    )
}

pub(super) async fn release(ctx: &Context, workspace: String, name: Option<String>) -> Result<i32> {
    let name = match name {
        Some(name) => name,
        None => pick(ctx, &workspace).await?,
    };
    leases::release_hold(&ctx.paths, workspace, name).await?;
    ctx.emit_styled(Style::Success, "Hold released", Released { released: true })?;
    Ok(0)
}

pub(super) async fn list(ctx: &Context, scope: WorkspaceScope) -> Result<i32> {
    let Some(workspace) = ui::select_workspace_filter(ctx, scope).await? else {
        let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
        ctx.show(&workspaces, |workspaces| {
            for workspace in workspaces {
                outln!(
                    "{}",
                    Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name)
                );
                render(&workspace.holds);
            }
        })?;
        return Ok(0);
    };
    let holds = leases::holds(&ctx.paths, workspace).await?;
    ctx.show(&holds, |holds| render(holds))?;
    Ok(0)
}

async fn pick(ctx: &Context, workspace: &str) -> Result<String> {
    let holds = leases::holds(&ctx.paths, workspace.to_owned()).await?;
    ensure!(!holds.is_empty(), "no holds on this workspace");
    let entries = holds
        .into_iter()
        .map(|hold| {
            let label = format!(
                "{}{}",
                hold.name,
                optional(hold.reason.as_deref(), |r| format!(" ({r})"))
            );
            (hold.name, label)
        })
        .collect();
    ui::require_argument(ctx, "hold", "name")?;
    ui::pick(ctx, "Hold> ", entries)
}

fn render(holds: &[WorkspaceHold]) {
    if holds.is_empty() {
        outln!("No holds");
    }
    for hold in holds {
        outln!(
            "  {}{}{}",
            hold.name,
            optional(hold.reason.as_deref(), |r| format!(" ({r})")),
            match hold.placed_by {
                HoldPlacer::Agent => " [placed by an agent]",
                HoldPlacer::User => "",
            }
        );
    }
}
