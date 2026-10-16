//! Review daemon-owned approval requests and render allocation decisions.
use anyhow::{Result, ensure};
use serde_json::json;

use super::EXIT_BUSY;
use crate::{
    cli::{ApprovalsCommand, client::request, context::Context, output::Style, ui},
    daemon::approvals::{ApprovalRequest, DecisionStatus},
    protocol::Method,
};

pub(super) async fn run(ctx: &Context, command: Option<ApprovalsCommand>) -> Result<i32> {
    match command {
        Some(command @ (ApprovalsCommand::Approve { .. } | ApprovalsCommand::Deny { .. })) => {
            ensure!(
                !crate::env::inherits_scope(&ctx.paths.state),
                "workspace processes cannot decide approval requests; ask the user to run this command"
            );
            let approve = matches!(command, ApprovalsCommand::Approve { .. });
            let (ApprovalsCommand::Approve { id } | ApprovalsCommand::Deny { id }) = command else {
                unreachable!()
            };
            let id = match id {
                Some(id) => id,
                None => pick_pending(ctx).await?,
            };
            let request =
                request::<Box<ApprovalRequest>>(&ctx.paths, Method::DecideApproval { id, approve })
                    .await?;
            ctx.emit(&describe(&request), &request)?;
            Ok(0)
        }
        Some(ApprovalsCommand::List { workspace }) => list(ctx, workspace).await,
        None => list(ctx, None).await,
    }
}

async fn list(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let requests =
        request::<Vec<ApprovalRequest>>(&ctx.paths, Method::ListApprovals { workspace }).await?;
    ctx.show(&requests, |requests| {
        for request in requests {
            println!("{}", describe(request));
            println!("  {}", request.specification);
        }
        if requests.is_empty() {
            println!("No approval requests");
        }
    })?;
    Ok(0)
}

async fn pick_pending(ctx: &Context) -> Result<String> {
    let requests =
        request::<Vec<ApprovalRequest>>(&ctx.paths, Method::ListApprovals { workspace: None })
            .await?;
    let pending: Vec<_> = requests
        .iter()
        .filter(|request| request.status == DecisionStatus::Pending)
        .map(|request| (request.id.clone(), describe(request)))
        .collect();
    ensure!(!pending.is_empty(), "no pending approval requests");
    ui::pick(ctx, "Approval request> ", pending)
}

fn describe(request: &ApprovalRequest) -> String {
    format!(
        "{} [{}] {} in workspace {}: {}",
        request.id,
        request.status,
        request.subject(),
        request.workspace,
        request.reason
    )
}

pub(super) fn declined(ctx: &Context, request: &ApprovalRequest) -> Result<i32> {
    let code = if request.status == DecisionStatus::Denied {
        "approval_denied"
    } else {
        "approval_pending"
    };
    ctx.emit_styled(
        Style::Warning,
        &describe(request),
        json!({"acquired": false, "code": code, "request": request}),
    )?;
    Ok(EXIT_BUSY)
}
