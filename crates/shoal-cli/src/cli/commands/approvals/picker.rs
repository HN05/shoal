//! The `shoal approvals` picker.
use anyhow::{Context as _, Result, ensure};
use sha2::{Digest as _, Sha256};

use super::{about, ensure_scope_token, list_requests, show_request, submit};
use crate::{
    cli::{
        args::approvals::Decision,
        context::Context,
        internal,
        output::{Palette, Style},
        ui::{self, Entries, WidthLimit},
    },
    workers::Worker,
};
use shoal_protocol::approvals::{ApprovalRequest, DecisionStatus, ProposalDecision};
use shoal_protocol::space_names::SpaceSelector;

/// Open the chosen request; Ctrl-A approves and Ctrl-D denies the
/// highlighted one in place, with the outcome shown in the list.
pub(super) async fn pick_and_open(
    ctx: &Context,
    space: Option<SpaceSelector>,
    requests: Vec<ApprovalRequest>,
) -> Result<i32> {
    let row = choose(ctx, space.as_ref(), requests)?;
    let request = displayed(ctx, &row).await?;
    show_request(ctx, Box::new(request)).await
}

pub(in crate::cli::commands) async fn print_rows(
    ctx: &Context,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    for (id, label) in listed_rows(list_requests(ctx, space).await?)? {
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
        request.number,
        approve,
        Some(Box::new(request)),
        ProposalDecision::default(),
    )
    .await
}

fn choose(
    ctx: &Context,
    space: Option<&SpaceSelector>,
    requests: Vec<ApprovalRequest>,
) -> Result<String> {
    let keys = [
        decision_key(ctx, ui::Key::Ctrl('a'), "approve", Decision::Approve)?,
        decision_key(ctx, ui::Key::Ctrl('d'), "deny", Decision::Deny)?,
    ];
    let reload = internal::shell_command(&ctx.paths, Worker::ApprovalRows { space })?;
    ui::pick_in_place(
        ctx,
        "Approval> ",
        listed_rows(requests)?,
        "open",
        &keys,
        &reload,
    )
}

fn decision_key<'a>(
    ctx: &Context,
    key: ui::Key,
    label: &'a str,
    decision: Decision,
) -> Result<ui::InPlaceKey<'a>> {
    Ok(ui::InPlaceKey {
        key,
        label,
        command: internal::shell_command(
            &ctx.paths,
            Worker::ApprovalDecide {
                decision: decision.into(),
            },
        )?,
    })
}

/// The request `row` shows, by its number wherever its workspace is,
/// refusing one that changed or replaced it since.
async fn displayed(ctx: &Context, row: &str) -> Result<ApprovalRequest> {
    let number = row
        .split_once(':')
        .and_then(|(number, _)| number.parse().ok())
        .context("picker returned an invalid approval request")?;
    let request = crate::client::approvals::show(&ctx.paths, number).await?;
    ensure!(
        row_id(&request)? == row,
        "approval request {number} changed since it was listed"
    );
    Ok(*request)
}

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

fn picker_rows(requests: &[ApprovalRequest]) -> Result<Entries> {
    let cells = requests
        .iter()
        .map(|request| {
            [
                (request.number.to_string(), Some(Style::Muted)),
                status_cell(request.status),
                (super::space_label(request), None),
                (about(request), None),
            ]
        })
        .collect();
    let limit = ui::picker_width().map(|width| WidthLimit { column: 3, width });
    let labels = ui::aligned_to(cells, Palette::picker(), limit);
    let ids = requests.iter().map(row_id).collect::<Result<Vec<_>>>()?;
    Ok(ids.into_iter().zip(labels).collect())
}

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
    Ok(format!("{}:{digest}", request.number))
}
