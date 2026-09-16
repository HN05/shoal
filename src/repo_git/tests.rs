use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{paths::Paths, protocol::Method, scope, workspace::Manager};

struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    manager: Arc<Manager>,
    repo_id: String,
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn commit(repo: &Path, file: &str) {
    git(repo, &["add", file]);
    git(
        repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            file,
        ],
    );
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let repo = root.path().join("repo with ' quotes & $literal");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("tracked"), "initial\n").unwrap();
        commit(&repo, "tracked");
        let manager = Manager::open(Paths {
            home: root.path().into(),
            state: root.path().join("state"),
            socket: root.path().join("unused.sock"),
        })
        .await
        .unwrap();
        let repo_id = manager
            .register(repo.to_str().unwrap().into(), None)
            .await
            .unwrap()
            .id;
        Self {
            root,
            repo,
            manager,
            repo_id,
        }
    }

    async fn add(&self, name: &str) -> crate::model::Workspace {
        self.manager
            .add(self.repo_id.clone(), name.into(), None)
            .await
            .unwrap()
    }

    fn remote(&self) -> PathBuf {
        let origin = self.root.path().join("origin.git");
        git(
            &self.repo,
            &[
                "clone",
                "--bare",
                self.repo.to_str().unwrap(),
                origin.to_str().unwrap(),
            ],
        );
        git(
            &self.repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&self.repo, &["fetch", "origin"]);
        git(
            &self.repo,
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        let author = self.root.path().join("author");
        git(
            &self.repo,
            &["clone", origin.to_str().unwrap(), author.to_str().unwrap()],
        );
        fs::write(author.join("upstream"), "remote\n").unwrap();
        commit(&author, "upstream");
        git(&author, &["push", "origin", "main"]);
        author
    }
}

#[tokio::test]
async fn names_suffix_only_conflicts_and_serialize_concurrent_adds() {
    let f = Fixture::new().await;
    assert_eq!(f.add("plain").await.branch, "plain");
    assert_eq!(f.add("HEAD").await.branch, "HEAD-2");
    git(&f.repo, &["branch", "feature"]);
    git(&f.repo, &["branch", "feature-2"]);
    git(
        &f.repo,
        &["update-ref", "refs/remotes/origin/feature-3", "HEAD"],
    );
    git(&f.repo, &["branch", "feature-4/nested"]);
    let workspace = f.add("feature").await;
    assert_eq!(workspace.branch, "feature-5");
    assert_eq!(workspace.path.file_name().unwrap(), "feature");
    assert_eq!(f.add("main").await.branch, "main-2");
    f.manager
        .remove(workspace.id, crate::removal::Choice::KeepBranch, 0)
        .await
        .unwrap();
    assert_eq!(f.add("feature").await.branch, "feature-6");
    git(&f.repo, &["branch", "concurrent"]);
    let (first, second) = tokio::join!(f.add("concurrent"), f.add("concurrent-2"));
    assert_ne!(first.branch, second.branch);
}

#[tokio::test]
async fn pull_updates_main_preserves_feature_and_enforces_scope() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    f.add("other").await;
    let before = git(&f.repo, &["rev-parse", "main"]);
    let author = f.remote();
    git(&f.repo, &["remote", "rename", "origin", "source"]);
    let expected = git(&author, &["rev-parse", "HEAD"]);
    f.manager
        .scopes
        .lock()
        .await
        .insert("token".into(), ("execution".into(), workspace.id.clone()));
    let mut denied = Method::PullMain {
        workspace: "other".into(),
    };
    assert!(
        scope::authorize(&f.manager, Some("token"), &mut denied)
            .await
            .is_err()
    );
    let mut allowed = Method::PullMain {
        workspace: "worker".into(),
    };
    scope::authorize(&f.manager, Some("token"), &mut allowed)
        .await
        .unwrap();
    let Method::PullMain { workspace: target } = allowed else {
        panic!("wrong method")
    };
    assert_eq!(target, workspace.id);
    let result = f.manager.pull_main(target).await.unwrap();
    assert!(result.updated);
    assert_eq!(result.previous_commit, before.trim());
    assert_eq!(result.commit, expected.trim());
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
    assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), before);
    assert!(!f.manager.pull_main("worker".into()).await.unwrap().updated);
    assert_eq!(git(&f.repo, &["for-each-ref", "refs/shoal/pull/"]), "");
    assert_eq!(
        fs::read_to_string(f.repo.join("upstream")).unwrap(),
        "remote\n"
    );
}

#[tokio::test]
async fn pull_refuses_missing_upstream_dirty_and_diverged_main() {
    let f = Fixture::new().await;
    f.add("worker").await;
    assert!(
        f.manager
            .pull_main("worker".into())
            .await
            .unwrap_err()
            .to_string()
            .contains("no branch upstream")
    );
    let before = git(&f.repo, &["rev-parse", "main"]);
    f.remote();
    fs::write(f.repo.join("tracked"), "local edits\n").unwrap();
    assert!(
        f.manager
            .pull_main("worker".into())
            .await
            .unwrap_err()
            .to_string()
            .contains("uncommitted or untracked")
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
    assert_eq!(
        fs::read_to_string(f.repo.join("tracked")).unwrap(),
        "local edits\n"
    );
    commit(&f.repo, "tracked");
    let diverged = git(&f.repo, &["rev-parse", "main"]);
    assert!(
        f.manager
            .pull_main("worker".into())
            .await
            .unwrap_err()
            .to_string()
            .contains("diverged")
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), diverged);
    assert_eq!(git(&f.repo, &["for-each-ref", "refs/shoal/pull/"]), "");
}

#[tokio::test]
async fn pull_handles_unchecked_ahead_and_separate_main_checkouts() {
    let f = Fixture::new().await;
    f.add("worker").await;
    let before = git(&f.repo, &["rev-parse", "HEAD"]);
    let author = f.remote();
    let expected = git(&author, &["rev-parse", "HEAD"]);
    git(&f.repo, &["switch", "-c", "source-branch"]);
    assert!(f.manager.pull_main("worker".into()).await.unwrap().updated);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
    assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), before);
    git(&author, &["reset", "--hard", "HEAD~1"]);
    git(&author, &["push", "--force", "origin", "main"]);
    assert!(!f.manager.pull_main("worker".into()).await.unwrap().updated);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
    git(&f.repo, &["update-ref", "refs/heads/main", before.trim()]);
    git(&author, &["reset", "--hard", expected.trim()]);
    git(&author, &["push", "origin", "main"]);
    let main_path = f.root.path().join("main checkout\nwith newline");
    git(
        &f.repo,
        &["worktree", "add", main_path.to_str().unwrap(), "main"],
    );
    assert!(f.manager.pull_main("worker".into()).await.unwrap().updated);
    assert_eq!(
        fs::read_to_string(main_path.join("upstream")).unwrap(),
        "remote\n"
    );
}

#[tokio::test]
async fn pull_refuses_main_in_managed_workspace() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    f.add("caller").await;
    f.remote();
    let before = git(&f.repo, &["rev-parse", "main"]);
    git(&f.repo, &["switch", "-c", "source-branch"]);
    git(&workspace.path, &["switch", "main"]);
    assert!(
        f.manager
            .pull_main("caller".into())
            .await
            .unwrap_err()
            .to_string()
            .contains("managed workspace")
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
}
