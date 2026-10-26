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
        ResourcesCommand::Acquire { kind, workspace } => {
            leases::acquire(&ctx, kind, workspace).await
        }
        ResourcesCommand::Release { kind, workspace } => {
            leases::release(&ctx, kind, workspace).await
        }
        ResourcesCommand::Leases {
            kind,
            workspace,
            all,
        } => leases::show(&ctx, kind, WorkspaceScope { workspace, all }).await,
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
