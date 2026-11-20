//! The forges the runner registers on and acts on, as the user.

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_forge::{RepoRequests, account::Account};
use shoal_runner::daemon::Forges;

use crate::{
    daemon::manager::Manager,
    forge::{ForgeRepo, items::reference::ItemRef},
    model::Repository,
};

impl Forges for Manager {
    fn user_account(&self) -> Account {
        self.links.user_account()
    }

    fn checkout_forge<'a>(
        &'a self,
        repository: &'a Repository,
    ) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(Manager::checkout_forge(self, repository))
    }

    fn resolve_forge<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async move { ForgeRepo::resolve(url, &self.forge_hosts()).await })
    }

    fn item_forge<'a>(&'a self, item: &'a ItemRef) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(Manager::item_forge(self, item))
    }
}
