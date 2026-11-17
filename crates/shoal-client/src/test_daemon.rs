//! Connections to a [`TestDaemon`], as the user and as a workspace's agent,
//! for the tests of what apps send through them, with the requests and
//! readings several of them share.
use shoal_daemon::test_daemon::TestDaemon;
use shoal_protocol::{
    model::{QueueEntry, Workspace},
    protocol::{ErrorCode, OperationMethod, RemoteError, operations::WorkspaceStart},
};

use crate::connection::Connector;

/// Connections to a test daemon, built from its paths and scope tokens.
pub(crate) trait Connect {
    /// Requests as the user.
    fn connector(&self) -> Connector;

    /// Requests as the agent of `workspace`.
    async fn scoped(&self, workspace: &Workspace) -> Connector;
}

impl Connect for TestDaemon {
    fn connector(&self) -> Connector {
        Connector {
            paths: self.manager.paths.clone(),
            scope: None,
        }
    }

    async fn scoped(&self, workspace: &Workspace) -> Connector {
        self.connector().scope(self.scope_token(workspace).await)
    }
}

/// The code `result` failed with.
pub(crate) fn code<T: std::fmt::Debug>(result: anyhow::Result<T>) -> ErrorCode {
    let error = result.unwrap_err();
    match error.downcast_ref::<RemoteError>() {
        Some(remote) => remote.code.clone(),
        None => panic!("expected a refusal: {error:#}"),
    }
}

/// The plan the daemon queued with `entry`.
pub(crate) async fn queued_plan(daemon: &TestDaemon, entry: &QueueEntry) -> serde_json::Value {
    let plan = daemon.manager.queued_plan(&entry.id).await.unwrap().plan;
    serde_json::from_str(&plan).unwrap()
}

/// A request to start workspace `start` of the test repository from HEAD.
pub(crate) fn start_workspace(daemon: &TestDaemon, start: WorkspaceStart) -> OperationMethod {
    OperationMethod::StartWorkspace {
        repository: daemon.repository.id.clone().into(),
        start,
        base: Some("HEAD".into()),
        path: None,
        git_profile: None,
        parent: None,
        options: Default::default(),
        issue: None,
        pr: None,
        agent: None,
        waiting: None,
        blockers: Vec::new(),
        env: None,
        queue_entry: None,
    }
}
