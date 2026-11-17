//! The repository a request about issues, PRs or CI runs acts in: that of
//! the space or registered checkout it comes from, or another one its
//! `--repo` or an item's, run's or workflow's URL names, registered or not.
//! Every caller reads another repository; only the user changes one.
use anyhow::{Context, Result, bail, ensure};

use super::place::{Access, Place};
use crate::{
    daemon::{scope::Caller, workspace::Manager},
    forge::ForgeRepo,
    protocol::{ItemContext, RepoTarget},
};
use shoal_forge::RepoRequests;

/// The refusal of a scoped caller's change in another repository than its
/// own.
pub(in crate::daemon) const ELSEWHERE: &str = "agents change issues, PRs and CI runs only in \
     their own repository; ask the user to change those of another repository";

/// What a request names when nothing gives it a repository.
const NO_REPOSITORY: &str = "name the repository with --repo, or the item by its URL";

/// The repository a request acts in and where the request came from.
pub(in crate::daemon::links) struct ForgeTarget {
    /// The space or registered checkout the request came from, if any.
    pub origin: Option<Place>,
    pub forge: ForgeRepo,
    /// Whether `forge` is the repository of `origin`.
    pub own: bool,
}

impl ForgeTarget {
    /// The place the request acts in as its own repository.
    pub(in crate::daemon::links) fn own_place(&self) -> Option<&Place> {
        self.origin.as_ref().filter(|_| self.own)
    }
}

impl Manager {
    /// The repository a request from `target` acts in with `access`: the
    /// one `target` names, else `named`, the one an item's, run's or
    /// workflow's URL names, else the context's own. A scoped `caller`
    /// changes only its own repository.
    pub(in crate::daemon::links) async fn forge_target(
        &self,
        target: &RepoTarget,
        named: Option<ForgeRepo>,
        access: Access,
        caller: Option<&Caller>,
    ) -> Result<ForgeTarget> {
        let origin = match &target.context {
            Some(context) => self.origin(context, access).await?,
            None => None,
        };
        let own = match &origin {
            Some(place) => Some(self.place_forge(place).await?),
            None => None,
        };
        let forge = match (target.repository.as_deref(), named) {
            (Some(repository), _) => self.target_named(own.as_ref(), repository).await?,
            (None, Some(named)) => named,
            (None, None) => own.clone().context(NO_REPOSITORY)?,
        };
        let own = own.as_ref() == Some(&forge);
        ensure!(own || access == Access::Read || caller.is_none(), ELSEWHERE);
        Ok(ForgeTarget { origin, forge, own })
    }

    /// Where `context` comes from, or nowhere for a chat without a
    /// repository naming none it acts in.
    async fn origin(&self, context: &ItemContext, access: Access) -> Result<Option<Place>> {
        if let ItemContext::Space {
            space,
            repository: None,
        } = context
            && self.space(space).await?.repository_id.is_none()
        {
            return Ok(None);
        }
        self.place(context, access).await.map(Some)
    }

    /// The repository `named` names: by URL on any forge, or as OWNER/NAME
    /// on the forge of `own`, the repository the request comes from.
    async fn target_named(&self, own: Option<&ForgeRepo>, named: &str) -> Result<ForgeRepo> {
        match own {
            Some(own) => self.named_forge(own.clone(), Some(named)).await,
            None if ForgeRepo::named_by_url(named) => {
                ForgeRepo::resolve(named, &self.forge_hosts()).await
            }
            None => bail!(
                "{named} names a repository on the current repository's forge, and there is \
                 none here; name it by its URL"
            ),
        }
    }
}
