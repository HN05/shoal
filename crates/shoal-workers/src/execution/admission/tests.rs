use std::future::Future;

use anyhow::Result;
use shoal_protocol::{
    paths::Paths,
    protocol::{self, Body, Request, Response},
};
use tokio::{io::BufReader, net::UnixListener};

use super::*;

/// A hang guard, not the behavior under test.
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("admission exchange timed out")
}

/// Accept the wrapper's next admission on `listener`, admit it and read its
/// run's end, with the connection to answer on.
async fn admit_and_read_end(
    listener: &UnixListener,
) -> Result<(RunReport, tokio::net::UnixStream)> {
    let (stream, _) = listener.accept().await?;
    let mut stream = BufReader::new(stream);
    let request: Request = protocol::read_buffered(&mut stream).await?;
    protocol::write(stream.get_mut(), &Response::new(request.id, Body::Ok)).await?;
    protocol::write(stream.get_mut(), &AdmissionFrame::Admitted).await?;
    let ran = protocol::read_buffered(&mut stream).await?;
    Ok((ran, stream.into_inner()))
}

#[tokio::test]
async fn a_run_s_end_the_daemon_did_not_acknowledge_is_sent_again() -> Result<()> {
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let run = CommandRun {
        key: CommandKey::Named("test".into()),
        now: false,
    };
    let space = SpaceSelector::id(&"w".into());
    let wrapper = tokio::spawn(async move {
        let paths = CommandPaths::from(paths);
        let admission = Admission::wait(&paths, &space, &run, AdmitAs::New).await?;
        let ran = RunReport::Ran {
            report: "run".into(),
            peak: 1,
            duration_ms: 1,
            whole: true,
            oom_killed: false,
        };
        admission.end(ran).await;
        anyhow::Ok(())
    });
    // A daemon that stops before it acknowledges the end.
    let (lost, stream) = bounded(admit_and_read_end(&listener)).await?;
    drop(stream);
    let (again, mut stream) = bounded(admit_and_read_end(&listener)).await?;
    assert_eq!(again, lost);
    protocol::write(&mut stream, &AdmissionFrame::Recorded).await?;
    bounded(wrapper).await??;
    Ok(())
}
