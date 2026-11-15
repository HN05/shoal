use anyhow::Result;

use crate::cli::{
    args::{
        WorkspaceScope,
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
            let scope = WorkspaceScope {
                workspace: space,
                all,
            };
            leases::show(&ctx, kind, scope).await
        }
        ResourcesCommand::Catalog { kind, workspace } => catalog::show(&ctx, kind, workspace).await,
        ResourcesCommand::History { kind } => match kind {
            HistoryKind::Sim {
                workspace,
                all,
                limit,
                before,
            } => simulators::history(&ctx, WorkspaceScope { workspace, all }, limit, before).await,
        },
    }
}
