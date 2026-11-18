//! An admitted chat launch: the chat recorded while it waited starts in a
//! background place where the terminal that queued it would have opened
//! one, and the chat's agent takes the launch's slot.
use anyhow::Result;

use crate::client::connection::CommandPaths;
use shoal_core::start_plan::{ChatPlan, ChatPlanStart};
use shoal_ids::queue_entry_ids::QueueEntryId;

/// Start the chat of the admitted launch `entry` that `plan` describes, its
/// agent taking the launch's slot.
pub(super) async fn start(
    paths: &CommandPaths,
    entry: &QueueEntryId,
    plan: ChatPlan,
) -> Result<()> {
    crate::env::launch_queue_entry(entry);
    let ChatPlanStart::Resolved(start) = plan.chat_start;
    crate::chat_starts::start_queued(paths, *start).await
}
