//! The forge of the repository a space, a name or an item names. Reads of a
//! checkout run under the file-access stall guard.
use anyhow::{Context, Result};
use shoal_core::repository::remote_url_from_path;
use shoal_forge_types::{
    ForgeRepo,
    items::{
        link::LinkKind,
        reference::{ItemInput, ItemRef},
    },
};

use shoal_daemon_base::forge_access::ForgeAccess;

use crate::{Links, space::LinkSpace};

/// The forge of the space's `origin`.
pub async fn of_space(forges: &ForgeAccess, space: &LinkSpace) -> Result<ForgeRepo> {
    forges.origin_forge(space.origin_path()).await
}

/// The repository `named` names, as OWNER/NAME on the forge of `own` or
/// as a repository URL on any forge, or `own` without one.
pub async fn named(forges: &ForgeAccess, own: ForgeRepo, named: Option<&str>) -> Result<ForgeRepo> {
    match named {
        Some(named) => forges.named_forge(&own, named).await,
        None => Ok(own),
    }
}

/// The repository the issue, PR or CI run URL `input` names, or none for a
/// number.
pub async fn of_input(forges: &ForgeAccess, input: &ItemInput) -> Result<Option<ForgeRepo>> {
    Ok(match input {
        ItemInput::Url(item) => Some(forges.item_forge(item).await?),
        ItemInput::Number(_) => None,
    })
}

impl Links {
    /// The forge of the repository whose origin `checkout` has, with the PR
    /// `input` names there.
    pub async fn pr_forge(
        &self,
        checkout: &std::path::Path,
        input: &ItemInput,
    ) -> Result<(ForgeRepo, ItemRef)> {
        let forge = (self.guarded_forge(checkout, "PR lookup needs an origin remote")).await?;
        let pull = ItemRef::in_repository(&forge, LinkKind::Pr, input)?;
        Ok((forge, pull))
    }

    /// The forge of the repository whose origin `checkout` has, reading the
    /// origin under the file-access stall guard; `missing` is the error
    /// without one.
    pub async fn guarded_forge(
        &self,
        checkout: &std::path::Path,
        missing: &'static str,
    ) -> Result<ForgeRepo> {
        let remote = self
            .file_access
            .checked(
                &self.notifications,
                checkout,
                remote_url_from_path(checkout),
            )
            .await?
            .context(missing)?;
        self.forges.forge(&remote).await
    }
}
