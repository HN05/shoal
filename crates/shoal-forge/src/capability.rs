//! What each forge supports. A request that needs a capability the forge
//! lacks is refused before anything is sent.
use anyhow::Result;

use crate::transport::account::Account;
use shoal_forge_types::{
    ForgeRepo,
    backends::{ForgeKind, Routes},
    capability::{Capabilities, Capability, Entry, Unsupported},
};

/// What `shoal pr revert` does on a forge that cannot revert PRs itself.
pub(crate) const GIT_REVERT: &str =
    "shoal pr revert opens the revert from a new workspace with git revert";

/// What to search with on a forge that cannot search by meaning.
pub(crate) const KEYWORD_SEARCH: &str = "search by keyword with --match keyword, or with --match both, which matches by keyword alone there";

/// Whether a forge has a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Support {
    Supported,
    /// With what to do instead, or why, when the forge's way differs.
    Unsupported(Option<&'static str>),
}

/// What a forge supports, which each forge declares.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Supports: Routes {
    /// Whether the forge has `capability`.
    fn support(&self, capability: Capability) -> Support;

    /// Whether the server `repo` is on has `capability`, where the forge's
    /// versions differ in it.
    async fn server_support(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        capability: Capability,
    ) -> Result<Support> {
        Ok(self.support(capability))
    }
}

/// Refuse `capability` unless `backend` has it.
pub(crate) fn require(backend: &(impl Supports + ?Sized), capability: Capability) -> Result<()> {
    match backend.support(capability) {
        Support::Supported => Ok(()),
        Support::Unsupported(_) => Err(unsupported(backend, capability)),
    }
}

/// The refusal of `capability`, also for a backend method that cannot carry
/// out a request needing it.
pub(crate) fn unsupported(
    backend: &(impl Supports + ?Sized),
    capability: Capability,
) -> anyhow::Error {
    let hint = match backend.support(capability) {
        Support::Unsupported(hint) => hint,
        Support::Supported => None,
    };
    Unsupported {
        forge: backend.name(),
        capability,
        hint,
    }
    .into()
}

/// What the repository's forge supports.
#[shoal_forge_derive::extension(pub CapabilityRequests, pub(crate) CapabilitySteps)]
impl ForgeRepo {
    /// Refuse a request that needs `capability` when the forge lacks it.
    pub fn require(&self, capability: Capability) -> Result<()> {
        require(&self.kind(), capability)
    }

    /// Refuse a request that needs any of `capabilities` the forge lacks.
    pub fn require_all(&self, capabilities: impl IntoIterator<Item = Capability>) -> Result<()> {
        capabilities
            .into_iter()
            .try_for_each(|capability| self.require(capability))
    }

    /// Refuse a request that needs `capability` when the repository's
    /// server lacks it, asking the server where the forge's versions differ.
    pub async fn require_on_server(&self, account: &Account, capability: Capability) -> Result<()> {
        match self
            .kind()
            .server_support(self, account, capability)
            .await?
        {
            Support::Supported => Ok(()),
            Support::Unsupported(hint) => Err(Unsupported {
                forge: self.kind().name(),
                capability,
                hint,
            }
            .into()),
        }
    }

    pub fn supports(&self, capability: Capability) -> bool {
        self.kind().support(capability) == Support::Supported
    }

    pub fn capabilities(&self) -> Capabilities {
        self.listed(
            Capability::ALL
                .iter()
                .map(|&capability| (capability, self.kind().support(capability))),
        )
    }

    /// The capabilities of the repository's server, asking it where the
    /// forge's versions differ.
    pub async fn server_capabilities(&self, account: &Account) -> Result<Capabilities> {
        let mut supports = Vec::new();
        for &capability in Capability::ALL {
            let support = self
                .kind()
                .server_support(self, account, capability)
                .await?;
            supports.push((capability, support));
        }
        Ok(self.listed(supports))
    }

    fn listed(&self, supports: impl IntoIterator<Item = (Capability, Support)>) -> Capabilities {
        Capabilities {
            forge: self.kind().name().to_owned(),
            capabilities: supports
                .into_iter()
                .map(|(capability, support)| {
                    let (supported, hint) = match support {
                        Support::Supported => (true, None),
                        Support::Unsupported(hint) => (false, hint),
                    };
                    Entry {
                        capability,
                        supported,
                        hint: hint.map(str::to_owned),
                    }
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests;
