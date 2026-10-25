//! What each lease kind offers: its configuration and available inventory.
use anyhow::Result;
use serde::Serialize;

use super::{ports, simulators};
use crate::{
    cli::{
        CatalogKind,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    model::PortCatalog,
    sim::SimulatorCatalog,
};

/// Every kind's catalog for one workspace.
#[derive(Debug, Serialize)]
struct Catalog {
    ports: PortCatalog,
    simulators: SimulatorCatalog,
}

pub(super) async fn show(
    ctx: &Context,
    kind: Option<CatalogKind>,
    workspace: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    match kind {
        Some(CatalogKind::Port) => {
            let catalog = ports::fetch_catalog(ctx, &workspace).await?;
            ctx.show(&catalog, |catalog| {
                ports::render_catalog(catalog, Palette::stdout(ctx.json))
            })?;
        }
        Some(CatalogKind::Sim) => {
            let catalog = simulators::fetch_catalog(ctx).await?;
            ctx.show(&catalog, simulators::render_catalog)?;
        }
        None => {
            let catalog = Catalog {
                ports: ports::fetch_catalog(ctx, &workspace).await?,
                simulators: simulators::fetch_catalog(ctx).await?,
            };
            ctx.show(&catalog, |catalog| {
                render_catalog(catalog, Palette::stdout(ctx.json))
            })?;
        }
    }
    Ok(0)
}

fn render_catalog(catalog: &Catalog, palette: Palette) {
    outln!("{}", palette.paint(Style::Heading, "Ports"));
    ports::render_catalog(&catalog.ports, palette);
    outln!("{}", palette.paint(Style::Heading, "Simulators"));
    simulators::render_catalog(&catalog.simulators);
}
