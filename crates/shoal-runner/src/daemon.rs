//! What the triggers need of the rest of the daemon, which implements it:
//! the registered repositories and their config, the agents a trigger's
//! launch meets and starts, and the permission a scoped caller acts with.
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_core::config::repo::ConfigLayers;
use shoal_daemon_base::callers::Caller;
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    config::permissions::PermissionKey,
    launches::{Launch, LaunchSource},
    model::Repository,
    protocol::ItemContext,
};

/// The registered repositories, and the config the daemon reads for them.
pub trait Repositories: Send + Sync {
    /// The registered repositories.
    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>>;

    /// The registered repository `id`.
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>>;

    /// The config layers of the registered repository `id`: its
    /// checkout's file and its local config.
    fn repository_layers<'a>(&'a self, id: &'a RepositoryId)
    -> BoxFuture<'a, Result<ConfigLayers>>;

    /// The registered repository a request made in `context` acts in: a
    /// workspace's or a chat's, or the checkout's; none without a context
    /// or for a chat without a repository.
    fn context_repository(
        &self,
        context: Option<ItemContext>,
    ) -> BoxFuture<'_, Result<Option<RepositoryId>>>;

    /// Whether this daemon acts on what the global config sets up for the
    /// machine, such as its global triggers and its webhook listener.
    fn acts_on_global_config(&self) -> bool;
}

/// The agents a trigger's launch starts, and those it waits for.
pub trait Agents: Send + Sync {
    /// The agent queue's entries that wait or start, not those that failed.
    fn live_queue_entries(&self) -> BoxFuture<'_, Result<HashSet<QueueEntryId>>>;

    /// The space the queued launch `entry` opened, once it did.
    fn started_launch<'a>(
        &'a self,
        entry: &'a QueueEntryId,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>>;

    /// The spaces whose agent runs or waits in the agent queue.
    fn agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>>;

    /// Launch `launch` as the user, through the operation `shoal start`
    /// uses, from `source`: the space it opens.
    fn launch(
        self: Arc<Self>,
        launch: Launch,
        source: LaunchSource,
    ) -> BoxFuture<'static, Result<SpaceId>>;
}

/// The permissions scoped callers act with.
pub trait Permissions: Send + Sync {
    /// Let `caller` take `action`, which `key` governs, in `repository`:
    /// an error refuses it, or makes it an approval request.
    fn authorize<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        key: PermissionKey,
        action: String,
        repository: Option<RepositoryId>,
    ) -> BoxFuture<'a, Result<()>>;
}

/// Everything the triggers need of the daemon, which each of their
/// operations that reaches beyond them takes.
pub trait Daemon: Repositories + Agents + Permissions {}

impl<T> Daemon for T where T: Repositories + Agents + Permissions {}
