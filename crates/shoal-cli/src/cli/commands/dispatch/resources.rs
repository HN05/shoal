use anyhow::Result;

use crate::cli::{
    args::{
        SpaceFilter,
        resources::{HistoryKind, ResourcesCommand},
    },
    commands::{catalog, leases, simulators},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: ResourcesCommand) -> Result<i32> {
    match command {
        ResourcesCommand::Acquire { kind, space } => leases::acquire(&ctx, kind, space).await,
        ResourcesCommand::Release { kind, space } => leases::release(&ctx, kind, space).await,
        ResourcesCommand::Leases { kind, space, all } => {
            let filter = SpaceFilter { space, all };
            leases::show(&ctx, kind, filter).await
        }
        ResourcesCommand::Catalog { kind, workspace } => catalog::show(&ctx, kind, workspace).await,
        ResourcesCommand::History { kind } => match kind {
            HistoryKind::Sim {
                workspace,
                all,
                limit,
                before,
            } => {
                let filter = SpaceFilter {
                    space: workspace,
                    all,
                };
                simulators::history(&ctx, filter, limit, before).await
            }
        },
    }
}
