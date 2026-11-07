use super::*;
use crate::{
    paths::Paths,
    protocol::{Method, Operation, Request},
};
use tokio::net::UnixListener;

#[tokio::test]
async fn follower_reconnects_at_the_last_displayed_cursor() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let paths = Paths::for_test(root.path());
    paths.prepare().unwrap();
    let listener = UnixListener::bind(&paths.socket).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: Request = protocol::read(&mut stream).await.unwrap();
        assert!(matches!(
            request.method,
            Method::Operation(Operation::Workspace(
                WorkspaceMethod::WatchWorkspaceEvents {
                    after: Some(after),
                    follow: true
                }
            )) if after == EventCursor::new(9)
        ));
        protocol::write(&mut stream, &Response::new(request.id, Body::Ok))
            .await
            .unwrap();
        protocol::write(
            &mut stream,
            &Response::new(
                request.id,
                Body::EventItem(EventItem::Gap {
                    after: EventCursor::new(9),
                    oldest_id: EventCursor::new(4),
                    latest_id: EventCursor::new(5),
                }),
            ),
        )
        .await
        .unwrap();
        drop(stream);
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: Request = protocol::read(&mut stream).await.unwrap();
        assert!(matches!(
            request.method,
            Method::Operation(Operation::Workspace(
                WorkspaceMethod::WatchWorkspaceEvents {
                    after: Some(after),
                    follow: true
                }
            )) if after == EventCursor::new(3)
        ));
        // Finish the infinite follower with an explicit terminal response.
        protocol::write(&mut stream, &Response::new(request.id, Body::Ok))
            .await
            .unwrap();
        protocol::write(
            &mut stream,
            &Response::new(
                request.id,
                Body::Busy {
                    message: "fixture end".into(),
                },
            ),
        )
        .await
        .unwrap();
    });
    let follow = async {
        let mut events = Events::open(&paths, Some(EventCursor::new(9)), true).await?;
        while events.next(&paths).await?.is_some() {}
        anyhow::Ok(())
    };
    let error = tokio::time::timeout(std::time::Duration::from_secs(60), follow)
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("received Busy"));
    server.await.unwrap();
}
