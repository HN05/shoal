//! The repository a request about issues, PRs or CI runs acts in: that of
//! the space or registered checkout it comes from, or another one its
//! `--repo` or an item's, run's or workflow's URL names, registered or not.
//! Every caller reads another repository; only the user changes one.
use anyhow::{Context, Result, bail, ensure};

use super::place::{Access, Place};
use crate::{
    daemon::{scope::Caller, workspace::Manager},
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

/// Why a request in its own repository reaches no forge.
const NO_ORIGIN: &str = "forge actions need an origin remote";

/// The repository a request acts in and where the request came from.
pub(in crate::daemon::links) struct ForgeTarget {
    /// The space or registered checkout the request came from, if any.
    pub origin: Option<Place>,
    /// None in the request's own repository when its checkout or worktree
    /// has no `origin`, which leaves it only what it links.
    forge: Option<ForgeRepo>,
    /// Whether the request acts in the repository of `origin`.
    pub own: bool,
}

impl ForgeTarget {
    /// The forge of the repository the request acts in.
    pub(in crate::daemon::links) fn forge(&self) -> Result<&ForgeRepo> {
        self.forge.as_ref().context(NO_ORIGIN)
    }

    /// [`Self::forge`], taken.
    pub(in crate::daemon::links) fn into_forge(self) -> Result<ForgeRepo> {
        self.forge.context(NO_ORIGIN)
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
        let own = match &origin {
            Some(place) => self.origin_forge_if_any(place).await?,
            None => None,
        };
        let forge = match (target.repository.as_deref(), named) {
            (Some(repository), _) => Some(self.target_named(own.as_ref(), repository).await?),
            (None, Some(named)) => Some(named),
            (None, None) if origin.is_some() => own.clone(),
            (None, None) => bail!(NO_REPOSITORY),
        };
        let own = forge.is_none() || own == forge;
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
            Some(input) => self.input_forge(input).await?,
            None => None,
        };
        let target = self.forge_target(target, named, access, caller).await?;
        let space = target.own_place().and_then(Place::space_id).cloned();
        let item = self.named_or_linked(input, space.as_ref(), target.forge()?, kind);
        let item = item.await?;
        Ok((target, item))
    }

    /// The repository the issue, PR or CI run URL `input` names, or none
    /// for a number.
    pub(in crate::daemon) async fn input_forge(
        &self,
        input: &ItemInput,
    ) -> Result<Option<ForgeRepo>> {
        Ok(match input {
            ItemInput::Url(item) => Some(self.item_forge(item).await?),
            ItemInput::Number(_) => None,
        })
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
            Access::Change => self.user_account(),
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

    /// The forge of the place's `origin`, or none for a checkout or
    /// worktree without one, which has no repository on a forge to act in.
    async fn origin_forge_if_any(&self, place: &Place) -> Result<Option<ForgeRepo>> {
        let remote = crate::repository::remote_url_from_path(place.origin_path()).await?;
        match remote {
            Some(remote) => Ok(Some(self.forge(&remote).await?)),
            None => Ok(None),
        }
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
