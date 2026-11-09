//! `shoal demo ls`: the running demos and the configured ones.
use std::collections::BTreeMap;

use anyhow::Result;

use super::lookup::{running, scope};
use crate::client::output::demo::Listing;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui,
    },
    demos::Preset,
    protocol::ConfigTarget,
};

pub(super) async fn list(ctx: &Context, workspace: Option<SpaceSelector>) -> Result<i32> {
    let workspace = scope(ctx, workspace).await?;
    let configured = match &workspace {
        Some(workspace) => {
            crate::client::workspaces::settings(
                &ctx.paths,
                ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
            )
            .await?
            .into_settings()
            .demos
        }
        None => BTreeMap::new(),
    };
    let listing = Listing {
        workspace: workspace
            .as_ref()
            .map(|workspace| workspace.name.to_string()),
        running: running(ctx, workspace.as_ref()).await?,
        configured,
    };
    let rows = listing_rows(&listing, Palette::stdout(ctx.json));
    ctx.show(&listing, |_| {
        for row in &rows {
            outln!("{row}");
        }
        if rows.is_empty() {
            outln!("No demos");
        }
    })?;
    Ok(0)
}

/// One row per demo: its name, workspace, state and preset; configured demos
/// that are not running follow the running ones.
fn listing_rows(listing: &Listing, palette: Palette) -> Vec<String> {
    let preset = |preset: Option<Preset>| preset.map(Preset::as_str).unwrap_or_default().to_owned();
    let mut rows: Vec<[ui::Cell; 4]> = listing
        .running
        .iter()
        .map(|demo| {
            [
                (demo.launch.name.clone(), Some(Style::Heading)),
                (demo.workspace.clone(), None),
                ("Running".into(), Some(Style::Success)),
                (preset(demo.launch.preset), Some(Style::Muted)),
            ]
        })
        .collect();
    for (name, demo) in &listing.configured {
        if listing
            .running
            .iter()
            .any(|running| running.launch.name == *name)
        {
            continue;
        }
        rows.push([
            (name.clone(), Some(Style::Heading)),
            (listing.workspace.clone().unwrap_or_default(), None),
            ("Not running".into(), Some(Style::Muted)),
            (preset(demo.preset), Some(Style::Muted)),
        ]);
    }
    ui::aligned(rows, palette)
}

#[cfg(test)]
mod tests;
