//! What each lease kind offers: its configuration and available inventory.
use anyhow::Result;
use serde::Serialize;

use super::{
    ports,
    resources::{self, Members},
    simulators,
};
use crate::{
    cli::{
        CatalogKind,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::resources::Catalog as ResourceCatalog,
    model::PortCatalog,
    sim::SimulatorCatalog,
};

/// Every kind's catalog for one workspace.
#[derive(Debug, Serialize)]
struct Catalog {
    ports: PortCatalog,
    simulators: SimulatorCatalog,
    resources: ResourceCatalog,
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
            display(ctx, &catalog, ports::render_catalog)
        }
        Some(CatalogKind::Sim) => {
            let catalog = simulators::fetch_catalog(ctx, &workspace).await?;
            display(ctx, &catalog, simulators::render_catalog)
        }
        Some(CatalogKind::Resource) => {
            let catalog = resources::fetch_catalog(ctx, &workspace, Members::Locks).await?;
            display(ctx, &catalog, resources::render_catalog)
        }
        Some(CatalogKind::Repo) => {
            let catalog = resources::fetch_catalog(ctx, &workspace, Members::Repositories).await?;
            display(ctx, &catalog, resources::render_catalog)
        }
        None => {
            let catalog = Catalog {
                ports: ports::fetch_catalog(ctx, &workspace).await?,
                simulators: simulators::fetch_catalog(ctx, &workspace).await?,
                resources: resources::fetch_catalog(ctx, &workspace, Members::All).await?,
            };
            display(ctx, &catalog, render_catalog)
        }
    }
}

fn display<T: Serialize>(ctx: &Context, catalog: &T, render: fn(&T, Palette)) -> Result<i32> {
    ctx.show(catalog, |catalog| {
        render(catalog, Palette::stdout(ctx.json))
    })?;
    Ok(0)
}

fn render_catalog(catalog: &Catalog, palette: Palette) {
    outln!("{}", palette.paint(Style::Heading, "Ports"));
    ports::render_catalog(&catalog.ports, palette);
    outln!("{}", palette.paint(Style::Heading, "Simulators"));
    simulators::render_catalog(&catalog.simulators, palette);
    outln!("{}", palette.paint(Style::Heading, "Resources"));
    resources::render_catalog(&catalog.resources, palette);
}
