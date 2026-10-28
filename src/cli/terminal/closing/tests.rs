use super::*;
use crate::{
    model::{AgentStatus, Completion, Execution, Inspection, Workspace},
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
            origin: None,
        };
        assert_eq!(
            inspect_lifecycle(Some(false), vec![execution]).await,
            Lifecycle::Kept {
                head: "head".into(),
                running: true,
                stopped: false,
                turn: None,
            }
        );
    }
    assert_eq!(
        inspect_lifecycle(Some(false), vec![]).await,
        Lifecycle::Kept {
            head: "head".into(),
            running: false,
            stopped: false,
            turn: None,
        }
    );
    assert_eq!(inspect_lifecycle(None, vec![]).await, Lifecycle::Active);
    // Completion cleanup closes the place only once it removes the workspace.
    assert_eq!(
        inspect_lifecycle(Some(true), vec![]).await,
        Lifecycle::Active
    );
    assert_eq!(
        inspect_with_records(Some(false), vec![], true).await,
        Lifecycle::Kept {
            head: "head".into(),
            running: false,
            stopped: true,
            turn: None,
        }
    );
}

#[test]
fn kept_completion_closes_the_place_once_the_agent_finishes_its_turn() {
    use AgentState::*;
    let completed = |running, stopped, turn| Lifecycle::Kept {
        head: "head".into(),
        running,
        stopped,
        turn,
    };
    let reports = [None, Some(Idle), Some(Working), Some(Waiting)];
    // Without hooks, the host's report decides.
    let running = completed(true, false, None);
    assert!(running.closes(Some(Idle)));
    for reported in [None, Some(Working), Some(Waiting)] {
        assert!(!running.closes(reported));
    }
    // With them, the hooks' report wins over the host's.
    for reported in reports {
        assert!(completed(true, false, Some(Idle)).closes(reported));
        assert!(!completed(true, false, Some(Working)).closes(reported));
        assert!(!completed(true, false, Some(Waiting)).closes(reported));
    }
    for reported in reports {
        assert!(completed(false, false, None).closes(reported));
        assert!(!completed(false, true, None).closes(reported));
        assert!(!completed(true, true, Some(Idle)).closes(reported));
        assert!(Lifecycle::Removed.closes(reported));
        assert!(!Lifecycle::Active.closes(reported));
    }
}

#[tokio::test]
async fn kept_completion_reads_the_hooks_turn_state() {
    let lifecycle = inspect(Some(false), vec![], false, Some(AgentState::Idle)).await;
    assert_eq!(
        lifecycle,
        Lifecycle::Kept {
            head: "head".into(),
            running: false,
            stopped: false,
            turn: Some(AgentState::Idle),
        }
    );
}

/// A host whose place reports `state` and that records each close.
struct FakeHost {
    state: Result<PlaceState, ()>,
    closed: std::sync::Mutex<Vec<String>>,
}

impl FakeHost {
    fn new(state: Result<PlaceState, ()>) -> Self {
        Self {
            state,
            closed: std::sync::Mutex::default(),
        }
    }

    fn closed(&self) -> Vec<String> {
        self.closed.lock().unwrap().clone()
    }
}

impl Host for FakeHost {
    fn name(&self) -> crate::sessions::HostName {
        crate::sessions::HostName::Terminal
    }

    fn serves(&self) -> bool {
        false
    }

    fn background_places(&self) -> &'static str {
        "fake places"
    }

    fn background_note(&self, name: &str) -> String {
        name.to_owned()
    }

    fn close_place<'a>(
        &'a self,
        place: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<()>> {
        self.closed.lock().unwrap().push(place.to_owned());
        Box::pin(async { Ok(()) })
    }

    fn place_state<'a>(
        &'a self,
        _place: &'a Watched,
    ) -> futures_util::future::BoxFuture<'a, Result<PlaceState>> {
        let state = self
            .state
            .map_err(|()| anyhow::anyhow!("the host cannot tell"));
        Box::pin(async move { state })
    }
}

/// Watch `host`'s place, begun under the completion at `kept_at`, with a
/// daemon reporting `lifecycles` in turn, the last one from then on, for at
/// most `looks` looks.
async fn watch_from(
    host: &FakeHost,
    kept_at: Option<&str>,
    lifecycles: &[Option<Lifecycle>],
    looks: usize,
) -> usize {
    let looked = std::sync::atomic::AtomicUsize::new(0);
    let next = || {
        let look = looked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let lifecycle = lifecycles[look.min(lifecycles.len() - 1)].clone();
        async move { Ok(lifecycle) }
    };
    let watching = || looked.load(std::sync::atomic::Ordering::SeqCst) < looks;
    let place = Watched {
        id: "place-1".into(),
        workspace: "workspace".into(),
        instance: None,
    };
    let earlier = EarlierCompletion(kept_at.map(str::to_owned));
    close_when_due(host, &place, earlier, next, watching)
        .await
        .unwrap();
    looked.load(std::sync::atomic::Ordering::SeqCst)
}

/// [`watch_from`] a workspace not done when the watch began.
async fn watch_fake(host: &FakeHost, lifecycles: &[Option<Lifecycle>], looks: usize) -> usize {
    watch_from(host, None, lifecycles, looks).await
}

/// A kept completion at `head` while the agent runs with `turn`.
fn kept(head: &str, turn: Option<AgentState>) -> Option<Lifecycle> {
    Some(Lifecycle::Kept {
        head: head.into(),
        running: true,
        stopped: false,
        turn,
    })
}

#[tokio::test(start_paused = true)]
async fn the_watcher_closes_a_place_once_its_workspace_is_done() {
    let active = Some(Lifecycle::Active);
    let host = FakeHost::new(Ok(PlaceState::Open(None)));
    assert_eq!(watch_fake(&host, &[Some(Lifecycle::Removed)], 5).await, 1);
    assert_eq!(host.closed(), ["place-1"]);
    let host = FakeHost::new(Ok(PlaceState::Open(None)));
    let idle = kept("head", Some(AgentState::Idle));
    assert_eq!(watch_fake(&host, &[active.clone(), idle], 5).await, 2);
    assert_eq!(host.closed(), ["place-1"]);
    let host = FakeHost::new(Ok(PlaceState::Open(Some(AgentState::Idle))));
    watch_fake(&host, &[active, kept("head", None)], 5).await;
    assert_eq!(host.closed(), ["place-1"]);
}

#[tokio::test(start_paused = true)]
async fn the_watcher_keeps_a_place_without_proof() {
    let active = Some(Lifecycle::Active);
    for (state, lifecycles) in [
        (Ok(PlaceState::Open(None)), vec![active.clone()]),
        (
            Ok(PlaceState::Open(Some(AgentState::Idle))),
            vec![active.clone(), kept("head", Some(AgentState::Working))],
        ),
        // A failed daemon or host query proves nothing.
        (Ok(PlaceState::Open(None)), vec![active, None]),
        (Err(()), vec![Some(Lifecycle::Removed)]),
    ] {
        let host = FakeHost::new(state);
        assert_eq!(watch_fake(&host, &lifecycles, 3).await, 3);
        assert!(host.closed().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn a_completion_before_the_watch_closes_nothing_until_it_changes() {
    let idle = || kept("head", Some(AgentState::Idle));
    // An agent started in a workspace already done is new work.
    let host = FakeHost::new(Ok(PlaceState::Open(None)));
    assert_eq!(watch_from(&host, Some("head"), &[idle()], 3).await, 3);
    assert!(host.closed().is_empty());
    // One made after the watch began closes, even before the first look.
    let host = FakeHost::new(Ok(PlaceState::Open(None)));
    assert_eq!(watch_fake(&host, &[idle()], 3).await, 1);
    assert_eq!(host.closed(), ["place-1"]);
    // Removal, a withdrawn completion and one at another HEAD all count.
    for later in [
        vec![Some(Lifecycle::Removed)],
        vec![Some(Lifecycle::Active), idle()],
        vec![kept("next", Some(AgentState::Idle))],
    ] {
        let host = FakeHost::new(Ok(PlaceState::Open(None)));
        let lifecycles: Vec<_> = [idle()].into_iter().chain(later).collect();
        watch_from(&host, Some("head"), &lifecycles, 5).await;
        assert_eq!(host.closed(), ["place-1"]);
    }
}

#[tokio::test(start_paused = true)]
async fn the_watcher_ends_once_the_host_reports_its_place_gone() {
    let host = FakeHost::new(Ok(PlaceState::Gone));
    assert_eq!(watch_fake(&host, &[Some(Lifecycle::Removed)], 5).await, 1);
    assert!(host.closed().is_empty());
}

/// `completion` is the recorded cleanup choice.
async fn inspect_lifecycle(completion: Option<bool>, executions: Vec<Execution>) -> Lifecycle {
    inspect(completion, executions, false, None).await
}

async fn inspect_with_records(
    completion: Option<bool>,
    executions: Vec<Execution>,
    stopped: bool,
) -> Lifecycle {
    inspect(completion, executions, stopped, None).await
}

/// `turn` is the state the agent's hooks reported.
async fn inspect(
    completion: Option<bool>,
    executions: Vec<Execution>,
    stopped: bool,
    turn: Option<AgentState>,
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
        let mut workspace = Workspace::new_record(
            "repo".into(),
            "workspace".into(),
            "/workspace".into(),
            "topic".into(),
            WorkspaceState::Ready,
        );
        workspace.agent_state = turn.map(|state| AgentStatus { state, since: 0 });
        let inspection = Inspection {
            workspace,
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
fn a_closing_place_is_recorded_until_its_watcher_ends() {
    let root = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    let path = Marker::path(&paths, "herdr", "workspace", "tab-1").unwrap();
    let closing = Marker::record(&paths, "herdr", "workspace", "tab-1");
    assert!(path.exists());
    drop(closing);
    assert!(!path.exists());
    assert_eq!(Marker::path(&paths, "herdr", "workspace", "a/b"), None);
    assert_eq!(Marker::path(&paths, "herdr", "workspace", ""), None);
}
