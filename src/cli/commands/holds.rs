//! CLI access to workspace holds.
use anyhow::{Result, ensure};
use serde_json::json;

use crate::{
    cli::{
        WorkspaceScope, client,
        context::{Context, optional},
        output::{Palette, Style},
        ui,
    },
    model::WorkspaceHold,
    protocol::Method,
};

pub(super) async fn acquire(
    ctx: &Context,
    workspace: String,
    name: String,
    reason: Option<String>,
) -> Result<i32> {
    let hold = client::request::<WorkspaceHold>(
        &ctx.paths,
        Method::HoldAcquire {
            workspace,
            name,
            reason,
        },
    )
    .await?;
    ctx.emit(
        &format!(
            "Hold acquired: {}{}",
            hold.name,
            optional(hold.reason.as_deref(), |r| format!(" ({r})"))
        ),
        &hold,
    )?;
    Ok(0)
}

pub(super) async fn release(ctx: &Context, workspace: String, name: Option<String>) -> Result<i32> {
    let name = match name {
        Some(name) => name,
        None => pick(ctx, &workspace).await?,
    };
    client::request::<()>(&ctx.paths, Method::HoldRelease { workspace, name }).await?;
    ctx.emit_styled(Style::Success, "Hold released", json!({"released": true}))?;
    Ok(0)
}

pub(super) async fn list(ctx: &Context, scope: WorkspaceScope) -> Result<i32> {
    let Some(workspace) = ui::select_workspace_filter(ctx, scope).await? else {
        let workspaces = client::workspaces(&ctx.paths).await?;
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
    let holds =
        client::request::<Vec<WorkspaceHold>>(&ctx.paths, Method::HoldList { workspace }).await?;
    ctx.show(&holds, |holds| render(holds))?;
    Ok(0)
}

async fn pick(ctx: &Context, workspace: &str) -> Result<String> {
    let holds = client::request::<Vec<WorkspaceHold>>(
        &ctx.paths,
        Method::HoldList {
            workspace: workspace.to_owned(),
        },
    )
    .await?;
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
    ui::pick(ctx, "Hold> ", entries)
}

fn render(holds: &[WorkspaceHold]) {
    if holds.is_empty() {
        outln!("No holds");
    }
    for hold in holds {
        outln!(
            "  {}{}",
            hold.name,
            optional(hold.reason.as_deref(), |r| format!(" ({r})"))
        );
    }
}
