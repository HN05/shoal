//! The columns of a new workspace's row that hold its head and checkout, as
//! the store's workspace mapper reads them back.
use anyhow::Result;
use shoal_refs::{BranchName, CommitId};

use crate::model::{Checkout, Head};
use crate::store;

/// The `base_commit`, `base_ref` and `git_dir` columns that hold what
/// `checkout` records.
pub(super) fn checkout_columns(
    checkout: &Checkout,
) -> (Option<&CommitId>, Option<&str>, Option<String>) {
    let base = checkout.base();
    (
        base.map(|base| &base.commit),
        base.and_then(|base| base.reference.as_deref()),
        (checkout.git_dir()).and_then(|path| path.to_str().map(str::to_owned)),
    )
}

/// The `branch`, `start_commit` and `made_for` columns that hold `head`.
pub(super) fn head_columns(
    head: &Head,
) -> Result<(Option<&BranchName>, Option<&CommitId>, Option<String>)> {
    Ok(match head {
        Head::Branch(branch) => (Some(branch), None, None),
        Head::Commit(at) => (
            None,
            Some(&at.commit),
            store::json_text(at.made_for.as_ref())?,
        ),
    })
}
