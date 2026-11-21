//! The places in code posts point at: made from paths in worktrees,
//! followed as the code changes, with the file versions they keep in the
//! repository.
use std::collections::{BTreeMap, BTreeSet};

use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::code::MAX_VERSIONED_LEN;

mod following;
mod recording;
pub(super) mod records;
mod versions;

pub(in crate::daemon) use following::Followed;
pub(super) use recording::NewCode;
pub(in crate::daemon) use recording::{canonical, relative};

/// `bytes` as text a reference keeps versions of: UTF-8 without NUL, of
/// at most [`MAX_VERSIONED_LEN`] bytes.
fn kept_text(bytes: Vec<u8>) -> Option<String> {
    let text = String::from_utf8(bytes).ok()?;
    (text.len() <= MAX_VERSIONED_LEN && !text.contains('\0')).then_some(text)
}

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
