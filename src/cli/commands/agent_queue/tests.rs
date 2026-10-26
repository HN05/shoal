use tokio::net::UnixListener;

use super::*;
use crate::protocol::{self, Body, Request, Response};

/// A hang guard, not the behavior under test.
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("hold exchange timed out")
}

#[tokio::test]
async fn starting_waits_for_a_hold_the_daemon_closed_unanswered() -> Result<()> {
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let starting = tokio::spawn({
        let paths = paths.clone();
        async move { Hold::start(&paths, "entry").await }
    });
    let (mut stream, _) = bounded(listener.accept()).await?;
    let _: Request = protocol::read(&mut stream).await?;
    drop(stream);
    // The hold asks again only after it has seen the closed connection.
    let (mut stream, _) = bounded(listener.accept()).await?;
    assert!(!starting.is_finished());
    let request: Request = protocol::read(&mut stream).await?;
    protocol::write(&mut stream, &Response::new(request.id, Body::Ok)).await?;
    let _hold = bounded(starting).await?;
    Ok(())
}
