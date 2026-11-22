//! What the agents need of the rest of the daemon, which implements it: the
//! spaces they run in and their settings, the repositories whose paths they
//! trust, the executions that run them, the
//! alerts that tell of them, what else ends with an agent's turn, the agent
//! queue, and the records of other subsystems they read and write in their
//! own transactions.
use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_core::{config::Effective, process::identity::AgentTrees};
use shoal_daemon_base::alerts::AlertKind;
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::agent::Agent;
use shoal_protocol::{
    model::{Repository, Space, Workspace},
    space_names::{SpaceName, SpaceSelector},
};

/// The spaces agents run in.
pub trait Spaces: Send + Sync {
    /// The workspace or chat `selector` names, by ID or name.
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>>;

    /// The workspace or chat with the ID `id`.
    fn space_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>>;

    /// Whether the space `id` exists.
    fn space_exists<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// The agent the space `id` runs, or last ran, as recorded.
    fn agent_of<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Agent>>>;

    /// The settings of `space`, as its config layers resolve them.
    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>>;

    /// The workspace `selector` names, refusing a chat.
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>>;
}

/// The registered repositories, whose paths agents trust.
pub trait Repositories: Send + Sync {
    /// The registered repository `id`.
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>>;
}

/// The executions that run agents.
pub trait Executions: Send + Sync {
    /// The agent the execution `id` registered, while it runs.
    fn agent_name<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Option<Agent>>;

    /// Whether Shoal is stopping the agent the execution `id` runs.
    fn agent_stopping<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, bool>;

    /// The process trees of the agents the tracked executions run.
    fn agent_trees(&self) -> BoxFuture<'_, Result<AgentTrees>>;

    /// The spaces whose connected agent Shoal is not stopping.
    fn running_agent_spaces(&self) -> BoxFuture<'_, Vec<SpaceId>>;
}

/// The alerts that tell of agents.
pub trait Alerts: Send + Sync {
    /// Tell the user of `message`, about no space. A failure only logs.
    fn notify_user(&self, kind: AlertKind, message: String) -> BoxFuture<'_, ()>;

    /// Tell whoever receives the events of the space `id`, which has `name`,
    /// of `message`. A failure only logs.
    fn notify_space<'a>(
        &'a self,
        name: &'a SpaceName,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()>;

    /// Tell the user that a state report of the agent of the space
    /// `selector` names failed with `error`.
    fn report_state_failure<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        error: &'a str,
    ) -> BoxFuture<'a, Result<()>>;
}

/// What else ends with an agent's turn.
pub trait Turns: Send + Sync {
    /// Post the `final_message` of the turn the agent `execution` runs in
    /// the space `selector` names ended where its turn marked it to go.
    fn end_marked_turn<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        execution: Option<&'a ExecutionId>,
        final_message: Option<String>,
    ) -> BoxFuture<'a, ()>;
}

/// The agent queue.
pub trait Queue: Send + Sync {
    /// Look at the queue again, since what admits its entries changed.
    fn wake_queue(&self);
}

/// Everything the agents need of the daemon.
pub trait Daemon: Spaces + Repositories + Executions + Alerts + Turns + Queue {}

impl<T> Daemon for T where T: Spaces + Repositories + Executions + Alerts + Turns + Queue {}

/// The records of other subsystems the agents read and write in their own
/// transactions.
pub trait Records: Send + Sync {
    /// Forget the commands the agent of the space `id` left running, which
    /// no hook reports the end of once it is idle.
    fn end_commands(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()>;

    /// Whether a stop is recorded for the execution `id`.
    fn stop_recorded(&self, db: &Connection, id: &ExecutionId) -> Result<bool>;

    /// Wake the agent of the space `id` if it is asleep: the restores Shoal
    /// queued on its own for it wait only for their turn in the agent queue.
    fn wake_asleep(&self, db: &Connection, id: &SpaceId) -> Result<()>;

    /// Whether the agent of the space `id`, or of a space below it whose
    /// activity it receives while the spaces `running` have their own,
    /// watches an open PR, whose activity starts its next turn.
    fn receives_open_pr_activity(
        &self,
        db: &Connection,
        id: &SpaceId,
        running: &[SpaceId],
    ) -> Result<bool>;
}
