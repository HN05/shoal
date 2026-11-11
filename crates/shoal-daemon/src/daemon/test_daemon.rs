//! A daemon serving the protocol on the socket of a temporary state
//! directory, for tests that connect to it the way the CLI and apps do.
use crate::client::scope_tokens::ScopeToken;
use std::{sync::Arc, time::Duration};

use tokio::{
    net::{UnixListener, UnixStream},
    task::{JoinHandle, JoinSet},
};

use super::{Server, connections::Capacity, workspace::Manager};
use crate::client::space_names::SpaceSelector;
use crate::{
    client::connection::Connector,
    daemon::test_support::{git, manager, repository},
    model::{Repository, Workspace},
};

/// A generous guard against a hang; no test waits this long when it passes.
pub(crate) const HANG: Duration = Duration::from_secs(60);

/// A daemon with a registered repository whose origin no test reaches.
pub(crate) struct TestDaemon {
    _root: tempfile::TempDir,
    pub manager: Arc<Manager>,
    pub repository: Repository,
    accepting: JoinHandle<()>,
}

impl TestDaemon {
    pub async fn start() -> Self {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        git(&path, &["config", "protocol.allow", "never"]);
        let repository = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        git(
            &path,
            &["remote", "add", "origin", "https://github.com/team/repo"],
        );
        // The cached remote HEAD names the default branch without a fetch.
        git(
            &path,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let accepting = accept(&manager);
        Self {
            _root: root,
            manager,
            repository,
            accepting,
        }
    }

    /// Close every connection and listen again, as a restarted daemon does.
    pub async fn restart(&mut self) {
        self.accepting.abort();
        let _ = (&mut self.accepting).await;
        std::fs::remove_file(&self.manager.paths.socket).unwrap();
        self.accepting = accept(&self.manager);
    }

    /// Requests as the user.
    pub fn connector(&self) -> Connector {
        Connector {
            paths: self.manager.paths.clone(),
            scope: None,
        }
    }

    /// Requests as the agent of `workspace`.
    pub async fn scoped(&self, workspace: &Workspace) -> Connector {
        self.connector().scope(self.scope_token(workspace).await)
    }

    pub async fn scope_token(&self, workspace: &Workspace) -> ScopeToken {
        let environment = self
            .manager
            .workspace_environment(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap();
        environment[crate::env::SCOPE_TOKEN].clone().into()
    }

    pub async fn workspace(&self, name: &str) -> Workspace {
        self.manager
            .create_workspace(
                &self.repository.id,
                name.into(),
                Some("HEAD".into()),
                None,
                None,
            )
            .await
            .unwrap()
    }

    /// A raw connection to the socket.
    pub async fn raw(&self) -> UnixStream {
        UnixStream::connect(&self.manager.paths.socket)
            .await
            .unwrap()
    }
}

/// Serve the socket of `manager` as the daemon does until the task is
/// aborted, which ends its connections with it.
fn accept(manager: &Arc<Manager>) -> JoinHandle<()> {
    let listener = UnixListener::bind(&manager.paths.socket).unwrap();
    let server = Server {
        capacity: Arc::new(Capacity::new(16, 16)),
        ..super::tests::server(manager.clone())
    };
    tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let server = server.clone();
            connections.spawn(async move {
                let operation = server.manager.gates.owned_operation().await;
                let _ = super::serve(stream, server, operation).await;
            });
        }
    })
}

#[cfg(test)]
mod tests;
