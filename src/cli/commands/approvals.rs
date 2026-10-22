//! Review daemon-owned approval requests and render allocation decisions.
use anyhow::{Context as _, Result, ensure};
use serde_json::json;

use super::EXIT_BUSY;
use crate::{
    cli::{client::request, context::Context, output::Style, ui},
    daemon::approvals::{ApprovalRequest, DecisionStatus},
    protocol::Method,
};

/// Approve or deny a request, choosing among pending ones when no number is given.
pub(super) async fn decide(ctx: &Context, id: Option<i64>, approve: bool) -> Result<i32> {
    ensure_scope_token(ctx)?;
    let (id, shown) = match id {
        Some(id) => (id, None),
        // Workspace processes decide only the requests routed to them, by number.
        None if crate::env::inherits_scope(&ctx.paths.state) => {
            anyhow::bail!("workspace processes decide by number: shoal approve <number>")
        }
        None => {
            let picked = pick_pending(ctx).await?;
            (picked.id, Some(Box::new(picked)))
        }
    };
    let request =
        request::<Box<ApprovalRequest>>(&ctx.paths, Method::DecideApproval { id, approve, shown })
            .await?;
    ctx.emit(&describe(&request), &request)?;
    Ok(0)
}

/// Pass a request routed to this workspace's agent up to the next coordinating
/// agent that holds a grant for it, or to the user.
pub(super) async fn escalate(ctx: &Context, id: i64) -> Result<i32> {
    ensure_scope_token(ctx)?;
    let workspace = ui::select_workspace(ctx, None, ui::Fallback::OwnWorkspace).await?;
    let request = request::<Box<ApprovalRequest>>(
        &ctx.paths,
        Method::EscalateApproval {
            workspace,
            id,
            shown: None,
        },
    )
    .await?;
    let decider = match &request.decider {
        Some(_) => "the next coordinating agent",
        None => "the user",
    };
    ctx.emit(
        &format!("Passed approval {} to {decider}", request.id),
        &request,
    )?;
    Ok(0)
}

/// A process that cleared the scope token it inherits would act as the user.
fn ensure_scope_token(ctx: &Context) -> Result<()> {
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot decide approval requests without their scope token"
    );
    Ok(())
}

pub(super) async fn list(ctx: &Context, workspace: Option<String>) -> Result<i32> {
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

async fn pick_pending(ctx: &Context) -> Result<ApprovalRequest> {
    let requests =
        request::<Vec<ApprovalRequest>>(&ctx.paths, Method::ListApprovals { workspace: None })
            .await?;
    let pending: Vec<_> = requests
        .iter()
        .filter(|request| request.status == DecisionStatus::Pending)
        .map(|request| (request.id.to_string(), summary(request)))
        .collect();
    ensure!(!pending.is_empty(), "no pending approval requests");
    let id: i64 = ui::pick(ctx, "Approval request> ", pending)?.parse()?;
    requests
        .into_iter()
        .find(|request| request.id == id)
        .context("picker returned an unknown approval request")
}

/// One line with what the user needs to decide a request: the description
/// and the exact settings an approval grants.
pub(in crate::cli) fn summary(request: &ApprovalRequest) -> String {
    format!("{}  {}", describe(request), request.specification)
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
