//! The `shoal approvals` picker: requests chosen to open or decide.
use anyhow::{Context as _, Result, ensure};
use sha2::{Digest as _, Sha256};

use super::{about, ensure_scope_token, list_requests, show_request, submit};
use crate::{
    cli::{
        args::approvals::Decision,
        client::request,
        context::Context,
        internal::{self, Worker},
        output::{Palette, Style},
        ui::{self, Entries, WidthLimit},
    },
    daemon::approvals::{ApprovalRequest, DecisionStatus, proposals::ProposalDecision},
    protocol::ApprovalMethod,
};

/// Open the chosen request; Ctrl-A approves and Ctrl-D denies the
/// highlighted one in place, with the outcome shown in the list.
pub(super) async fn pick_and_open(
    ctx: &Context,
    workspace: Option<String>,
    requests: Vec<ApprovalRequest>,
) -> Result<i32> {
    let row = choose(ctx, workspace.as_deref(), requests)?;
    let request = displayed(ctx, &row).await?;
    show_request(ctx, Box::new(request)).await
}

/// Print the list's rows for the picker to reload after a decision.
pub(in crate::cli::commands) async fn print_rows(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<i32> {
    for (id, label) in listed_rows(list_requests(ctx, workspace).await?)? {
        outln!("{}", ui::picker_line(&id, &label));
    }
    Ok(0)
}

/// Approve or deny the request the picker's `row` shows, and print the
/// outcome for the picker to show.
pub(in crate::cli::commands) async fn decide_row(
    ctx: &Context,
    row: &str,
    decision: Decision,
) -> Result<i32> {
    ensure_scope_token(ctx)?;
    let request = displayed(ctx, row).await?;
    let approve = decision == Decision::Approve;
    submit(
        ctx,
        request.id,
        approve,
        Some(Box::new(request)),
        ProposalDecision::default(),
    )
    .await
}

/// Choose a request's row, with keys that decide the highlighted one
/// without leaving the list.
fn choose(
    ctx: &Context,
    workspace: Option<&str>,
    requests: Vec<ApprovalRequest>,
) -> Result<String> {
    let keys = [
        decision_key(ctx, "ctrl-a", "approve", Decision::Approve)?,
        decision_key(ctx, "ctrl-d", "deny", Decision::Deny)?,
    ];
    let reload = internal::shell_command(&ctx.paths, Worker::ApprovalRows { workspace })?;
    ui::pick_in_place(
        ctx,
        "Approval> ",
        listed_rows(requests)?,
        "open",
        &keys,
        &reload,
    )
}

/// `key`, which makes `decision` through the decision worker.
fn decision_key<'a>(
    ctx: &Context,
    key: &'a str,
    label: &'a str,
    decision: Decision,
) -> Result<ui::InPlaceKey<'a>> {
    Ok(ui::InPlaceKey {
        key,
        label,
        command: internal::shell_command(&ctx.paths, Worker::ApprovalDecide { decision })?,
    })
}

/// The request `row` shows, by its number wherever its workspace is,
/// refusing one that changed or replaced it since.
async fn displayed(ctx: &Context, row: &str) -> Result<ApprovalRequest> {
    let id = row
        .split_once(':')
        .and_then(|(id, _)| id.parse().ok())
        .context("picker returned an invalid approval request")?;
    let request =
        request::<Box<ApprovalRequest>>(&ctx.paths, ApprovalMethod::ShowApproval { id }).await?;
    ensure!(
        row_id(&request)? == row,
        "approval request {id} changed since it was listed"
    );
    Ok(*request)
}

/// Every request's row, pending ones first.
fn listed_rows(mut requests: Vec<ApprovalRequest>) -> Result<Entries> {
    requests.sort_by_key(|request| request.status != DecisionStatus::Pending);
    picker_rows(&requests)
}

pub(super) async fn pick_pending(ctx: &Context) -> Result<ApprovalRequest> {
    let pending: Vec<_> = list_requests(ctx, None)
        .await?
        .into_iter()
        .filter(|request| request.status == DecisionStatus::Pending)
        .collect();
    ensure!(!pending.is_empty(), "no pending approval requests");
    ui::require_argument(ctx, "approval request", "id")?;
    let row = ui::pick(ctx, "Approval> ", picker_rows(&pending)?)?;
    pending
        .into_iter()
        .find(|request| row_id(request).is_ok_and(|id| id == row))
        .context("picker returned an unknown approval request")
}

/// One aligned row per request, by [`row_id`]: its number, its status once
/// decided, the workspace, and what it asks for and why, cut to fit the
/// picker.
fn picker_rows(requests: &[ApprovalRequest]) -> Result<Entries> {
    let cells = requests
        .iter()
        .map(|request| {
            [
                (request.id.to_string(), Some(Style::Muted)),
                status_cell(request.status),
                (request.workspace.clone(), None),
                (about(request), None),
            ]
        })
        .collect();
    let limit = ui::picker_width().map(|width| WidthLimit { column: 3, width });
    let labels = ui::aligned_to(cells, Palette::picker(), limit);
    let ids = requests.iter().map(row_id).collect::<Result<Vec<_>>>()?;
    Ok(ids.into_iter().zip(labels).collect())
}

/// A decided request's status: a mark for an approval or a denial, and the
/// status's name otherwise.
fn status_cell(status: DecisionStatus) -> ui::Cell {
    match status {
        DecisionStatus::Pending => (String::new(), None),
        DecisionStatus::Approved => ("✓".into(), Some(Style::Success)),
        DecisionStatus::Denied => ("✗".into(), Some(Style::Error)),
        status => (status.to_string(), Some(Style::Muted)),
    }
}

/// A row's id: the request's number and a digest of its record as shown,
/// since a later request may reuse the number and a proposal may change.
/// Objects serialize with sorted keys, so the same record has one digest.
fn row_id(request: &ApprovalRequest) -> Result<String> {
    let record = serde_json::to_vec(&serde_json::to_value(request)?)?;
    let digest = Sha256::digest(record)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("{}:{digest}", request.id))
}
