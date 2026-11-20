//! The permission a scoped caller acts with, as links ask for it.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_daemon_base::permitted_actions::Action;
use shoal_links::daemon::Permissions;

use crate::daemon::{manager::Manager, scope::Caller};

impl Permissions for Manager {
    fn authorize_action<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        action: Action,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::authorize_action(self, caller, action))
    }
}
