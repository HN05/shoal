//! The permission a scoped caller acts with, as links ask for it.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_daemon_base::permitted_actions::Action;
use shoal_links::daemon::Permissions;

use shoal_ids::repository_ids::RepositoryId;

use crate::{
    config::permissions::{self, PermissionKey},
    daemon::{manager::Manager, scope::Caller},
};

impl Permissions for Manager {
    fn authorize_action<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        action: Action,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::authorize_action(self, caller, action))
    }

    fn caller_permissions<'a>(
        &'a self,
        caller: &'a Caller,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<permissions::Permissions>> {
        Box::pin(Manager::caller_permissions(self, caller, repository))
    }

    fn authorize_actions<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        actions: Vec<Action>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::authorize_actions(self, caller, actions))
    }

    fn refuse_denied<'a>(
        &'a self,
        caller: &'a Caller,
        key: PermissionKey,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::refuse_denied(self, caller, key, repository))
    }
}
