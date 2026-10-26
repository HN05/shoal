use anyhow::Result;

use crate::cli::{
    args::approvals::{ApprovalsArgs, ApprovalsCommand},
    commands::approvals,
    context::Context,
};

pub(in crate::cli::commands) async fn run(
    ctx: Context,
    command: Option<ApprovalsCommand>,
    args: ApprovalsArgs,
) -> Result<i32> {
    match command {
        Some(ApprovalsCommand::Withdraw { id, workspace }) => {
            approvals::withdraw(&ctx, workspace, id).await
        }
        None => approvals::run(&ctx, args).await,
    }
}
