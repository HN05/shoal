use super::*;
use crate::{paths::Paths, protocol::Request};
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
            Method::WatchWorkspaceEvents {
                since: Some(9),
                follow: true
            }
        ));
        protocol::write(&mut stream, &Response::new(request.id, Body::Ok))
            .await
            .unwrap();
        protocol::write(
            &mut stream,
            &Response::new(
                request.id,
                Body::EventItem(EventItem::Gap {
                    since: 9,
                    oldest_id: 4,
                    latest_id: 5,
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
            Method::WatchWorkspaceEvents {
                since: Some(3),
                follow: true
            }
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
    let ctx = Context::new(paths, true);
    let error = tokio::time::timeout(std::time::Duration::from_secs(60), run(&ctx, true, Some(9)))
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("received Busy"));
    server.await.unwrap();
}
