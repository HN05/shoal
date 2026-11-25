use shoal_core::test_support::{git, repository};
use shoal_protocol::{alerts::AlertKind, model::Repository};

use crate::stand_in::Fixture;

/// A fixture whose contributions go as soon as nothing keeps them.
async fn fixture() -> Fixture {
    let fixture = Fixture::new().await;
    fixture.configure("[cleanup]\ngrace_minutes = 0\n");
    fixture
}

/// A clone registered in place and then marked as a contribution. Its
/// path is canonical, as a clone's under `root_dir` is.
async fn contribution(fixture: &Fixture) -> Repository {
    let root = fixture.root.path();
    let upstream = repository(root, "upstream");
    let clone = root.join("repo");
    let (upstream, clone) = (upstream.to_str().unwrap(), clone.to_str().unwrap());
    git(root, &["clone", "-q", upstream, clone]);
    let repo = fixture
        .register(std::fs::canonicalize(clone).unwrap().display())
        .await;
    let id = repo.id.clone();
    let marked = fixture.repositories.store.run(move |db| {
        db.execute("UPDATE repositories SET contribution=1 WHERE id=?1", [id])?;
        Ok(())
    });
    marked.await.unwrap();
    fixture.repositories.repository(&repo.id).await.unwrap()
}

async fn registered(fixture: &Fixture, repo: &Repository) -> bool {
    let repos = fixture.repositories.list().await.unwrap();
    repos.iter().any(|other| other.id == repo.id)
}

async fn sweep(fixture: &Fixture) {
    let repositories = &fixture.repositories;
    repositories
        .sweep_contributions(&fixture.daemon)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_contribution_waits_out_the_grace_period() {
    let fixture = fixture().await;
    let repo = contribution(&fixture).await;
    let local = "[cleanup]\ngrace_minutes = 10\n";
    fixture.write_local_config(&repo.id, local).await;
    sweep(&fixture).await;
    assert!(registered(&fixture, &repo).await);
    let id = repo.id.clone();
    let over = fixture.repositories.store.run(move |db| {
        db.execute(
            "UPDATE contribution_grace SET cleanup_at=0 WHERE repository_id=?1",
            [id],
        )?;
        Ok(())
    });
    over.await.unwrap();
    sweep(&fixture).await;
    assert!(!registered(&fixture, &repo).await);
}

#[tokio::test]
async fn a_removed_contribution_names_the_fork_it_leaves() {
    let fixture = fixture().await;
    let repo = contribution(&fixture).await;
    let fork = repository(fixture.root.path(), "fork");
    let fork = fork.to_str().unwrap();
    git(&repo.path, &["remote", "add", "fork", fork]);
    sweep(&fixture).await;
    assert!(!registered(&fixture, &repo).await);
    let notifications = fixture.repositories.notifications.list(false, 10).await;
    let notifications = notifications.unwrap();
    let removed = (notifications.iter())
        .find(|notification| notification.kind == AlertKind::RepositoryRemoved)
        .unwrap();
    assert!(removed.message.contains(fork), "{}", removed.message);
}

#[tokio::test]
async fn a_restarted_daemon_keeps_contributions_while_their_holds_resume() {
    let fixture = fixture().await;
    let repo = contribution(&fixture).await;
    let started = tokio::time::Instant::now();
    let holds = fixture.repositories.contribution_holds();
    holds.await_resumes(started + std::time::Duration::from_secs(3600));
    sweep(&fixture).await;
    assert!(registered(&fixture, &repo).await);
    holds.await_resumes(started);
    sweep(&fixture).await;
    assert!(!registered(&fixture, &repo).await);
}
