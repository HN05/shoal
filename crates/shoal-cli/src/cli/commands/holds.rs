//! CLI access to the holds that keep spaces from cleanup.
use anyhow::{Result, ensure};

use super::acquisition::Finish;
use crate::client::leases;
use crate::client::output::acquisition::Released;
use crate::{
    cli::{
        args::WorkspaceScope,
        context::{Context, optional},
        output::{Palette, Style},
        ui,
    },
    model::{HoldPlacer, SpaceHold},
};
use shoal_protocol::space_names::SpaceSelector;

/// Exit 2 means a pending or denied approval: a scoped process holding its
/// own space asks its coordinator or the user first.
pub(super) async fn acquire(
    ctx: &Context,
    space: SpaceSelector,
    name: String,
    reason: Option<String>,
) -> Result<i32> {
    let outcome = leases::acquire_hold(&ctx.paths, space, name, reason).await?;
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
        std::convert::identity,
    )
}

pub(super) async fn release(
    ctx: &Context,
    space: SpaceSelector,
    name: Option<String>,
) -> Result<i32> {
    let name = match name {
        Some(name) => name,
        None => pick(ctx, &space).await?,
    };
    leases::release_hold(&ctx.paths, space, name).await?;
    ctx.emit_styled(Style::Success, "Hold released", Released { released: true })?;
    Ok(0)
}

/// The holds of the space `scope` selects, or of every space, the
/// workspaces then the chats.
pub(super) async fn list(ctx: &Context, scope: WorkspaceScope) -> Result<i32> {
    let Some(space) = ui::select_space_filter(ctx, scope).await? else {
        let spaces = crate::client::spaces::list(&ctx.paths).await?;
        ctx.show(&spaces.0, |spaces| {
            for space in spaces {
                outln!(
                    "{}",
                    Palette::stdout(ctx.json).paint(Style::Heading, &space.name)
                );
                render(&space.holds);
            }
        })?;
        return Ok(0);
    };
    let holds = leases::holds(&ctx.paths, space).await?;
    ctx.show(&holds, |holds| render(holds))?;
    Ok(0)
}

async fn pick(ctx: &Context, space: &SpaceSelector) -> Result<String> {
    let holds = leases::holds(&ctx.paths, space.clone()).await?;
    ensure!(!holds.is_empty(), "no holds on this space");
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

/// What a removal that releases `holds` names before it asks: each hold,
/// with its reason; nothing without one.
pub(in crate::cli::commands) fn losses(holds: &[SpaceHold]) -> Vec<String> {
    if holds.is_empty() {
        return Vec::new();
    }
    let lines = (holds.iter()).map(|hold| {
        format!(
            "  {}{}",
            hold.name,
            optional(hold.reason.as_deref(), |r| format!(" ({r})"))
        )
    });
    std::iter::once("Holds:".to_owned()).chain(lines).collect()
}

fn render(holds: &[SpaceHold]) {
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
