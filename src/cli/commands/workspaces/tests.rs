use super::editor;
use std::time::Duration;

#[test]
fn a_launch_in_a_workspace_opens_only_that_workspace() {
    use crate::{model::Workspace, state::WorkspaceState};
    let workspace = |repository: &str, branch: &str| {
        Workspace::new_record(
            repository.into(),
            branch.into(),
            format!("/work/{branch}").into(),
            branch.into(),
            WorkspaceState::Ready,
        )
    };
    let child = workspace("repo", "child");
    let plan = super::AddPlan::agent_in(&child, "claude".parse().unwrap(), None, Vec::new());
    assert!(plan.opens(&child));
    // The same branch in another repository, or another branch, is not the target.
    assert!(!plan.opens(&workspace("other", "child")));
    assert!(!plan.opens(&workspace("repo", "sibling")));
    let mut creating =
        super::AddPlan::agent_in(&child, "claude".parse().unwrap(), None, Vec::new());
    creating.creation.branch = Some("new".into());
    assert!(!creating.opens(&child));
}

#[test]
fn plans_queued_with_codex_cli_stay_tracked() {
    let decode = |json: &str| serde_json::from_str::<super::ResolvedAddAgent>(json).unwrap();
    assert!(decode(r#"{"agent":"codex","codex_mode":"cli"}"#).tracked);
    assert!(!decode(r#"{"agent":"codex","codex_mode":"app"}"#).tracked);
    assert!(!decode(r#"{"agent":"claude","codex_mode":null}"#).tracked);
    assert!(decode(r#"{"agent":"codex","tracked":true}"#).tracked);
}
use std::ffi::OsString;

#[test]
fn editor_prefers_visual_splits_on_whitespace_and_requires_a_value() {
    assert_eq!(
        editor([Some(" code  --wait ".into()), Some("vi".into())]).unwrap(),
        [OsString::from("code"), OsString::from("--wait")]
    );
    for visual in [None, Some(" ".into())] {
        assert_eq!(editor([visual, Some("vi".into())]).unwrap(), ["vi"]);
    }
    for missing in [None, Some(OsString::new()), Some(" ".into())] {
        assert!(editor([missing.clone(), missing]).is_err());
    }
}

fn deadline(seconds: u64) -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(seconds)
}

#[tokio::test]
async fn item_wait_continues_on_the_replacement_daemon() {
    use crate::{
        cli::context::Context,
        daemon::links::wait::Updates,
        paths::Paths,
        protocol::{self, Body, ErrorCode, Method, Request, Response},
    };
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let paths = Paths::for_test(root.path());
    paths.prepare().unwrap();
    let socket = paths.socket.clone();
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        // The restarting daemon drops the wait without a reply.
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: Request = protocol::read(&mut stream).await.unwrap();
        assert!(matches!(
            request.method,
            Method::WatchItems {
                timeout_secs: Some(600),
                ..
            }
        ));
        drop(stream);
        // Its socket goes with it, and the next daemon listens a moment later.
        drop(listener);
        std::fs::remove_file(&socket).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        // It knows the agent's scope once the agent's wrapper reattaches.
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: Request = protocol::read(&mut stream).await.unwrap();
        let refused = Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
        protocol::write(&mut stream, &Response::new(request.id, refused))
            .await
            .unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: Request = protocol::read(&mut stream).await.unwrap();
        let Method::WatchItems {
            workspace,
            timeout_secs,
            ..
        } = request.method
        else {
            panic!("expected an item wait");
        };
        assert_eq!(workspace, "work");
        assert!((1..=600).contains(&timeout_secs.unwrap()));
        let updates = Updates {
            updates: Vec::new(),
            messages: Vec::new(),
            timed_out: true,
            superseded: false,
        };
        protocol::write(
            &mut stream,
            &Response::new(request.id, Body::PrUpdates(updates)),
        )
        .await
        .unwrap();
    });
    let ctx = Context::new(paths, true);
    let body = tokio::time::timeout(
        Duration::from_secs(60),
        super::wait_for_items(&ctx, "work", Default::default(), Some(deadline(600)), false),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(Updates::try_from(body).unwrap().timed_out);
    server.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn item_wait_ends_at_its_deadline_while_the_daemon_is_away() {
    use crate::{
        cli::context::Context,
        daemon::links::wait::Updates,
        paths::Paths,
        protocol::{self, Method, Request},
    };
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let paths = Paths::for_test(root.path());
    paths.prepare().unwrap();
    let listener = tokio::net::UnixListener::bind(&paths.socket).unwrap();
    // The daemon drops the wait and then accepts nothing more.
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        drop(stream);
        anyhow::Ok((request, listener))
    });
    let ctx = Context::new(paths, true);
    let started = tokio::time::Instant::now();
    // The paused clock reaches the deadline only once nothing else can run.
    let body = super::wait_for_items(&ctx, "work", Default::default(), Some(deadline(600)), false)
        .await
        .unwrap();
    assert!(Updates::try_from(body).unwrap().timed_out);
    assert!(started.elapsed() >= Duration::from_secs(600));
    let (request, _listener) = server.await.unwrap().unwrap();
    assert!(matches!(
        request.method,
        Method::WatchItems {
            timeout_secs: Some(600),
            ..
        }
    ));
}
