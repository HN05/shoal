use tokio::net::UnixListener;

use super::*;
use anyhow::Result;
use shoal_protocol::paths::Paths;
use shoal_protocol::protocol::{self, Body, Request, Response};

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
        async move { Hold::start(&paths, &"entry".into()).await }
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

#[tokio::test]
async fn an_admission_keeps_a_frame_that_came_with_its_reply() -> Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let daemon = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        let mut sent = Vec::new();
        protocol::write(&mut sent, &Response::new(request.id, Body::Ok)).await?;
        protocol::write(&mut sent, &AdmissionFrame::Admitted).await?;
        stream.write_all(&sent).await?;
        Ok::<_, anyhow::Error>(stream)
    });
    let command = CommandKey::Named("test".into());
    let space = SpaceSelector::id(&"w".into());
    let mut admission = bounded(CommandAdmission::request(
        &paths,
        space,
        command,
        false,
        AdmitAs::New,
    ))
    .await?;
    let _stream = bounded(daemon).await??;
    assert_eq!(bounded(admission.frame()).await?, AdmissionFrame::Admitted);
    Ok(())
}
