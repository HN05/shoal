//! What each forge does its own way with a repository's branches: listing
//! them and the open PRs that join them.
use anyhow::{Context, Result};
use serde_json::Value;

use crate::{account::Account, backends::Backend};
use shoal_forge_types::{
    ForgeRepo,
    backends::ForgeKind,
    branches::{ForgeBranch, OpenPull},
};

/// How the forge lists a repository's branches.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Branches: Backend {
    /// The repository's branches.
    async fn branches(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<ForgeBranch>>;

    /// The path under the repository that lists its open PRs.
    fn open_pulls_path(&self) -> &'static str;

    /// The open PR of `repo` a listing of them returned as `value`.
    fn open_pull(&self, repo: &ForgeRepo, value: &Value) -> Result<OpenPull>;

    /// The open PRs from branches of `repo` into the repository it is a
    /// fork of, which its own listing leaves out; none for a repository that
    /// is no fork.
    async fn outgoing_pulls(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<OpenPull>>;
}

/// Where a forge's listing of branches puts what Shoal reads of each, as
/// JSON pointers; none for what it leaves out.
pub(crate) struct BranchFields {
    pub name: &'static str,
    pub head: &'static str,
    pub committed_at: Option<&'static str>,
}

impl BranchFields {
    /// The branch the listing returned as `value`, `protected` as the
    /// forge says it is.
    pub(crate) fn read(&self, value: &Value, protected: bool) -> Result<ForgeBranch> {
        let text = |pointer| value.pointer(pointer).and_then(Value::as_str);
        let name = text(self.name).context("the forge listed a branch without a name")?;
        let head = (text(self.head))
            .with_context(|| format!("the forge listed branch {name} without its commit"))?;
        Ok(ForgeBranch {
            name: name.into(),
            head: head.into(),
            committed_at: self.committed_at.and_then(text).map(Into::into),
            protected,
        })
    }
}
