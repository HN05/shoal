//! The forges links resolve repositories on, by the forge each host serves.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_forge::{
    RepoRequests,
    ci::workflow::{WorkflowLink, WorkflowRequests},
    items::reference::ReferenceRequests,
};
use shoal_links::daemon::Forges;

use crate::{
    daemon::manager::Manager,
    forge::{ForgeRepo, items::reference::ItemRef},
};

impl Forges for Manager {
    fn resolve_forge<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async move { ForgeRepo::resolve(url, &self.forge_hosts()).await })
    }

    fn resolve_named<'a>(
        &'a self,
        own: &'a ForgeRepo,
        named: &'a str,
    ) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async move { own.resolve_named(named, &self.forge_hosts()).await })
    }

    fn resolve_workflow<'a>(
        &'a self,
        url: &'a str,
    ) -> BoxFuture<'a, Result<(ForgeRepo, WorkflowLink)>> {
        Box::pin(async move { ForgeRepo::resolve_workflow_url(url, &self.forge_hosts()).await })
    }

    fn item_forge<'a>(&'a self, item: &'a ItemRef) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async move { item.forge(&self.forge_hosts()).await })
    }
}
