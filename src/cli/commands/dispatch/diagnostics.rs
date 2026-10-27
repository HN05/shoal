use anyhow::Result;

use crate::cli::{
    args::diagnostics::{AlertsCommand, DiagnosticsCommand},
    commands::notifications,
    context::Context,
};

pub(in crate::cli::commands) async fn run(
    ctx: Context,
    command: DiagnosticsCommand,
) -> Result<i32> {
    match command {
        DiagnosticsCommand::Alerts {
            command: Some(AlertsCommand::Send { message, workspace }),
            ..
        } => notifications::send(&ctx, workspace, message).await,
        DiagnosticsCommand::Alerts {
            command: Some(AlertsCommand::Clear),
            ..
        } => notifications::clear(&ctx).await,
        DiagnosticsCommand::Alerts {
            command: Some(AlertsCommand::Withdraw { id, workspace }),
            ..
        } => notifications::withdraw(&ctx, workspace, id).await,
        DiagnosticsCommand::Alerts {
            command: None,
            follow: true,
            keep_unread,
            resume,
            ..
        } => notifications::follow(&ctx, keep_unread, resume.unwrap_or(0)).await,
        DiagnosticsCommand::Alerts {
            command: None,
            all,
            keep_unread,
            limit,
            ..
        } => notifications::list(&ctx, all, keep_unread, limit).await,
    }
}
