//! Who acts for a space from above: whether it has an agent of its own,
//! which passes its events up, and the spaces that decide its requests.
use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::space_ids::SpaceId;
use shoal_store::tree::{self, TrackedAgents};

/// Whether the space `id` has an agent among `agents`.
pub fn has_running_agent(db: &Connection, id: &SpaceId, agents: &TrackedAgents) -> Result<bool> {
    shoal_store::exists(
        db,
        &format!("SELECT 1 WHERE {}", tree::has_agent("?1")),
        params![id, agents.parameter()],
    )
}

/// The spaces that decide the requests of the space `id`, nearest first:
/// those above it with an agent, then `elsewhere`, those on the machines
/// above. A space without an agent of its own has none, since its
/// coordinator or the user acts there.
pub fn deciders(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
    elsewhere: Vec<SpaceId>,
) -> Result<Vec<SpaceId>> {
    if !has_running_agent(db, id, agents)? {
        return Ok(Vec::new());
    }
    let mut deciders = tree::spaces_above(db, id, agents)?;
    deciders.extend(elsewhere);
    Ok(deciders)
}
