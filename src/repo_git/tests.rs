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
            .register_repository(repo.to_str().unwrap().into(), None, None)
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
            .create_workspace(&self.repo_id, name.into(), None)
            .await
            .unwrap()
    }

    fn remote(&self) -> PathBuf {
        let branch = git(&self.repo, &["branch", "--show-current"]);
        let branch = branch.trim_end_matches('\n');
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
            &[
                "branch",
                &format!("--set-upstream-to=origin/{branch}"),
                branch,
            ],
        );
        let author = self.root.path().join("author");
        git(
            &self.repo,
            &["clone", origin.to_str().unwrap(), author.to_str().unwrap()],
        );
        fs::write(author.join("upstream"), "remote\n").unwrap();
        commit(&author, "upstream");
        git(&author, &["push", "origin", branch]);
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
        .remove_workspace(&workspace.id, crate::removal::BranchChoice::KeepBranch, 0)
        .await
        .unwrap();
    assert_eq!(f.add("feature").await.branch, "feature-6");
    git(&f.repo, &["branch", "concurrent"]);
    let (first, second) = tokio::join!(f.add("concurrent"), f.add("concurrent-2"));
    assert_ne!(first.branch, second.branch);
}

#[tokio::test]
async fn remote_default_controls_creation_pull_diff_and_removal() {
    for branch in ["develop", "master", "release/current"] {
        let f = Fixture::new().await;
        git(&f.repo, &["branch", "-m", branch]);
        let before = git(&f.repo, &["rev-parse", "HEAD"]);
        let author = f.remote();
        // A branch called main and an unrelated checkout must not override HEAD.
        git(&f.repo, &["branch", "main"]);
        git(&f.repo, &["switch", "-c", "unrelated"]);
        let expected = git(&author, &["rev-parse", "HEAD"]);
        let workspace = f.add("henrik/topic").await;
        assert_eq!(workspace.base_ref, Some(format!("refs/heads/{branch}")));
        assert_eq!(workspace.base_commit.as_deref(), Some(expected.trim()));
        assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), expected);
        assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
        assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), before);
        assert_eq!(
            git(&f.repo, &["symbolic-ref", "refs/remotes/origin/HEAD"]),
            format!("refs/remotes/origin/{branch}\n")
        );
        assert!(
            f.manager
                .check_removal(&workspace.id, 0)
                .await
                .unwrap()
                .can_delete_branch()
        );
        assert_eq!(
            f.manager.diff_base(&workspace.id).await.unwrap().commit,
            expected.trim()
        );
        fs::write(author.join("upstream"), "newer\n").unwrap();
        commit(&author, "upstream");
        git(&author, &["push", "origin", branch]);
        let result = f.manager.pull_default_branch(&workspace.id).await.unwrap();
        assert_eq!(result.branch, branch);
        assert!(result.updated);
        assert_eq!(result.commit, git(&author, &["rev-parse", "HEAD"]).trim());
        assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), expected);
        assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
        assert_eq!(
            f.manager.diff_base(&workspace.id).await.unwrap().commit,
            expected.trim()
        );
    }
}

#[tokio::test]
async fn non_main_default_refresh_preserves_safety_and_explicit_overrides() {
    let f = Fixture::new().await;
    git(&f.repo, &["branch", "-m", "develop"]);
    git(&f.repo, &["branch", "main"]);
    f.remote();
    git(&f.repo, &["remote", "rename", "origin", "upstream"]);
    let before = git(&f.repo, &["rev-parse", "HEAD"]);
    fs::write(f.repo.join("tracked"), "dirty\n").unwrap();
    for (index, base) in [None, Some("develop"), Some("refs/heads/develop")]
        .into_iter()
        .enumerate()
    {
        let error = f
            .manager
            .create_workspace(
                &f.repo_id,
                format!("blocked-{index}"),
                base.map(str::to_owned),
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("develop checkout has uncommitted"),
            "{error:#}"
        );
    }
    let explicit = f
        .manager
        .create_workspace(&f.repo_id, "explicit".into(), Some("main".into()))
        .await
        .unwrap();
    assert_eq!(git(&explicit.path, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&f.repo, &["rev-parse", "develop"]), before);
    assert_eq!(
        fs::read_to_string(f.repo.join("tracked")).unwrap(),
        "dirty\n"
    );
}

#[tokio::test]
async fn local_defaults_and_unavailable_or_ambiguous_remote_defaults() {
    let f = Fixture::new().await;
    git(&f.repo, &["branch", "-m", "trunk"]);
    assert_eq!(
        f.add("local").await.base_ref.as_deref(),
        Some("refs/heads/trunk")
    );
    git(&f.repo, &["checkout", "--detach"]);
    assert!(
        f.manager
            .create_workspace(&f.repo_id, "detached".into(), None)
            .await
            .is_err()
    );
    f.manager
        .create_workspace(&f.repo_id, "explicit-detached".into(), Some("trunk".into()))
        .await
        .unwrap();
    git(&f.repo, &["switch", "trunk"]);
    git(
        &f.repo,
        &[
            "remote",
            "add",
            "upstream",
            f.root.path().join("missing.git").to_str().unwrap(),
        ],
    );
    assert!(
        f.manager
            .create_workspace(&f.repo_id, "offline".into(), None)
            .await
            .is_err()
    );
    f.manager
        .create_workspace(&f.repo_id, "explicit-offline".into(), Some("trunk".into()))
        .await
        .unwrap();
    git(
        &f.repo,
        &["remote", "add", "backup", f.repo.to_str().unwrap()],
    );
    let error = f
        .manager
        .create_workspace(&f.repo_id, "ambiguous".into(), None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("default remote is ambiguous"),
        "{error:#}"
    );
}

#[tokio::test]
async fn nested_branch_conflicts_suffix_the_blocked_component() {
    let f = Fixture::new().await;
    git(&f.repo, &["branch", "henrik"]);
    git(&f.repo, &["branch", "henrik-2"]);
    git(&f.repo, &["branch", "henrik-3/topic/nested"]);
    git(
        &f.repo,
        &["update-ref", "refs/remotes/origin/henrik-3/topic-2", "HEAD"],
    );
    let workspace = f.add("henrik/topic").await;
    assert_eq!(workspace.name, "henrik-topic");
    assert_eq!(workspace.branch, "henrik-3/topic-3");
    assert_eq!(
        git(&workspace.path, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/henrik-3/topic-3"
    );
    // A sibling can share an existing namespace without renaming its prefix.
    assert_eq!(f.add("henrik-3/other").await.branch, "henrik-3/other");
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
        .issue_scope(
            "token".into(),
            scope::Caller {
                execution_id: "execution".into(),
                workspace_id: workspace.id.clone(),
            },
        )
        .await;
    // Scoped processes never pull, not even their own repository; they refresh
    // merge sources of their own workspace instead.
    for target in ["other", "worker"] {
        let mut denied = Method::PullDefaultBranch {
            workspace: target.into(),
        };
        assert!(
            scope::authorize(&f.manager, Some("token"), &mut denied)
                .await
                .unwrap_err()
                .to_string()
                .contains("shoal merge refreshes")
        );
    }
    let mut denied = Method::RefreshMergeSource {
        workspace: "other".into(),
        branch: "main".into(),
    };
    assert!(
        scope::authorize(&f.manager, Some("token"), &mut denied)
            .await
            .is_err()
    );
    let mut allowed = Method::RefreshMergeSource {
        workspace: "worker".into(),
        branch: "main".into(),
    };
    scope::authorize(&f.manager, Some("token"), &mut allowed)
        .await
        .unwrap();
    let Method::RefreshMergeSource {
        workspace: target, ..
    } = allowed
    else {
        panic!("wrong method")
    };
    assert_eq!(target, workspace.id);
    let result = f.manager.pull_default_branch(&target).await.unwrap();
    assert!(result.updated);
    assert_eq!(result.previous_commit, before.trim());
    assert_eq!(result.commit, expected.trim());
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
    assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), before);
    assert!(
        !f.manager
            .pull_default_branch("worker")
            .await
            .unwrap()
            .updated
    );
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
            .pull_default_branch("worker")
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
            .pull_default_branch("worker")
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
            .pull_default_branch("worker")
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
    assert!(
        f.manager
            .pull_default_branch("worker")
            .await
            .unwrap()
            .updated
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
    assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), before);
    git(&author, &["reset", "--hard", "HEAD~1"]);
    git(&author, &["push", "--force", "origin", "main"]);
    assert!(
        !f.manager
            .pull_default_branch("worker")
            .await
            .unwrap()
            .updated
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
    git(&f.repo, &["update-ref", "refs/heads/main", before.trim()]);
    git(&author, &["reset", "--hard", expected.trim()]);
    git(&author, &["push", "origin", "main"]);
    let main_path = f.root.path().join("main checkout\nwith newline");
    git(
        &f.repo,
        &["worktree", "add", main_path.to_str().unwrap(), "main"],
    );
    assert!(
        f.manager
            .pull_default_branch("worker")
            .await
            .unwrap()
            .updated
    );
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
            .pull_default_branch("caller")
            .await
            .unwrap_err()
            .to_string()
            .contains("managed workspace")
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
}

#[tokio::test]
async fn creation_refreshes_main_instead_of_using_checkout_head() {
    let f = Fixture::new().await;
    let original = git(&f.repo, &["rev-parse", "HEAD"]);
    let author = f.remote();
    git(&f.repo, &["remote", "rename", "origin", "source"]);
    git(&f.repo, &["switch", "-c", "unrelated"]);
    let fetch_head = f.repo.join(".git/FETCH_HEAD");
    fs::write(&fetch_head, "unrelated fetch sentinel\n").unwrap();
    for (index, base) in [None, Some("main"), Some("refs/heads/main")]
        .into_iter()
        .enumerate()
    {
        fs::write(author.join("upstream"), format!("revision {index}\n")).unwrap();
        commit(&author, "upstream");
        git(&author, &["push", "origin", "main"]);
        let expected = git(&author, &["rev-parse", "HEAD"]);
        let workspace = f
            .manager
            .create_workspace(
                &f.repo_id,
                format!("worker-{index}"),
                base.map(str::to_owned),
            )
            .await
            .unwrap();
        assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), expected);
        assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
        assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), original);
        assert_eq!(workspace.base_commit.as_deref(), Some(expected.trim()));
        assert_eq!(workspace.base_ref.as_deref(), Some("refs/heads/main"));
        assert_eq!(
            fs::read_to_string(&fetch_head).unwrap(),
            "unrelated fetch sentinel\n"
        );
        assert_eq!(git(&f.repo, &["for-each-ref", "refs/shoal/pull/"]), "");
    }
}

#[tokio::test]
async fn creation_refuses_failed_refreshes_without_creating_a_branch() {
    for failure in [
        "dirty",
        "diverged",
        "missing-upstream",
        "unavailable-remote",
        "managed-main",
    ] {
        let f = Fixture::new().await;
        let owned = f.add("existing").await;
        f.remote();
        let expected_error = match failure {
            "dirty" => {
                fs::write(f.repo.join("tracked"), "local edits\n").unwrap();
                "uncommitted or untracked"
            }
            "diverged" => {
                fs::write(f.repo.join("tracked"), "local commit\n").unwrap();
                commit(&f.repo, "tracked");
                "diverged"
            }
            "missing-upstream" => {
                git(&f.repo, &["branch", "--unset-upstream", "main"]);
                "no branch upstream"
            }
            "managed-main" => {
                git(&f.repo, &["switch", "-c", "unrelated"]);
                git(&owned.path, &["switch", "main"]);
                "managed workspace"
            }
            _ => {
                git(
                    &f.repo,
                    &[
                        "remote",
                        "set-url",
                        "origin",
                        f.root.path().join("missing.git").to_str().unwrap(),
                    ],
                );
                "git failed"
            }
        };
        let before = git(&f.repo, &["rev-parse", "main"]);
        let error = f
            .manager
            .create_workspace(&f.repo_id, "worker".into(), None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains(expected_error),
            "{failure}: {error:#}"
        );
        assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
        assert_eq!(
            git(
                &f.repo,
                &["for-each-ref", "refs/heads/worker", "refs/shoal/pull/"]
            ),
            ""
        );
        let failed = f.manager.workspace("worker").await.unwrap();
        assert_eq!(failed.state, crate::state::WorkspaceState::Failed);
        assert!(!failed.path.exists());
    }
}

#[tokio::test]
async fn creation_honors_explicit_history_and_preserves_ahead_main() {
    let f = Fixture::new().await;
    let original = git(&f.repo, &["rev-parse", "HEAD"]);
    let author = f.remote();
    // Explicit history remains usable even when refreshing main would fail.
    fs::write(f.repo.join("tracked"), "local edits\n").unwrap();
    for (index, base) in ["HEAD", original.trim()].into_iter().enumerate() {
        let workspace = f
            .manager
            .create_workspace(&f.repo_id, format!("explicit-{index}"), Some(base.into()))
            .await
            .unwrap();
        assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), original);
        assert_eq!(git(&f.repo, &["rev-parse", "main"]), original);
    }
    fs::write(f.repo.join("tracked"), "initial\n").unwrap();
    let workspace = f.add("updated").await;
    assert_eq!(
        git(&workspace.path, &["rev-parse", "HEAD"]),
        git(&author, &["rev-parse", "HEAD"])
    );
    fs::write(f.repo.join("tracked"), "ahead\n").unwrap();
    commit(&f.repo, "tracked");
    let ahead = git(&f.repo, &["rev-parse", "main"]);
    assert_eq!(
        git(&f.add("ahead").await.path, &["rev-parse", "HEAD"]),
        ahead
    );
}

#[tokio::test]
async fn existing_branch_opens_without_suffix_and_reuses_owned_workspace() {
    let f = Fixture::new().await;
    git(&f.repo, &["branch", "coworker/topic"]);
    let opened = f
        .manager
        .open_branch(&f.repo_id, "coworker/topic")
        .await
        .unwrap();
    assert!(!opened.reused);
    assert_eq!(opened.workspace.branch, "coworker/topic");
    assert_eq!(opened.workspace.name, "coworker-topic");
    assert_eq!(
        opened.workspace.base_ref.as_deref(),
        Some("refs/heads/main")
    );
    assert_eq!(
        git(&opened.workspace.path, &["branch", "--show-current"]).trim(),
        "coworker/topic"
    );
    let again = f
        .manager
        .open_branch(&f.repo_id, "refs/heads/coworker/topic")
        .await
        .unwrap();
    assert!(again.reused);
    assert_eq!(again.workspace.id, opened.workspace.id);
    git(&opened.workspace.path, &["switch", "--detach"]);
    assert!(
        f.manager
            .open_branch(&f.repo_id, "coworker/topic")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn existing_branch_refuses_other_checkouts_and_name_collisions() {
    let f = Fixture::new().await;
    let error = f.manager.open_branch(&f.repo_id, "main").await.unwrap_err();
    assert!(error.to_string().contains("already checked out"));
    assert!(f.manager.list_workspaces().await.unwrap().is_empty());
    git(&f.repo, &["branch", "topic/one"]);
    f.add("topic-one").await;
    assert!(
        f.manager
            .open_branch(&f.repo_id, "topic/one")
            .await
            .is_err()
    );
    assert_eq!(f.manager.list_workspaces().await.unwrap().len(), 1);
    git(&f.repo, &["switch", "--detach"]);
    let main = f.manager.open_branch(&f.repo_id, "main").await.unwrap();
    assert_eq!(main.workspace.branch, "main");
    f.manager
        .remove_workspace(
            &main.workspace.id,
            crate::removal::BranchChoice::KeepBranch,
            0,
        )
        .await
        .unwrap();
    assert!(!main.workspace.path.exists());
    assert_eq!(
        git(&f.repo, &["rev-parse", "main"]),
        git(&f.repo, &["rev-parse", "HEAD"])
    );
}

#[tokio::test]
async fn existing_remote_branch_discovers_fetches_and_tracks_new_heads() {
    let f = Fixture::new().await;
    let author = f.remote();
    git(&author, &["switch", "-c", "coworker/topic"]);
    fs::write(author.join("coworker"), "work\n").unwrap();
    commit(&author, "coworker");
    git(&author, &["push", "origin", "coworker/topic"]);
    let branches = f.manager.branches(&f.repo_id).await.unwrap();
    assert!(
        branches
            .iter()
            .any(|b| b.selector() == "refs/remotes/origin/coworker/topic")
    );
    let opened = f
        .manager
        .open_branch(&f.repo_id, "origin/coworker/topic")
        .await
        .unwrap();
    assert_eq!(opened.workspace.branch, "coworker/topic");
    assert_eq!(
        git(&opened.workspace.path, &["rev-parse", "HEAD"]),
        git(&author, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(
            &opened.workspace.path,
            &["rev-parse", "--symbolic-full-name", "@{upstream}"]
        )
        .trim(),
        "refs/remotes/origin/coworker/topic"
    );
    assert!(
        f.manager
            .open_branch(&f.repo_id, "origin/coworker/topic")
            .await
            .unwrap()
            .reused
    );
}

#[tokio::test]
async fn existing_remote_branch_rejects_ambiguity_and_unrelated_local_branch() {
    let f = Fixture::new().await;
    let author = f.remote();
    git(&author, &["switch", "-c", "topic"]);
    git(&author, &["push", "origin", "topic"]);
    let origin = f.root.path().join("origin.git");
    git(
        &f.repo,
        &["remote", "add", "other", origin.to_str().unwrap()],
    );
    assert!(
        f.manager
            .open_branch(&f.repo_id, "topic")
            .await
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    git(&f.repo, &["branch", "topic"]);
    assert!(
        f.manager
            .open_branch(&f.repo_id, "origin/topic")
            .await
            .unwrap_err()
            .to_string()
            .contains("does not track")
    );
    assert!(f.manager.list_workspaces().await.unwrap().is_empty());
    // Explicit local selection remains usable with unavailable remotes.
    git(
        &f.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "/nonexistent/shoal-test-remote",
        ],
    );
    assert!(f.manager.open_branch(&f.repo_id, "topic").await.is_ok());
}

#[tokio::test]
async fn existing_tracking_branch_fast_forwards_and_refuses_divergence() {
    let f = Fixture::new().await;
    let author = f.remote();
    git(&author, &["switch", "-c", "topic"]);
    git(&author, &["push", "origin", "topic"]);
    git(&f.repo, &["fetch", "origin"]);
    git(&f.repo, &["branch", "--track", "topic", "origin/topic"]);
    fs::write(author.join("later"), "new work\n").unwrap();
    commit(&author, "later");
    git(&author, &["push", "origin", "topic"]);
    let opened = f
        .manager
        .open_branch(&f.repo_id, "origin/topic")
        .await
        .unwrap();
    assert_eq!(
        git(&opened.workspace.path, &["rev-parse", "HEAD"]),
        git(&author, &["rev-parse", "HEAD"])
    );
    git(&author, &["switch", "-c", "diverged"]);
    git(&author, &["push", "origin", "diverged"]);
    git(&f.repo, &["fetch", "origin"]);
    git(&f.repo, &["switch", "--track", "origin/diverged"]);
    fs::write(f.repo.join("local"), "local work\n").unwrap();
    commit(&f.repo, "local");
    let before = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["switch", "main"]);
    fs::write(author.join("remote"), "remote work\n").unwrap();
    commit(&author, "remote");
    git(&author, &["push", "origin", "diverged"]);
    assert!(
        f.manager
            .open_branch(&f.repo_id, "origin/diverged")
            .await
            .is_err()
    );
    assert_eq!(git(&f.repo, &["rev-parse", "diverged"]), before);
}

#[tokio::test]
async fn existing_default_branch_survives_normal_workspace_removal() {
    let f = Fixture::new().await;
    f.remote();
    git(
        &f.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(&f.repo, &["switch", "--detach"]);
    let opened = f.manager.open_branch(&f.repo_id, "main").await.unwrap();
    let opening = git(&f.repo, &["rev-parse", "main"]);
    assert!(opened.workspace.base_ref.is_none());
    fs::write(
        opened.workspace.path.join("new-work"),
        "workspace changes\n",
    )
    .unwrap();
    commit(&opened.workspace.path, "new-work");
    let diff = f.manager.diff_base(&opened.workspace.id).await.unwrap();
    assert_eq!(diff.commit, opening.trim());
    assert_eq!(
        git(
            &opened.workspace.path,
            &["diff", "--name-only", &diff.commit]
        )
        .trim(),
        "new-work"
    );
    let before = git(&f.repo, &["rev-parse", "main"]);
    let removed = f
        .manager
        .remove_workspace(&opened.workspace.id, crate::removal::BranchChoice::Auto, 0)
        .await
        .unwrap();
    assert!(!removed.branch_deleted);
    assert!(!opened.workspace.path.exists());
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
    assert!(f.manager.open_branch(&f.repo_id, "main").await.is_ok());
}
