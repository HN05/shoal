//! The `shoal approvals` picker: requests chosen to open or decide.
use anyhow::{Context as _, Result, ensure};

use super::{about, ensure_scope_token, list_requests, listed_pending, show_request, submit};
use crate::{
    cli::{
        args::approvals::Decision,
        context::Context,
        internal::{self, Worker},
        output::{Palette, Style},
        ui::{self, Entries, WidthLimit},
    },
    daemon::approvals::{ApprovalRequest, DecisionStatus, proposals::ProposalDecision},
};

/// Open the chosen request; Ctrl-A approves and Ctrl-D denies the
/// highlighted one in place, with the outcome shown in the list.
pub(super) async fn pick_and_open(
    ctx: &Context,
    workspace: Option<String>,
    requests: Vec<ApprovalRequest>,
) -> Result<i32> {
    let worker = |worker| internal::shell_command(&ctx.paths, worker);
    let decide = |key, label, decision| -> Result<_> {
        Ok(ui::InPlaceKey {
            key,
            label,
            command: worker(Worker::ApprovalDecide { decision })?,
        })
    };
    let keys = [
        decide("ctrl-a", "approve", Decision::Approve)?,
        decide("ctrl-d", "deny", Decision::Deny)?,
    ];
    let reload = worker(Worker::ApprovalRows {
        workspace: workspace.as_deref(),
    })?;
    let row = ui::pick_in_place(
        ctx,
        "Approval> ",
        listed_rows(requests),
        "open",
        &keys,
        &reload,
    )?;
    let (id, created_at) = parse_row(&row)?;
    let request = list_requests(ctx, workspace)
        .await?
        .into_iter()
        .find(|request| request.id == id && request.created_at == created_at)
        .with_context(|| format!("approval request {id} changed since it was listed"))?;
    show_request(ctx, Box::new(request)).await
}

/// Print the list's rows for the picker to reload after a decision.
pub(in crate::cli::commands) async fn print_rows(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<i32> {
    for (id, label) in listed_rows(list_requests(ctx, workspace).await?) {
        outln!("{}", ui::picker_line(&id, &label));
    }
    Ok(0)
}

/// Approve or deny the pending request the picker's `row` names, and print
/// the outcome for the picker to show.
pub(in crate::cli::commands) async fn decide_row(
    ctx: &Context,
    row: &str,
    decision: Decision,
) -> Result<i32> {
    ensure_scope_token(ctx)?;
    let (id, created_at) = parse_row(row)?;
    let request = listed_pending(ctx, id, created_at).await?;
    let approve = decision == Decision::Approve;
    submit(
        ctx,
        id,
        approve,
        Some(Box::new(request)),
        ProposalDecision::default(),
    )
    .await
}

/// Every request's row, pending ones first.
fn listed_rows(mut requests: Vec<ApprovalRequest>) -> Entries {
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
    let (id, created_at) = parse_row(&ui::pick(ctx, "Approval> ", picker_rows(&pending))?)?;
    pending
        .into_iter()
        .find(|request| request.id == id && request.created_at == created_at)
        .context("picker returned an unknown approval request")
}

/// One aligned row per request, by its number and when it was made since a
/// later request may reuse the number: the number, its status once
/// decided, the workspace, and what it asks for and why, cut to fit the
/// picker.
fn picker_rows(requests: &[ApprovalRequest]) -> Entries {
    let cells = requests
        .iter()
        .map(|request| {
            let status = match request.status {
                DecisionStatus::Pending => String::new(),
                status => status.to_string(),
            };
            [
                (request.id.to_string(), Some(Style::Muted)),
                (status, Some(Style::Muted)),
                (request.workspace.clone(), None),
                (about(request), None),
            ]
        })
        .collect();
    let limit = ui::picker_width().map(|width| WidthLimit { column: 3, width });
    let labels = ui::aligned_to(cells, Palette::picker(), limit);
    requests
        .iter()
        .map(|request| format!("{}:{}", request.id, request.created_at))
        .zip(labels)
        .collect()
}

/// The number and creation time a row's id holds.
fn parse_row(row: &str) -> Result<(i64, u64)> {
    let invalid = || anyhow::anyhow!("picker returned an invalid approval request");
    let (id, created_at) = row.split_once(':').ok_or_else(invalid)?;
    Ok((
        id.parse().map_err(|_| invalid())?,
        created_at.parse().map_err(|_| invalid())?,
    ))
}
