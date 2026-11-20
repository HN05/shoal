//! The repository a request about issues, PRs or CI runs acts in: that of
//! the space or registered checkout it comes from, or another one its
//! `--repo` or an item's, run's or workflow's URL names, registered or not.
//! Every caller reads another repository; only the user changes one.
use anyhow::{Result, bail, ensure};

use super::place::{Access, Place};
use crate::{
    daemon::{manager::Manager, scope::Caller},
    forge::{
        ForgeRepo,
        items::{
            link::ItemKind,
            reference::{ItemInput, ItemRef},
        },
    },
    protocol::{ItemContext, RepoTarget},
};
use shoal_forge::RepoRequests;
use shoal_forge::account::Account;

/// The refusal of a scoped caller's change in another repository than its
/// own.
pub(in crate::daemon) const ELSEWHERE: &str = "agents change issues, PRs and CI runs only in \
     their own repository; ask the user to change those of another repository";

/// What a request names when nothing gives it a repository.
const NO_REPOSITORY: &str = "name the repository with --repo, or the item by its URL";

/// The refusal of a URL of another repository than the one `--repo` names.
const CONFLICTING: &str = "the URL names another repository than --repo; leave out --repo";

/// The repository a request acts in and where the request came from.
pub(in crate::daemon::links) struct ForgeTarget {
    /// The space or registered checkout the request came from, if any.
    pub origin: Option<Place>,
    /// An error in the request's own repository when its checkout or
    /// worktree has no `origin` on a forge, which leaves it only what it
    /// links.
    forge: Result<ForgeRepo>,
    /// Whether the request acts in the repository of `origin`.
    pub own: bool,
}

impl ForgeTarget {
    /// The forge of the repository the request acts in.
    pub(in crate::daemon::links) fn forge(&self) -> Result<&ForgeRepo> {
        (self.forge.as_ref()).map_err(|error| anyhow::anyhow!("{error:#}"))
    }

    /// [`Self::forge`], taken.
    pub(in crate::daemon::links) fn into_forge(self) -> Result<ForgeRepo> {
        self.forge
    }

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
        let own_forge = match &origin {
            Some(place) => shoal_links::forges::origin(self, place.origin_path()).await,
            None => Err(anyhow::anyhow!(NO_REPOSITORY)),
        };
        // The origin's forge is needed only where something else is named
        // to compare it with, or where the request reaches the forge.
        let (forge, own) = match (target.repository.as_deref(), named) {
            (None, None) => {
                ensure!(origin.is_some(), NO_REPOSITORY);
                (own_forge, true)
            }
            (Some(repository), named) => {
                let forge = self.target_named(own_forge.as_ref().ok(), repository);
                let forge = forge.await?;
                ensure!(named.is_none_or(|named| named == forge), CONFLICTING);
                let own = own_forge.as_ref().ok() == Some(&forge);
                (Ok(forge), own)
            }
            (None, Some(named)) => {
                let own = own_forge.as_ref().ok() == Some(&named);
                (Ok(named), own)
            }
        };
        ensure!(own || access == Access::Read || caller.is_none(), ELSEWHERE);
        Ok(ForgeTarget { origin, forge, own })
    }

    /// The `kind` item `input` names in the `target` repository, by number
    /// or URL, else the one item of `kind` assigned to the space the
    /// request comes from in its own repository, with where it is.
    pub(in crate::daemon::links) async fn target_item(
        &self,
        target: &RepoTarget,
        input: Option<ItemInput>,
        kind: ItemKind,
        access: Access,
        caller: Option<&Caller>,
    ) -> Result<(ForgeTarget, ItemRef)> {
        let named = match &input {
            Some(input) => shoal_links::forges::of_input(self, input).await?,
            None => None,
        };
        let target = self.forge_target(target, named, access, caller).await?;
        let space = target.own_place().and_then(Place::space_id).cloned();
        let item = self.named_or_linked(input, space.as_ref(), target.forge()?, kind);
        let item = item.await?;
        Ok((target, item))
    }

    /// The account a request acts as in `target`: that of the place it
    /// comes from in its own repository, and the [`elsewhere_account`]
    /// in another.
    ///
    /// [`elsewhere_account`]: Self::elsewhere_account
    pub(in crate::daemon::links) fn target_account(
        &self,
        target: &ForgeTarget,
        access: Access,
        caller: Option<&Caller>,
    ) -> Account {
        match target.own_place() {
            Some(place) => self.place_account(place),
            None => self.elsewhere_account(access, caller),
        }
    }

    /// The account a request acts as in `target` when that is not its own
    /// repository: the caller's to read, and the user's own logins for a
    /// change, which only the user makes there.
    pub(in crate::daemon::links) fn elsewhere_account(
        &self,
        access: Access,
        caller: Option<&Caller>,
    ) -> Account {
        match access {
            Access::Read => self.read_account(caller),
            Access::Change => self.links.user_account(),
        }
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
            Some(own) => shoal_links::forges::named(self, own.clone(), Some(named)).await,
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
