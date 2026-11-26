//! The places in code that posts point at.
use std::collections::{BTreeMap, BTreeSet};

use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::code::kept_text;

mod following;
mod recording;
pub(super) mod records;
mod versions;

pub use following::Followed;
pub(super) use recording::NewCode;
pub use recording::{canonical, relative};

/// The spaces whose versions `code` keeps, by repository.
pub(super) fn versioned(code: &[NewCode]) -> BTreeMap<RepositoryId, BTreeSet<SpaceId>> {
    let mut versioned: BTreeMap<RepositoryId, BTreeSet<SpaceId>> = BTreeMap::new();
    for code in code {
        (versioned.entry(code.repository_id.clone()).or_default()).insert(code.space_id.clone());
    }
    versioned
}

#[cfg(test)]
mod tests;
