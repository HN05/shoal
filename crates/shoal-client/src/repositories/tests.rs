use tokio::net::UnixListener;

use super::*;
use crate::test_daemon::Connect as _;
use shoal_daemon::test_daemon::TestDaemon;
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
        shared_url: None,
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

/// A Claude Code config in `directory` trusting each of `paths`.
fn trust_in(directory: &std::path::Path, paths: &[&std::path::Path]) {
    let projects: serde_json::Map<_, _> = paths
        .iter()
        .map(|path| (path.display().to_string(), trusted()))
        .collect();
    std::fs::create_dir_all(directory).unwrap();
    let config = serde_json::json!({ "projects": projects });
    std::fs::write(directory.join(".claude.json"), config.to_string()).unwrap();
}

fn trusted() -> serde_json::Value {
    serde_json::json!({"hasTrustDialogAccepted": true})
}

/// The projects the Claude Code config in `directory` lists.
fn projects_in(directory: &std::path::Path) -> serde_json::Value {
    let config = std::fs::read_to_string(directory.join(".claude.json")).unwrap();
    serde_json::from_str::<serde_json::Value>(&config).unwrap()["projects"].take()
}

/// The test daemon, its home, and a config location its repository's
/// agents inherited, which the daemon keeps as a launch does.
async fn with_inherited_config() -> (TestDaemon, PathBuf, PathBuf) {
    let daemon = TestDaemon::start().await;
    let home = std::fs::canonicalize(&daemon.manager.paths.home).unwrap();
    let inherited = home.join("inherited");
    let kept = BTreeMap::from([(
        shoal_protocol::env::CLAUDE_CONFIG_DIR.to_owned(),
        inherited.display().to_string(),
    )]);
    keep_agent_environment(&daemon.connector(), &daemon.repository.id, kept)
        .await
        .unwrap();
    (daemon, home, inherited)
}

#[tokio::test]
async fn removing_a_repository_cleans_its_trust_where_its_agents_kept_configs() {
    let (daemon, home, inherited) = with_inherited_config().await;
    let workspace = daemon.workspace("topic").await;
    let repo = &daemon.repository;
    let checkout = std::fs::canonicalize(&repo.path).unwrap();
    let directory = std::fs::canonicalize(&repo.workspaces_dir).unwrap();
    let worktree = std::fs::canonicalize(&workspace.path).unwrap();
    let keep = std::path::Path::new("/keep");
    for config in [&home, &inherited] {
        trust_in(config, &[&checkout, &directory, &worktree, keep]);
    }

    remove(&daemon.connector(), repo.id.clone().into(), None)
        .await
        .unwrap();
    for config in [&home, &inherited] {
        assert_eq!(projects_in(config), serde_json::json!({"/keep": trusted()}));
    }
    let kept = daemon
        .manager
        .agents
        .agent_environments(&repo.id)
        .await
        .unwrap();
    assert!(kept.is_empty(), "{kept:?}");
}

#[tokio::test]
async fn moving_a_repository_carries_its_trust_to_its_new_directory() {
    let (daemon, home, inherited) = with_inherited_config().await;
    daemon.workspace("topic").await;
    let repo = &daemon.repository;
    let checkout = std::fs::canonicalize(&repo.path).unwrap();
    let directory = std::fs::canonicalize(&repo.workspaces_dir).unwrap();
    for config in [&home, &inherited] {
        trust_in(config, &[&checkout, &directory]);
    }
    let root = home.join("new");
    let config = format!("[machine]\nroot_dir = {root:?}\n");
    shoal_daemon::test_daemon::configure(&daemon.manager, &config).await;

    let moved = move_all(&daemon.connector(), true).await.unwrap();
    let [moved] = moved.as_slice() else {
        panic!("expected one move: {moved:?}");
    };
    let moved_to = std::fs::canonicalize(&moved.repository.workspaces_dir).unwrap();
    assert!(moved_to.starts_with(&root), "{}", moved_to.display());
    let expected = serde_json::json!({
        checkout.display().to_string(): trusted(),
        moved_to.display().to_string(): trusted(),
    });
    for config in [&home, &inherited] {
        assert_eq!(projects_in(config), expected);
    }
}
