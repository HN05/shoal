use anyhow::Result;

use crate::cli::{
    args::diagnostics::{AlertsArgs, AlertsCommand, DiagnosticsCommand},
    commands::notifications,
    context::Context,
};

pub(in crate::cli::commands) async fn run(
    ctx: Context,
    command: DiagnosticsCommand,
) -> Result<i32> {
    match command {
        DiagnosticsCommand::Alerts {
            command:
                Some(AlertsCommand::Send {
                    message,
                    attach,
                    space,
                }),
            ..
        } => notifications::send(&ctx, space, message, attach).await,
        DiagnosticsCommand::Alerts {
            command: Some(AlertsCommand::Clear),
            ..
        } => notifications::clear(&ctx).await,
        DiagnosticsCommand::Alerts {
            command: Some(AlertsCommand::Withdraw { id, space }),
            ..
        } => notifications::withdraw(&ctx, space, id).await,
        DiagnosticsCommand::Alerts {
            command: None,
            list:
                AlertsArgs {
                    follow: true,
                    keep_unread,
                    resume,
                    ..
                },
        } => notifications::follow(&ctx, keep_unread, resume.unwrap_or(0)).await,
        DiagnosticsCommand::Alerts {
            command: None,
            list,
        } => notifications::list(&ctx, list).await,
    }
}
