use super::*;
use crate::{
    model::Chat,
    protocol::{self, Body, ChatMethod, ExecutionMethod, Method, Operation, Request, Response},
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
async fn attach_candidates_follow_workspace_and_chat_session_labels() -> Result<()> {
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
    let script = bin.join("zmx");
    fs::write(
        &script,
        "#!/bin/sh\n[ \"$*\" = list ] || exit 1\nprintf '  name=workspace-session\\tpid=1\\tclients=0\\tshoal.workspace=w1\\n  name=chat-session\\tpid=2\\tclients=0\\tshoal.chat=c1\\n'\n",
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
        BTreeSet::from([OsStr::new("first"), OsStr::new("chat")])
    );
    Ok(())
}

async fn serve_attach_targets(listener: UnixListener, root: &std::path::Path) -> Result<()> {
    for _ in 0..3 {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        let body = if matches!(
            request.method,
            Method::Operation(Operation::Execution(ExecutionMethod::ListAgents))
        ) {
            // No agent runs in a recorded place.
            Body::RunningAgents(Vec::new())
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
                origin: None,
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
    [("w1", "first"), ("w2", "second")]
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
