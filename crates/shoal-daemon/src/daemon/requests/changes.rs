//! The daemon's answers to [`ChangeMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Context as _, Result};

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{Body, ChangeMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ChangeMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ChangeMethod::RecordChange { change } => {
            let caller = caller.context("only an agent's hooks record its changes")?;
            Body::Changes(manager.record_tool_change(caller, change).await?)
        }
        ChangeMethod::ListChanges { filter } => {
            Body::Changes(manager.changes(caller, filter).await?)
        }
        ChangeMethod::ChangeDiffs { filter } => {
            Body::ChangeDiffs(manager.change_diffs(caller, filter).await?)
        }
        ChangeMethod::ClaimChanges {
            space,
            changes,
            thread,
        } => {
            let claimed = manager.claim_changes(caller, space.as_ref(), &changes, thread);
            Body::Changes(claimed.await?)
        }
        ChangeMethod::ReportEdits { paths } => {
            manager.report_edits(caller, paths).await?;
            Body::Ok
        }
        ChangeMethod::UnclaimChanges { space, changes } => {
            let unclaimed = manager.unclaim_changes(caller, space.as_ref(), &changes);
            Body::Changes(unclaimed.await?)
        }
    }))
}

impl Scoped for ChangeMethod {
    /// A hook records the changes of its own space's agent. A reading of
    /// a space, and a bare number, name the caller's own space by default;
    /// a thread's reading checks the caller reads the thread, and a full
    /// change number its space, as they are answered.
    async fn scope(
        &mut self,
        _manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            ChangeMethod::RecordChange { .. } | ChangeMethod::ReportEdits { .. } => None,
            ChangeMethod::ListChanges { filter } | ChangeMethod::ChangeDiffs { filter } => {
                match filter.thread {
                    Some(_) => None,
                    None => Some(caller.own_by_default(&mut filter.space)),
                }
            }
            ChangeMethod::ClaimChanges { space, .. }
            | ChangeMethod::UnclaimChanges { space, .. } => Some(caller.own_by_default(space)),
        })
    }
}
