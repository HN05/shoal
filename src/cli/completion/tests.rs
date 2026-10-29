use super::*;
use crate::{
    model::{Chat, OwnedPlace, RunningAgent},
    protocol::{self, Body, ChatMethod, ExecutionMethod, Method, Operation, Request, Response},
    sessions::{HostName, Origin},
    state::WorkspaceState,
};
use std::{fs, os::unix::fs::PermissionsExt};
use tokio::net::UnixListener;

#[tokio::test(start_paused = true)]
async fn live_lookups_keep_the_interactive_budget() {
    let started = tokio::time::Instant::now();
    let error = live(std::future::pending::<Result<()>>())
        .await
        .unwrap_err();
    assert_eq!(started.elapsed(), Duration::from_millis(500));
    assert_eq!(error.to_string(), "live completion lookup timed out");
    assert_eq!(live(async { Ok("candidate") }).await.unwrap(), "candidate");
    assert_eq!(
        live::<()>(async { anyhow::bail!("discovery failed") })
            .await
            .unwrap_err()
            .to_string(),
        "discovery failed"
    );
}

#[tokio::test]
async fn attach_candidates_follow_recorded_places() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    install_session_fixture(root.path())?;
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        crate::test_support::isolated_test(
            root.path(),
            "cli::completion::tests::attach_candidates_child",
        )
        .env("PATH", root.path().join("bin"))
        .output(),
    )
    .await
    .context("attach candidate discovery stalled")??;
    assert!(output.status.success(), "{output:?}");
    Ok(())
}

fn install_session_fixture(root: &std::path::Path) -> Result<()> {
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    // Only `retained` is open: the records of `gone` and `stale` outlive
    // their sessions.
    let script = bin.join("zmx");
    fs::write(
        &script,
        "#!/bin/sh\n[ \"$*\" = list ] || exit 1\nprintf '  name=retained\\tpid=3\\tclients=0\\n'\n",
    )?;
    fs::set_permissions(script, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[tokio::test]
#[ignore = "isolated environment launched by attach candidate regression"]
async fn attach_candidates_child() -> Result<()> {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return Ok(());
    }
    let root = std::env::current_dir()?;
    let paths = Paths::for_test(&root);
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let daemon = serve_attach_targets(listener, &root);
    let typed = Typed {
        state: Some(paths.state),
        ..Typed::default()
    };
    // Exercise the same discovery as Tab, without requiring two subprocesses
    // and daemon replies to finish within its 500 ms interactive deadline.
    let (candidates, served) = tokio::join!(
        typed.candidates(Target::Workspaces(Need::Session), ""),
        daemon
    );
    served?;
    let candidates = candidates?;
    let names: BTreeSet<_> = candidates.iter().map(|value| value.get_value()).collect();
    assert_eq!(
        names,
        BTreeSet::from([
            OsStr::new("first"),
            OsStr::new("second"),
            OsStr::new("fourth"),
            OsStr::new("chat")
        ])
    );
    Ok(())
}

async fn serve_attach_targets(listener: UnixListener, root: &std::path::Path) -> Result<()> {
    for _ in 0..4 {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        let body = if matches!(
            request.method,
            Method::Operation(Operation::Execution(ExecutionMethod::ListAgents))
        ) {
            // A connected agent counts as it is; executions whose wrapper
            // is gone count only while their session is open.
            Body::RunningAgents(
                [
                    ("w1", Some("claude"), "workspace-session"),
                    ("w3", None, "stale"),
                    ("w4", None, "retained"),
                ]
                .map(|(workspace, agent, session)| RunningAgent {
                    execution: format!("{workspace}-execution"),
                    workspace_id: workspace.into(),
                    agent: agent.map(Into::into),
                    restorable: agent.is_some(),
                    state: None,
                    origin: Some(Origin::created(HostName::Zmx, session.into())),
                })
                .into(),
            )
        } else if matches!(
            request.method,
            Method::Operation(Operation::Workspace(WorkspaceMethod::OwnedPlaces))
        ) {
            Body::OwnedPlaces(
                [("w2", "retained"), ("w3", "gone")]
                    .map(|(workspace, session)| OwnedPlace {
                        workspace_id: workspace.into(),
                        origin: Origin::created(HostName::Zmx, session.into()),
                    })
                    .into(),
            )
        } else if matches!(
            request.method,
            Method::Operation(Operation::Chat(ChatMethod::ListChats))
        ) {
            Body::Chats(vec![Chat {
                id: "c1".into(),
                repository_id: "repo".into(),
                name: "chat".into(),
                agent: "codex".into(),
                pid: 1,
                started_at: 0,
                origin: Some(Origin::created(HostName::Zmx, "chat-session".into())),
            }])
        } else {
            assert!(matches!(
                request.method,
                Method::Operation(Operation::Workspace(WorkspaceMethod::ListWorkspaces))
            ));
            Body::Workspaces(attach_workspaces(root))
        };
        protocol::write(&mut stream, &Response::new(request.id, body)).await?;
    }
    Ok(())
}

fn attach_workspaces(root: &std::path::Path) -> Vec<Workspace> {
    [
        ("w1", "first"),
        ("w2", "second"),
        ("w3", "third"),
        ("w4", "fourth"),
    ]
    .into_iter()
    .map(|(id, name)| {
        let mut workspace = Workspace::new_record(
            "repo".into(),
            name.into(),
            root.join(name),
            name.into(),
            WorkspaceState::Ready,
        );
        workspace.id = id.into();
        workspace
    })
    .collect()
}
