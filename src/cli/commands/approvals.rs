//! Review daemon-owned approval requests and render allocation decisions.
use anyhow::{Context as _, Result, ensure};
use serde_json::json;

use super::EXIT_BUSY;
use crate::{
    cli::{
        ApprovalsArgs,
        client::{self, request},
        context::Context,
        output::Style,
        ui,
    },
    daemon::approvals::{
        ApprovalRequest, DecisionStatus,
        proposals::{Duplicate, ProposalDecision},
    },
    protocol::Method,
};

pub(super) async fn run(ctx: &Context, args: ApprovalsArgs) -> Result<i32> {
    let details = decision_details(&args)?;
    if args.approve || args.deny {
        return decide(ctx, args.id, args.approve, details).await;
    }
    ensure!(
        details == ProposalDecision::default(),
        "use --approve with a title or body"
    );
    match args.id {
        Some(id) if args.escalate => escalate(ctx, id, None).await,
        Some(id) if args.withdraw => withdraw(ctx, None, id).await,
        Some(id) => show(ctx, id).await,
        None => list(ctx, args.workspace).await,
    }
}

fn decision_details(args: &ApprovalsArgs) -> Result<ProposalDecision> {
    Ok(ProposalDecision {
        title: args.title.clone(),
        body: super::items::read_body(crate::cli::BodyArgs {
            body: args.body.body.clone(),
            body_file: args.body.body_file.clone(),
        })?,
        reason: args.reason.clone(),
        duplicate_of: args
            .duplicate_of
            .clone()
            .map(Duplicate::Issue)
            .or(args.duplicate_of_approval.map(Duplicate::Approval)),
    })
}

async fn show(ctx: &Context, id: i64) -> Result<i32> {
    let request = request::<Box<ApprovalRequest>>(&ctx.paths, Method::ShowApproval { id }).await?;
    show_request(ctx, request).await
}

async fn show_request(ctx: &Context, request: Box<ApprovalRequest>) -> Result<i32> {
    if ctx.interactive_output() && request.status == DecisionStatus::Pending {
        return prompt_decision(ctx, request).await;
    }
    ctx.show(&request, |request| print_request(request))?;
    Ok(0)
}

async fn prompt_decision(ctx: &Context, request: Box<ApprovalRequest>) -> Result<i32> {
    ensure_scope_token(ctx)?;
    if !can_decide(ctx, &request).await? {
        print_request(&request);
        return Ok(0);
    }
    print_request(&request);
    let approve = prompt_decision_answer(ctx, &request)?;
    submit(
        ctx,
        request.id,
        approve,
        Some(request),
        ProposalDecision::default(),
    )
    .await
}

async fn can_decide(ctx: &Context, request: &ApprovalRequest) -> Result<bool> {
    if !crate::env::inherits_scope(&ctx.paths.state) {
        return Ok(true);
    }
    let owner = ui::select_workspace(ctx, None, ui::Fallback::OwnWorkspace).await?;
    let owner = client::inspect(&ctx.paths, owner).await?.workspace.id;
    Ok(request.decider.as_deref() == Some(owner.as_str()))
}

fn prompt_decision_answer(ctx: &Context, request: &ApprovalRequest) -> Result<bool> {
    let prompt = format!("Approve ({})? [y/n]", request.id);
    let answer = ui::input_or_cancel(ctx, &prompt)?.unwrap_or_default();
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" => Ok(true),
        "n" => Ok(false),
        _ => Err(ui::SelectionCanceled.into()),
    }
}

fn print_request(request: &ApprovalRequest) {
    outln!("{}", describe(request));
    for line in details(request) {
        outln!("  {line}");
    }
}

/// Approve or deny a request, choosing among pending ones when no number is
/// given. Only proposals take `details`.
pub(super) async fn decide(
    ctx: &Context,
    id: Option<i64>,
    approve: bool,
    details: ProposalDecision,
) -> Result<i32> {
    ensure_scope_token(ctx)?;
    // Refused before the picker asks which request.
    details.check(approve)?;
    let (id, shown) = match id {
        Some(id) => (id, None),
        // Workspace processes decide only the requests routed to them, by number.
        None if crate::env::inherits_scope(&ctx.paths.state) => {
            anyhow::bail!(
                "workspace processes decide by number: shoal approvals <number> --approve"
            )
        }
        None => {
            let picked = pick_pending(ctx).await?;
            (picked.id, Some(Box::new(picked)))
        }
    };
    submit(ctx, id, approve, shown, details).await
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
    prompt_decision(ctx, Box::new(request)).await
}

/// Send the decision on request `id`, which `shown` names when the user
/// chose it from a list, so a request reusing its number is refused. Only
/// proposals take `details`.
async fn submit(
    ctx: &Context,
    id: i64,
    approve: bool,
    shown: Option<Box<ApprovalRequest>>,
    details: ProposalDecision,
) -> Result<i32> {
    let method = Method::DecideApproval {
        id,
        approve,
        shown,
        details,
    };
    let request = request::<Box<ApprovalRequest>>(&ctx.paths, method).await?;
    for warning in &request.warnings {
        eprintln!("{warning}");
    }
    let message = match &request.url {
        Some(url) => format!("{}\n  {url}", describe(&request)),
        None => describe(&request),
    };
    ctx.emit(&message, &request)?;
    Ok(0)
}

/// Pass a request routed to this workspace's agent up to the next coordinating
/// agent that holds a grant for it, or to the user.
async fn escalate(ctx: &Context, id: i64, shown: Option<Box<ApprovalRequest>>) -> Result<i32> {
    ensure_scope_token(ctx)?;
    let workspace = ui::select_workspace(ctx, None, ui::Fallback::OwnWorkspace).await?;
    let request = request::<Box<ApprovalRequest>>(
        &ctx.paths,
        Method::EscalateApproval {
            workspace,
            id,
            shown,
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

/// Withdraw the workspace's pending request `id`, which its agent no longer needs.
pub(super) async fn withdraw(ctx: &Context, workspace: Option<String>, id: i64) -> Result<i32> {
    let workspace = match workspace {
        Some(workspace) => workspace,
        None => request::<Box<ApprovalRequest>>(&ctx.paths, Method::ShowApproval { id })
            .await?
            .workspace_id
            .clone(),
    };
    let request =
        request::<Box<ApprovalRequest>>(&ctx.paths, Method::WithdrawApproval { workspace, id })
            .await?;
    ctx.emit(&describe(&request), &request)?;
    Ok(0)
}

pub(super) async fn list(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let requests =
        request::<Vec<ApprovalRequest>>(&ctx.paths, Method::ListApprovals { workspace }).await?;
    if ctx.interactive_output() && !requests.is_empty() {
        let (selected, decision) = pick_request(ctx, requests)?;
        return match decision {
            Some(approve) => {
                ensure_scope_token(ctx)?;
                submit(
                    ctx,
                    selected.id,
                    approve,
                    Some(selected),
                    ProposalDecision::default(),
                )
                .await
            }
            None => show_request(ctx, selected).await,
        };
    }
    ctx.show(&requests, |requests| {
        for request in requests {
            print_request(request);
        }
        if requests.is_empty() {
            outln!("No approval requests");
        }
    })?;
    Ok(0)
}

fn pick_request(
    ctx: &Context,
    requests: Vec<ApprovalRequest>,
) -> Result<(Box<ApprovalRequest>, Option<bool>)> {
    let entries = requests
        .iter()
        .map(|request| (request.id.to_string(), summary(request)))
        .collect();
    let bindings = ui::KeyBindings(&[
        ("enter", "inspect", None),
        ("ctrl-a", "approve", Some(true)),
        ("ctrl-d", "deny", Some(false)),
    ]);
    let picked = ui::pick_with_keys(ctx, "Approvals> ", entries, bindings)?;
    let id: i64 = picked.id.parse()?;
    let selected = requests
        .into_iter()
        .find(|request| request.id == id)
        .context("unknown approval request")?;
    Ok((Box::new(selected), picked.action))
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
    } else if let Some(proposed) = request.proposed_pull() {
        (&proposed.pull.labels, &proposed.pull.body)
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
