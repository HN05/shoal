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
    submit(ctx, id, approve, shown).await
}

/// Approve or deny the pending request `id` created at `created_at`, as a
/// list showed it: refuse one that replaced it, and ask which with what it
/// grants in view.
pub(super) async fn decide_listed(ctx: &Context, id: i64, created_at: u64) -> Result<i32> {
    ensure_scope_token(ctx)?;
    let request =
        request::<Vec<ApprovalRequest>>(&ctx.paths, Method::ListApprovals { workspace: None })
            .await?
            .into_iter()
            .find(|request| {
                request.id == id
                    && request.created_at == created_at
                    && request.status == DecisionStatus::Pending
            })
            .with_context(|| format!("approval request {id} changed since it was listed"))?;
    let shown = summary(&request);
    let choices = [
        (true, format!("approve  {shown}")),
        (false, format!("deny     {shown}")),
    ];
    let choices: Vec<_> = choices
        .iter()
        .map(|(approve, label)| (*approve, label.as_str()))
        .collect();
    let approve = ui::pick_choice(ctx, "Approval> ", &choices)?;
    submit(ctx, id, approve, Some(Box::new(request))).await
}

/// Send the decision on request `id`, which `shown` names when the user
/// chose it from a list, so a request reusing its number is refused.
async fn submit(
    ctx: &Context,
    id: i64,
    approve: bool,
    shown: Option<Box<ApprovalRequest>>,
) -> Result<i32> {
    let method = Method::DecideApproval { id, approve, shown };
    let request = request::<Box<ApprovalRequest>>(&ctx.paths, method).await?;
    let message = match &request.url {
        Some(url) => format!("{}\n  {url}", describe(&request)),
        None => describe(&request),
    };
    ctx.emit(&message, &request)?;
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
            for line in details(request) {
                println!("  {line}");
            }
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
/// and the exact settings an approval grants, or what a proposal opens.
pub(in crate::cli) fn summary(request: &ApprovalRequest) -> String {
    if request.target.is_proposal() {
        return describe(request);
    }
    format!("{}  {}", describe(request), request.specification)
}

fn describe(request: &ApprovalRequest) -> String {
    let reason = match request.reason.as_str() {
        "" => String::new(),
        reason => format!(": {reason}"),
    };
    format!(
        "{} [{}] {} in workspace {}{reason}",
        request.id,
        request.status,
        request.subject(),
        request.workspace,
    )
}

/// The lines below a request in a listing: a proposal's labels and
/// description, which the user may edit before approving, or the exact
/// settings an approval grants.
fn details(request: &ApprovalRequest) -> Vec<String> {
    let (labels, body) = if let Some(proposed) = request.proposed_issue() {
        (&proposed.issue.labels, &proposed.issue.body)
    } else {
        return vec![request.specification.to_string()];
    };
    let mut lines = Vec::new();
    if !labels.is_empty() {
        lines.push(format!("Labels: {}", labels.join(", ")));
    }
    lines.extend(body.lines().map(|line| format!("  {line}")));
    lines
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
