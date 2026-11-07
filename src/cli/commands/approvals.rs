//! Review daemon-owned approval requests and render allocation decisions.
use crate::daemon::approvals::TargetExt;
use crate::daemon::approvals::proposals::ProposalExt;
use anyhow::{Context as _, Result, ensure};
use serde_json::Value;

mod picker;
mod push;

pub(super) use picker::{decide_row, print_rows};
pub(super) use push::push_approved;

use super::EXIT_BUSY;
use crate::client::output::acquisition::NotAcquired;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        args::approvals::ApprovalsArgs,
        context::Context,
        output::{Palette, Style},
        ui::{self, Cell},
    },
    client::approvals::{
        self, ApprovalRequest, DecisionStatus, Duplicate, ProposalDecision, Specification,
    },
    client::item_numbers::ItemNumber,
};

pub(super) async fn run(ctx: &Context, args: ApprovalsArgs) -> Result<i32> {
    let details = decision_details(&args)?;
    if args.approve || args.deny {
        return decide(ctx, args.number, args.approve, details).await;
    }
    ensure!(
        details == ProposalDecision::default(),
        "use --approve with a title or body"
    );
    match args.number {
        Some(number) if args.escalate => escalate(ctx, number, None).await,
        Some(number) if args.withdraw => withdraw(ctx, number).await,
        Some(number) => show(ctx, number).await,
        None => list(ctx, args.space).await,
    }
}

fn decision_details(args: &ApprovalsArgs) -> Result<ProposalDecision> {
    Ok(ProposalDecision {
        title: args.title.clone(),
        body: super::items::read_body(crate::cli::args::BodyArgs {
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

async fn show(ctx: &Context, number: ItemNumber) -> Result<i32> {
    show_request(ctx, numbered(ctx, number).await?).await
}

/// The request `number`.
async fn numbered(ctx: &Context, number: ItemNumber) -> Result<Box<ApprovalRequest>> {
    approvals::show(&ctx.paths, number).await
}

pub(super) async fn show_request(ctx: &Context, request: Box<ApprovalRequest>) -> Result<i32> {
    if ctx.interactive_output() && request.status == DecisionStatus::Pending {
        return prompt_decision(ctx, request).await;
    }
    ctx.show(&request, |request| print_details(ctx, request))?;
    Ok(0)
}

async fn prompt_decision(ctx: &Context, request: Box<ApprovalRequest>) -> Result<i32> {
    ensure_scope_token(ctx)?;
    print_details(ctx, &request);
    if !can_decide(ctx, &request).await? {
        return Ok(0);
    }
    let approve = prompt_decision_answer(ctx)?;
    submit(
        ctx,
        request.number,
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
    let owner = match crate::env::in_chat() {
        Some(chat) => chat,
        None => {
            let owner = ui::select_workspace(ctx, None, ui::Fallback::OwnWorkspace).await?;
            crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::parse(&owner))
                .await?
                .workspace
                .id
        }
    };
    Ok(request.decider.as_deref() == Some(owner.as_str()))
}

/// Read `y` or `n`; anything else leaves the request undecided.
fn prompt_decision_answer(ctx: &Context) -> Result<bool> {
    let answer = ui::input_or_cancel(ctx, "Approve? [y/n]")?.unwrap_or_default();
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

/// The request's number and subject, then labeled rows with what the user
/// needs to decide it, and a proposal's description below them.
fn print_details(ctx: &Context, request: &ApprovalRequest) {
    let palette = Palette::stdout(ctx.json);
    let heading = format!("Approval {}: {}", request.number, request.subject());
    outln!("{}", palette.paint(Style::Heading, heading));
    for line in ui::aligned(detail_rows(request), palette) {
        outln!("  {line}");
    }
    if let Some(body) = proposed_body(request).filter(|body| !body.trim().is_empty()) {
        outln!();
        for line in body.lines() {
            outln!("  {line}");
        }
    }
}

/// Status once decided, space, reason, and the settings an approval
/// grants or the labels a proposal adds.
fn detail_rows(request: &ApprovalRequest) -> Vec<[Cell; 2]> {
    let mut rows = Vec::new();
    let mut row = |label: &str, value: String| {
        if !value.is_empty() {
            rows.push([(label.to_owned(), Some(Style::Muted)), (value, None)]);
        }
    };
    if request.status != DecisionStatus::Pending {
        row("Status", request.status.to_string());
    }
    row("Space", space_label(request));
    row("Reason", request.reason.clone());
    match proposed_labels(request) {
        Some(labels) => row("Labels", labels.join(", ")),
        None => row("Settings", settings(&request.specification)),
    }
    row("URL", request.url.clone().unwrap_or_default());
    rows
}

fn proposed_labels(request: &ApprovalRequest) -> Option<&Vec<String>> {
    request
        .proposed_issue()
        .map(|proposed| &proposed.issue.labels)
        .or_else(|| {
            request
                .proposed_pull()
                .map(|proposed| &proposed.pull.labels)
        })
}

fn proposed_body(request: &ApprovalRequest) -> Option<&str> {
    request
        .proposed_issue()
        .map(|proposed| proposed.issue.body.as_str())
        .or_else(|| {
            request
                .proposed_pull()
                .map(|proposed| proposed.pull.body.as_str())
        })
}

/// The settings an approval grants as `key=value` pairs; nested settings
/// take dotted keys and unset ones are left out.
fn settings(specification: &Specification) -> String {
    fn flatten(key: &str, value: &Value, pairs: &mut Vec<String>) {
        match value {
            Value::Null => {}
            Value::Object(fields) => {
                for (field, value) in fields {
                    let nested = match key {
                        "" => field.clone(),
                        _ => format!("{key}.{field}"),
                    };
                    flatten(&nested, value, pairs);
                }
            }
            Value::String(text) => pairs.push(format!("{key}={text}")),
            value => pairs.push(format!("{key}={value}")),
        }
    }
    let mut pairs = Vec::new();
    if let Ok(value) = serde_json::to_value(specification) {
        flatten("", &value, &mut pairs);
    }
    pairs.join("  ")
}

/// Approve or deny a request, choosing among pending ones when no number is
/// given. Only proposals take `details`.
pub(super) async fn decide(
    ctx: &Context,
    number: Option<ItemNumber>,
    approve: bool,
    details: ProposalDecision,
) -> Result<i32> {
    ensure_scope_token(ctx)?;
    // Refused before the picker asks which request.
    details.check(approve)?;
    let (number, shown) = match number {
        Some(number) => (number, None),
        // Workspace processes decide only the requests routed to them, by number.
        None if crate::env::inherits_scope(&ctx.paths.state) => {
            anyhow::bail!(
                "workspace processes decide by number: shoal approvals SPACE.ITEM --approve"
            )
        }
        None => {
            let picked = picker::pick_pending(ctx).await?;
            (picked.number, Some(Box::new(picked)))
        }
    };
    submit(ctx, number, approve, shown, details).await
}

/// Approve or deny the pending request `number` created at `created_at`,
/// as a list showed it: refuse one that replaced it, and ask which with
/// what it grants in view.
pub(super) async fn decide_listed(
    ctx: &Context,
    number: ItemNumber,
    created_at: crate::time::UnixTime,
) -> Result<i32> {
    ensure_scope_token(ctx)?;
    let request = list_requests(ctx, None)
        .await?
        .into_iter()
        .find(|request| {
            request.number == number
                && request.created_at == created_at
                && request.status == DecisionStatus::Pending
        })
        .with_context(|| format!("approval request {number} changed since it was listed"))?;
    prompt_decision(ctx, Box::new(request)).await
}

/// Send the decision on request `number`, which `shown` names when the
/// user chose it from a list, so a request reusing its number is refused.
/// Only proposals take `details`.
pub(super) async fn submit(
    ctx: &Context,
    number: ItemNumber,
    approve: bool,
    shown: Option<Box<ApprovalRequest>>,
    details: ProposalDecision,
) -> Result<i32> {
    let env = crate::env::without_secrets();
    let mut request = approvals::decide(&ctx.paths, number, approve, shown, details, env).await?;
    // An approved push runs in an operation whose end decides the request.
    if let Some(operation) = request.operation.clone()
        && request.status == DecisionStatus::Pending
    {
        request = super::operations::follow_started(ctx, operation).await?;
    }
    for warning in &request.warnings {
        eprintln!("{warning}");
    }
    let message = match &request.url {
        Some(url) => format!("{}\n  {url}", outcome(&request)),
        None => outcome(&request),
    };
    ctx.emit(&message, &request)?;
    Ok(0)
}

/// Pass a request routed to this space up to the space above it, or to the
/// user.
async fn escalate(
    ctx: &Context,
    number: ItemNumber,
    shown: Option<Box<ApprovalRequest>>,
) -> Result<i32> {
    ensure_scope_token(ctx)?;
    let space = ui::select_space(ctx, None, ui::Fallback::OwnWorkspace).await?;
    let request =
        approvals::escalate(&ctx.paths, SpaceSelector::parse(&space), number, shown).await?;
    let decider = match &request.decider {
        Some(_) => "the space above",
        None => "the user",
    };
    ctx.emit(
        &format!("Passed approval {} to {decider}", request.number),
        &request,
    )?;
    Ok(0)
}

/// A process that cleared the scope token it inherits would act as the user.
pub(super) fn ensure_scope_token(ctx: &Context) -> Result<()> {
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot decide approval requests without their scope token"
    );
    Ok(())
}

/// Withdraw the space's pending request `number`, which its agent no
/// longer needs.
pub(super) async fn withdraw(ctx: &Context, number: ItemNumber) -> Result<i32> {
    let space = numbered(ctx, number).await?.space_id;
    let request = approvals::withdraw(&ctx.paths, SpaceSelector::parse(&space), number).await?;
    ctx.emit(&outcome(&request), &request)?;
    Ok(0)
}

pub(super) async fn list(ctx: &Context, space: Option<SpaceSelector>) -> Result<i32> {
    let requests = list_requests(ctx, space.clone()).await?;
    if ctx.interactive_output() && !requests.is_empty() {
        return picker::pick_and_open(ctx, space, requests).await;
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

pub(super) async fn list_requests(
    ctx: &Context,
    space: Option<SpaceSelector>,
) -> Result<Vec<ApprovalRequest>> {
    approvals::list(&ctx.paths, space).await
}

/// What a request asks for, and why when its agent said.
pub(in crate::cli) fn about(request: &ApprovalRequest) -> String {
    match request.reason.as_str() {
        "" => request.subject(),
        reason => format!("{}: {reason}", request.subject()),
    }
}

/// The request's new status after a decision or withdrawal.
fn outcome(request: &ApprovalRequest) -> String {
    format!(
        "Approval {} {}: {}",
        request.number,
        request.status,
        request.subject()
    )
}

/// One line with what the user needs to decide a request: the description
/// and the exact settings an approval grants, or what a proposal opens.
pub(in crate::cli) fn summary(request: &ApprovalRequest) -> String {
    if request.target.is_proposal() {
        return describe(request);
    }
    format!(
        "{}  {}",
        describe(request),
        settings(&request.specification)
    )
}

fn describe(request: &ApprovalRequest) -> String {
    let reason = match request.reason.as_str() {
        "" => String::new(),
        reason => format!(": {reason}"),
    };
    format!(
        "{} [{}] {} in space {}{reason}",
        request.number,
        request.status,
        request.subject(),
        space_label(request),
    )
}

/// The space that made the request, by name, marked once removed.
pub(in crate::cli) fn space_label(request: &ApprovalRequest) -> String {
    match request.space_removed {
        true => format!("{} (removed)", request.space),
        false => request.space.clone(),
    }
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
        return vec![settings(&request.specification)];
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
        NotAcquired {
            acquired: false,
            code: Some(code.to_owned()),
            message: None,
            pool: None,
            resource: None,
            request: Some(Box::new(request.clone())),
        },
    )?;
    Ok(EXIT_BUSY)
}
