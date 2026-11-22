//! The repository a request about issues, PRs or CI runs acts in: that of
//! the space or registered checkout it comes from, or another one its
//! `--repo` or an item's, run's or workflow's URL names, registered or not.
//! Every caller reads another repository; only the user changes one.
use anyhow::{Result, bail, ensure};

use shoal_daemon_base::callers::Caller;
use shoal_forge::transport::account::Account;
use shoal_forge_types::{
    ForgeRepo,
    items::{
        link::ItemKind,
        reference::{ItemInput, ItemRef},
    },
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::protocol::{ItemContext, RepoTarget};

use crate::{
    Links,
    daemon::Daemon,
    forges,
    place::{Access, Place},
};

/// The refusal of a scoped caller's change in another repository than its
/// own.
pub const ELSEWHERE: &str = "agents change issues, PRs and CI runs only in \
     their own repository; ask the user to change those of another repository";

/// What a request names when nothing gives it a repository.
const NO_REPOSITORY: &str = "name the repository with --repo, or the item by its URL";

/// The refusal of a URL of another repository than the one `--repo` names.
const CONFLICTING: &str = "the URL names another repository than --repo; leave out --repo";

/// The repository a request acts in and where the request came from.
pub struct ForgeTarget {
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
    pub fn forge(&self) -> Result<&ForgeRepo> {
        (self.forge.as_ref()).map_err(|error| anyhow::anyhow!("{error:#}"))
    }

    /// [`Self::forge`], taken.
    pub fn into_forge(self) -> Result<ForgeRepo> {
        self.forge
    }

    /// The place the request acts in as its own repository.
    pub fn own_place(&self) -> Option<&Place> {
        self.origin.as_ref().filter(|_| self.own)
    }
}

impl Links {
    /// The repository a request from `target` acts in with `access`: the
    /// one `target` names, else `named`, the one an item's, run's or
    /// workflow's URL names, else the context's own. A scoped `caller`
    /// changes only its own repository.
    pub async fn forge_target(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        named: Option<ForgeRepo>,
        access: Access,
        caller: Option<&Caller>,
    ) -> Result<ForgeTarget> {
        let origin = match &target.context {
            Some(context) => self.origin(daemon, context, access).await?,
            None => None,
        };
        let own_forge = match &origin {
            Some(place) => forges::origin(daemon, place.origin_path()).await,
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
                let forge = target_named(daemon, own_forge.as_ref().ok(), repository);
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
    pub async fn target_item(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        input: Option<ItemInput>,
        kind: ItemKind,
        access: Access,
        caller: Option<&Caller>,
    ) -> Result<(ForgeTarget, ItemRef)> {
        let named = match &input {
            Some(input) => forges::of_input(daemon, input).await?,
            None => None,
        };
        let target = (self.forge_target(daemon, target, named, access, caller)).await?;
        let space = target.own_place().and_then(Place::space_id).cloned();
        let item = named_or_linked(daemon, input, space.as_ref(), target.forge()?, kind);
        let item = item.await?;
        Ok((target, item))
    }

    /// The account a request acts as in `target`: that of the place it
    /// comes from in its own repository, and the [`elsewhere_account`]
    /// in another.
    ///
    /// [`elsewhere_account`]: Self::elsewhere_account
    pub fn target_account(
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
    pub fn elsewhere_account(&self, access: Access, caller: Option<&Caller>) -> Account {
        match access {
            Access::Read => self.read_account(caller),
            Access::Change => self.user_account(),
        }
    }

    /// Where `context` comes from, or nowhere for a chat without a
    /// repository naming none it acts in.
    async fn origin(
        &self,
        daemon: &dyn Daemon,
        context: &ItemContext,
        access: Access,
    ) -> Result<Option<Place>> {
        if let ItemContext::Space {
            space,
            repository: None,
        } = context
            && daemon.space(space).await?.repository_id.is_none()
        {
            return Ok(None);
        }
        self.place(daemon, context, access).await.map(Some)
    }
}

/// The repository `named` names: by URL on any forge, or as OWNER/NAME
/// on the forge of `own`, the repository the request comes from.
async fn target_named(
    daemon: &dyn Daemon,
    own: Option<&ForgeRepo>,
    named: &str,
) -> Result<ForgeRepo> {
    match own {
        Some(own) => forges::named(daemon, own.clone(), Some(named)).await,
        None if ForgeRepo::named_by_url(named) => daemon.resolve_forge(named).await,
        None => bail!(
            "{named} names a repository on the current repository's forge, and there is \
             none here; name it by its URL"
        ),
    }
}

/// The `kind` item `named` names in the repository of `forge`, else the
/// one item of `kind` assigned to the space `space`.
pub async fn named_or_linked(
    daemon: &dyn Daemon,
    named: Option<ItemInput>,
    space: Option<&SpaceId>,
    forge: &ForgeRepo,
    kind: ItemKind,
) -> Result<ItemRef> {
    match (named, space) {
        (Some(named), _) => ItemRef::in_repository(forge, kind.into(), &named),
        (None, Some(id)) => {
            let linked = only_assigned(kind, daemon.linked_items(id, kind).await?)?;
            linked.belongs_to(forge)?;
            Ok(linked)
        }
        (None, None) => bail!(NOTHING_LINKED),
    }
}

/// Why an action outside a space needs its item named.
pub const NOTHING_LINKED: &str =
    "outside a workspace or chat, nothing is linked; pass a number or URL";

/// The one assigned item of `kind` among `assigned`.
fn only_assigned(kind: ItemKind, assigned: Vec<ItemRef>) -> Result<ItemRef> {
    let noun = kind.noun();
    match assigned.as_slice() {
        [url] => Ok(url.clone()),
        [] => bail!("no {noun} is linked; pass a number or URL"),
        _ => bail!("several {noun}s are linked; pass a number or URL"),
    }
}

#[cfg(test)]
mod tests;
