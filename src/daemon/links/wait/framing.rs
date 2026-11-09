//! Fitting a response's updates and messages into one protocol frame,
//! moving what does not fit to a later delivery.
use anyhow::Result;

use super::{
    Updates,
    cursor::{all_pending, save_pending},
};
use crate::{
    client::space_ids::SpaceId,
    daemon::{agent_messages::AgentMessage, workspace::Manager},
    forge::updates::Update,
};

/// The bytes a response's updates and messages may take, leaving room for
/// its other fields.
pub(crate) const RESPONSE_LIMIT: usize = crate::protocol::MAX_FRAME - 256;

impl Manager {
    /// The leading updates of `batch` that fit one response beside `reserved`
    /// bytes of its other content. Updates sharing
    /// a delivery are acknowledged together, so a delivery is cut only when
    /// it comes first and does not fit alone; its remainder then moves to a
    /// new delivery and stays pending once the sent part is acknowledged. The
    /// caller holds the PR gates of the workspaces in `batch`.
    pub(super) async fn fit_response(
        &self,
        mut batch: Batch,
        reserved: usize,
    ) -> Result<Vec<Update>> {
        let limit = RESPONSE_LIMIT.saturating_sub(reserved);
        match frame_cut(&batch, limit) {
            Cut::Between(fitting) => batch.truncate(fitting),
            Cut::Inside(fitting) => {
                let (workspace, update) = &batch[0];
                let (workspace, delivery) = (workspace.clone(), update.delivery.clone());
                self.store
                    .run(move |db| redeliver_after(db, &workspace, &delivery, fitting))
                    .await?;
                batch.truncate(fitting);
            }
        }
        Ok(batch.into_iter().map(|(_, update)| update).collect())
    }
}

/// Pending updates for one response, each with the workspace whose cursor
/// holds it.
pub(super) type Batch = Vec<(SpaceId, Update)>;

pub(super) fn own_batch(id: &SpaceId, updates: Vec<Update>) -> Batch {
    updates
        .into_iter()
        .map(|update| (id.to_owned(), update))
        .collect()
}

/// Where a response ends within a batch.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Cut {
    /// After this many updates, between deliveries.
    Between(usize),
    /// After this many updates of the first delivery, which does not fit alone.
    Inside(usize),
}

/// Where the updates that fit in `limit` bytes end: after whole deliveries,
/// or inside the first when it alone does not fit.
pub(super) fn frame_cut(batch: &Batch, limit: usize) -> Cut {
    let sizes = batch
        .iter()
        .map(|(_, update)| serde_json::to_vec(update).map_or(usize::MAX, |bytes| bytes.len()) + 1)
        .collect::<Vec<_>>();
    let mut size = 0_usize;
    let mut fitting = 0;
    while fitting < batch.len() {
        let delivery = &batch[fitting].1.delivery;
        let end = fitting
            + batch[fitting..]
                .iter()
                .take_while(|(_, update)| update.delivery == *delivery)
                .count();
        let group = sizes[fitting..end]
            .iter()
            .fold(0_usize, |size, bytes| size.saturating_add(*bytes));
        if size.saturating_add(group) >= limit {
            if fitting > 0 {
                break;
            }
            let mut size = 0_usize;
            let inside = sizes[..end]
                .iter()
                .take_while(|bytes| {
                    size = size.saturating_add(**bytes);
                    size < limit
                })
                .count();
            return if inside < end {
                Cut::Inside(inside.max(1))
            } else {
                Cut::Between(end)
            };
        }
        size += group;
        fitting = end;
    }
    Cut::Between(fitting)
}

/// Move the pending updates of `delivery` on the workspace's cursor after the
/// first `kept` to a new delivery, so acknowledging `delivery` keeps them.
fn redeliver_after(
    db: &rusqlite::Connection,
    workspace: &SpaceId,
    delivery: &str,
    kept: usize,
) -> Result<()> {
    let next = uuid::Uuid::new_v4().to_string();
    let mut seen = 0;
    for (id, url, mut pending) in all_pending(db, std::slice::from_ref(workspace))? {
        let mut moved = false;
        for update in &mut pending {
            if update.delivery == delivery {
                seen += 1;
                if seen > kept {
                    update.delivery.clone_from(&next);
                    moved = true;
                }
            }
        }
        if moved {
            save_pending(db, &id, &url, &pending)?;
        }
    }
    Ok(())
}

/// `updates` with the unread `messages` when the wait found updates and both
/// fit one response; otherwise the messages wait for a later delivery.
pub(super) fn with_unread_messages(
    mut updates: Updates,
    messages: Vec<AgentMessage>,
) -> Result<Updates> {
    if updates.updates.is_empty() || messages.is_empty() {
        return Ok(updates);
    }
    let size = serde_json::to_vec(&updates)?.len() + serde_json::to_vec(&messages)?.len();
    if size <= RESPONSE_LIMIT {
        updates.messages = messages;
    }
    Ok(updates)
}
