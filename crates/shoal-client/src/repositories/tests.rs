use tokio::net::UnixListener;

use super::*;
use shoal_protocol::paths::Paths;
use shoal_protocol::protocol::{self, Body, Method, Operation, Request, Response};

/// A hang guard, not the behavior under test.
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("hold exchange timed out")
}

#[tokio::test]
async fn a_contribution_hold_resumes_after_the_daemon_closes_the_registration() -> Result<()> {
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let registering = tokio::spawn({
        let paths = paths.clone();
        async move { register_contribution(&paths, "https://forge.example/a/b".into()).await }
    });
    let (mut stream, _) = bounded(listener.accept()).await?;
    let request: Request = protocol::read(&mut stream).await?;
    let repo = Repository {
        id: "r1".into(),
        path: "/work/b/.checkout".into(),
        source: "https://forge.example/a/b".into(),
        last_used: 1,
        name: None,
        workspaces_dir: "/work/b".into(),
        contribution: true,
    };
    let body = Body::Repository(repo);
    protocol::write(&mut stream, &Response::new(request.id, body)).await?;
    let (_repo, _hold) = bounded(registering).await??;
    drop(stream);
    let (mut stream, _) = bounded(listener.accept()).await?;
    let request: Request = protocol::read(&mut stream).await?;
    let Method::Operation(Operation::Repository(RepositoryMethod::HoldContribution { repository })) =
        request.method
    else {
        anyhow::bail!("the hold resumed as {}", request.method.name());
    };
    assert_eq!(repository.as_str(), "r1");
    Ok(())
}
