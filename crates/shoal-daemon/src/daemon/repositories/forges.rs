//! The forges a contribution forks on.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_forge::account::Account;
use shoal_repositories::daemon::Forges;

use crate::{daemon::manager::Manager, forge::ForgeRepo};

impl Forges for Manager {
    fn forge<'a>(&'a self, remote: &'a str) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(Manager::forge(self, remote))
    }

    fn user_account(&self) -> Account {
        self.links.user_account()
    }
}
