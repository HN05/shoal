//! Whether a space, or one below it whose activity its agent receives,
//! links an item of a kind, which walks the space tree.
use anyhow::Result;
use shoal_links::wait::followed::followed_items;

use crate::forge::items::link::LinkKind;
use shoal_ids::space_ids::SpaceId;

/// Whether the workspace, or a child workspace whose activity its agent
/// receives, links an item of `kind`; every workspace has items of no kind.
pub(in crate::daemon::links) fn links_kind(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<LinkKind>,
    agents: &crate::daemon::tree::TrackedAgents,
) -> Result<bool> {
    if kind.is_none() || !followed_items(db, id, kind)?.is_empty() {
        return Ok(true);
    }
    for child in crate::daemon::tree::delegated(db, id, agents)? {
        if !followed_items(db, &child.id, kind)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}
