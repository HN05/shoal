//! The `shoal approvals` picker: requests chosen to open or decide.
use anyhow::{Context as _, Result, ensure};

use super::{about, ensure_scope_token, list_requests, show_request, submit};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Entries, WidthLimit},
    },
    daemon::approvals::{ApprovalRequest, DecisionStatus, proposals::ProposalDecision},
};

/// Open the chosen request, or decide it with a key and choose again while
/// pending requests remain; leaving the picker after a decision succeeds.
pub(super) async fn pick_and_decide(
    ctx: &Context,
    workspace: Option<String>,
    mut requests: Vec<ApprovalRequest>,
) -> Result<i32> {
    let mut decided = false;
    loop {
        let (selected, decision) = match pick_request(ctx, requests) {
            Err(error) if decided && error.is::<ui::SelectionCanceled>() => return Ok(0),
            picked => picked?,
        };
        let Some(approve) = decision else {
            return show_request(ctx, selected).await;
        };
        ensure_scope_token(ctx)?;
        submit(
            ctx,
            selected.id,
            approve,
            Some(selected),
            ProposalDecision::default(),
        )
        .await?;
        decided = true;
        requests = list_requests(ctx, workspace.clone()).await?;
        if !requests
            .iter()
            .any(|request| request.status == DecisionStatus::Pending)
        {
            return Ok(0);
        }
    }
}

/// Choose a request, pending ones first, and how to act on it.
fn pick_request(
    ctx: &Context,
    mut requests: Vec<ApprovalRequest>,
) -> Result<(Box<ApprovalRequest>, Option<bool>)> {
    requests.sort_by_key(|request| request.status != DecisionStatus::Pending);
    let bindings = ui::KeyBindings(&[
        ("enter", "open", None),
        ("ctrl-a", "approve", Some(true)),
        ("ctrl-d", "deny", Some(false)),
    ]);
    let picked = ui::pick_with_keys(ctx, "Approval> ", picker_rows(&requests), bindings)?;
    let id: i64 = picked.id.parse()?;
    let selected = requests
        .into_iter()
        .find(|request| request.id == id)
        .context("unknown approval request")?;
    Ok((Box::new(selected), picked.action))
}

pub(super) async fn pick_pending(ctx: &Context) -> Result<ApprovalRequest> {
    let pending: Vec<_> = list_requests(ctx, None)
        .await?
        .into_iter()
        .filter(|request| request.status == DecisionStatus::Pending)
        .collect();
    ensure!(!pending.is_empty(), "no pending approval requests");
    let id: i64 = ui::pick(ctx, "Approval> ", picker_rows(&pending))?.parse()?;
    pending
        .into_iter()
        .find(|request| request.id == id)
        .context("picker returned an unknown approval request")
}

/// One aligned row per request: its number, its status once decided, the
/// workspace, and what it asks for and why, cut to fit the picker.
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
        .map(|request| request.id.to_string())
        .zip(labels)
        .collect()
}
