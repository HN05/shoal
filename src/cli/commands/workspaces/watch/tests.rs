use std::time::Duration;

fn deadline(seconds: u64) -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(seconds)
}

#[tokio::test]
async fn item_wait_continues_on_the_replacement_daemon() {
    use crate::{
        cli::context::Context,
        daemon::links::wait::Updates,
        paths::Paths,
        protocol::{self, Body, ErrorCode, ItemMethod, Method, Operation, Request, Response},
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
            Method::Operation(Operation::Item(ItemMethod::WatchItems {
                timeout_secs: Some(600),
                ..
            }))
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
        let Method::Operation(Operation::Item(ItemMethod::WatchItems {
            workspace,
            timeout_secs,
            ..
        })) = request.method
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
            attached: None,
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
        protocol::{self, ItemMethod, Method, Operation, Request},
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
        Method::Operation(Operation::Item(ItemMethod::WatchItems {
            timeout_secs: Some(600),
            ..
        }))
    ));
}
