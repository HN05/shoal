//! The places in code posts point at: made from paths in worktrees,
//! followed as the code changes, with the file versions they keep in the
//! repository.
use std::collections::{BTreeMap, BTreeSet};

use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};

mod recording;
pub(super) mod records;
mod versions;

pub(super) use recording::NewCode;

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
