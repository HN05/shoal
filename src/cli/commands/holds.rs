//! CLI access to workspace holds.
use anyhow::Result;
use serde_json::json;

use crate::{
    cli::{
        HoldCommand, WorkspaceScope, client,
        context::{Context, optional},
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    model::WorkspaceHold,
    protocol::Method,
};

pub(super) async fn run(
    ctx: &Context,
    command: Option<HoldCommand>,
    scope: WorkspaceScope,
) -> Result<i32> {
    match command {
        Some(HoldCommand::Acquire {
            workspace,
            name,
            reason,
        }) => {
            let workspace =
                ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
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
        Some(HoldCommand::Release { workspace, name }) => {
            let workspace =
                ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
            client::request::<()>(&ctx.paths, Method::HoldRelease { workspace, name }).await?;
            ctx.emit_styled(Style::Success, "Hold released", json!({"released": true}))?;
            Ok(0)
        }
        Some(HoldCommand::List { scope }) => list(ctx, scope).await,
        None => list(ctx, scope).await,
    }
}

async fn list(ctx: &Context, scope: WorkspaceScope) -> Result<i32> {
    if scope.all {
        let workspaces = client::workspaces(&ctx.paths).await?;
        ctx.show(&workspaces, |workspaces| {
            for workspace in workspaces {
                println!(
                    "{}",
                    Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name)
                );
                render(&workspace.holds);
            }
        })?;
    } else {
        let workspace =
            ui::select_workspace(ctx, scope.workspace, Fallback::CurrentDirectory).await?;
        let holds =
            client::request::<Vec<WorkspaceHold>>(&ctx.paths, Method::HoldList { workspace })
                .await?;
        ctx.show(&holds, |holds| render(holds))?;
    }
    Ok(0)
}

fn render(holds: &[WorkspaceHold]) {
    if holds.is_empty() {
        println!("No holds");
    }
    for hold in holds {
        println!(
            "  {}{}",
            hold.name,
            optional(hold.reason.as_deref(), |r| format!(" ({r})"))
        );
    }
}
