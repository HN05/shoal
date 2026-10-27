use super::*;
use crate::{
    model::{Completion, Execution, Inspection, Workspace},
    paths::Paths,
    protocol::{self, Body, Method, Operation, Request, Response, WorkspaceMethod},
    state::{ExecutionState, WorkspaceState},
};
use tokio::{io::BufReader, net::UnixListener};

#[tokio::test]
async fn kept_completion_waits_for_running_and_unresolved_executions() {
    for state in [ExecutionState::Running, ExecutionState::Unknown] {
        let execution = Execution {
            id: "agent".into(),
            workspace_id: "workspace".into(),
            state,
            wrapper: None,
            child: None,
            group_id: None,
        };
        assert_eq!(
            inspect_lifecycle(Some(false), vec![execution]).await,
            Lifecycle::Kept {
                running: true,
                stopped: false
            }
        );
    }
    assert_eq!(
        inspect_lifecycle(Some(false), vec![]).await,
        Lifecycle::Kept {
            running: false,
            stopped: false
        }
    );
    assert_eq!(inspect_lifecycle(None, vec![]).await, Lifecycle::Active);
    // Completion cleanup closes the tab only once it removes the workspace.
    assert_eq!(
        inspect_lifecycle(Some(true), vec![]).await,
        Lifecycle::Active
    );
    assert_eq!(
        inspect_with_records(Some(false), vec![], true).await,
        Lifecycle::Kept {
            running: false,
            stopped: true
        }
    );
}

#[test]
fn kept_completion_closes_the_tab_once_the_agent_finishes_its_turn() {
    use AgentStatus::*;
    let completed = |running, stopped| Lifecycle::Kept { running, stopped };
    let running = completed(true, false);
    for status in [Idle, Done] {
        assert!(running.closes_tab(status));
    }
    for status in [Working, Blocked, Unknown] {
        assert!(!running.closes_tab(status));
    }
    for status in [Idle, Working, Blocked, Done, Unknown] {
        assert!(completed(false, false).closes_tab(status));
        assert!(!completed(false, true).closes_tab(status));
        assert!(!completed(true, true).closes_tab(status));
        assert!(Lifecycle::Removed.closes_tab(status));
        assert!(!Lifecycle::Active.closes_tab(status));
    }
}

/// `completion` is the recorded cleanup choice.
async fn inspect_lifecycle(completion: Option<bool>, executions: Vec<Execution>) -> Lifecycle {
    inspect_with_records(completion, executions, false).await
}

async fn inspect_with_records(
    completion: Option<bool>,
    executions: Vec<Execution>,
    stopped: bool,
) -> Lifecycle {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(temp.path());
    paths.prepare().unwrap();
    if stopped {
        crate::execution::recovery::save_record(&paths, "workspace", "agent", "codex", None, None)
            .unwrap();
    }
    let listener = UnixListener::bind(&paths.socket).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let request: Request = protocol::read_buffered(&mut stream).await.unwrap();
        assert!(
            matches!(request.method, Method::Operation(Operation::Workspace(WorkspaceMethod::InspectWorkspace { workspace })) if workspace == "workspace")
        );
        let inspection = Inspection {
            workspace: Workspace::new_record(
                "repo".into(),
                "workspace".into(),
                "/workspace".into(),
                "topic".into(),
                WorkspaceState::Ready,
            ),
            completion: completion.map(|cleanup| Completion {
                head: "head".into(),
                cleanup,
                error: None,
            }),
            executions,
            issue: None,
            pr_cleanup: None,
            ports: vec![],
            resources: vec![],
            simulators: vec![],
        };
        protocol::write(
            stream.get_mut(),
            &Response::new(request.id, Body::Inspection(inspection)),
        )
        .await
        .unwrap();
    });
    let lifecycle = workspace_lifecycle(&Context::new(paths, true), "workspace")
        .await
        .unwrap();
    server.await.unwrap();
    lifecycle
}

#[test]
fn a_closing_tab_is_recorded_until_its_watcher_ends() {
    let root = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    let path = ClosingTab::path(&paths, "workspace", "tab-1").unwrap();
    let closing = ClosingTab::record(&paths, "workspace", "tab-1");
    assert!(path.exists());
    drop(closing);
    assert!(!path.exists());
    assert_eq!(ClosingTab::path(&paths, "workspace", "a/b"), None);
    assert_eq!(ClosingTab::path(&paths, "workspace", ""), None);
}
