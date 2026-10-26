//! Forks: where an account that cannot push to a repository pushes its
//! branches for PRs instead.
use anyhow::{Context, Result};

use crate::{
    ForgeRepo, account::Account, backends::Backend, capability::Capability,
    repo::remote_url::RemoteUrl,
};

impl ForgeRepo {
    /// Whether `account` may push branches to the repository.
    pub async fn pushable(&self, account: &Account) -> Result<bool> {
        self.kind.pushable(self, account).await
    }

    /// `account`'s fork of the repository, created when it has none.
    pub async fn fork(&self, account: &Account) -> Result<ForgeRepo> {
        self.require(Capability::Forks)?;
        let path = self.kind.fork(self, account).await?;
        Ok(self.sibling(&path))
    }

    /// The URL of this repository written like `remote`, a URL of another
    /// repository on the same forge.
    pub fn remote_like(&self, remote: &str) -> Result<String> {
        RemoteUrl::parse(remote)
            .and_then(|url| url.with_path(&self.path))
            .with_context(|| format!("{remote} names no repository path"))
    }
}

#[cfg(test)]
mod tests;
