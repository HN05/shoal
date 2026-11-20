//! What links need of the rest of the daemon, which implements it: which
//! forge each host serves.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_forge_types::{ForgeRepo, items::reference::ItemRef};

/// The forges repositories are on, by the forge each host serves.
pub trait Forges: Send + Sync {
    /// The repository the remote or web URL `url` names.
    fn resolve_forge<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The repository `named` names, as OWNER/NAME on the forge of `own`
    /// or as a repository URL on any forge.
    fn resolve_named<'a>(
        &'a self,
        own: &'a ForgeRepo,
        named: &'a str,
    ) -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The forge the repository of `item` is on.
    fn item_forge<'a>(&'a self, item: &'a ItemRef) -> BoxFuture<'a, Result<ForgeRepo>>;
}
