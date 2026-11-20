//! The permission a scoped caller acts with.

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_daemon_base::permitted_actions::Action;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::config::permissions::PermissionKey;
use shoal_runner::daemon::Permissions;

use crate::daemon::{manager::Manager, scope::Caller};

impl Permissions for Manager {
    fn authorize<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        key: PermissionKey,
        action: String,
        repository: Option<RepositoryId>,
    ) -> BoxFuture<'a, Result<()>> {
        let mut authorized = Action::new(key, action);
        authorized.repository = repository;
        Box::pin(self.authorize_action(caller, authorized))
    }
}
