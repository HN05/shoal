use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc};

use crate::{
    daemon::{
        scope,
        workspace::{ExecutionKind, Manager, NewWorktreeOptions},
    },
    paths::Paths,
    protocol::{DaemonMethod, ExecutionMethod, Operation, RemovalMethod, WorkspaceMethod},
    removal::InspectionPolicy,
    test_support::{commit, git, manager, repository},
};

struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    manager: Arc<Manager>,
    repo_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo with ' quotes & $literal");
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
            .create_workspace(&self.repo_id, name.into(), None, None, None)
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
    // Unpushed work keeps the branch, and with it the suffix.
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    f.manager
        .remove_workspace(
            &workspace.id,
            crate::removal::UnsavedWork::default(),
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert_eq!(f.add("feature").await.branch, "feature-6");
    git(&f.repo, &["branch", "concurrent"]);
    let (first, second) = tokio::join!(f.add("concurrent"), f.add("concurrent-2"));
    assert_ne!(first.branch, second.branch);
}

#[tokio::test]
async fn remote_default_controls_creation_diff_and_removal() {
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
                .check_removal(&workspace.id, InspectionPolicy::IncludeDirectoryProcesses)
                .await
                .unwrap()
                .branch_preserved()
        );
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
                None,
                None,
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
        .create_workspace(
            &f.repo_id,
            "explicit".into(),
            Some("main".into()),
            None,
            None,
        )
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
            .create_workspace(&f.repo_id, "detached".into(), None, None, None)
            .await
            .is_err()
    );
    f.manager
        .create_workspace(
            &f.repo_id,
            "explicit-detached".into(),
            Some("trunk".into()),
            None,
            None,
        )
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
            .create_workspace(&f.repo_id, "offline".into(), None, None, None)
            .await
            .is_err()
    );
    f.manager
        .create_workspace(
            &f.repo_id,
            "explicit-offline".into(),
            Some("trunk".into()),
            None,
            None,
        )
        .await
        .unwrap();
    git(
        &f.repo,
        &["remote", "add", "backup", f.repo.to_str().unwrap()],
    );
    let error = f
        .manager
        .create_workspace(&f.repo_id, "ambiguous".into(), None, None, None)
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
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), expected);
        assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
        assert_eq!(
            git(&f.repo, &["rev-parse", "refs/remotes/source/main"]),
            expected
        );
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
            .create_workspace(&f.repo_id, "worker".into(), None, None, None)
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
            .create_workspace(
                &f.repo_id,
                format!("explicit-{index}"),
                Some(base.into()),
                None,
                None,
            )
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
async fn branch_compatibility_preserves_allocation_and_existing_branch_policies() {
    let mut cases = vec![
        ("HEAD".to_owned(), "HEAD".to_owned(), true),
        ("@".to_owned(), "workspace".to_owned(), true),
        ("topic/HEAD".to_owned(), "topic-HEAD".to_owned(), false),
        ("HEAD/topic".to_owned(), "HEAD-topic".to_owned(), false),
        ("topic/@".to_owned(), "topic--".to_owned(), false),
        ("@/topic".to_owned(), "topic".to_owned(), false),
    ];
    for len in [39, 40, 41, 63, 64, 65] {
        for digit in ["a", "B", "g"] {
            let name = digit.repeat(len);
            cases.push((
                name.clone(),
                digit.repeat(len.min(64)),
                digit != "g" && matches!(len, 40 | 64),
            ));
            cases.push((
                format!("topic/{name}"),
                format!("topic-{name}").chars().take(64).collect(),
                false,
            ));
        }
    }
    for (name, workspace_name, reserved) in cases {
        let f = Fixture::new().await;
        let allocated = f.add(&name).await;
        let expected = if reserved {
            format!("{name}-2")
        } else {
            name.clone()
        };
        assert_eq!(allocated.branch, expected);
        assert_eq!(allocated.name, workspace_name);
        assert_eq!(
            allocated.path.file_name().unwrap().to_str().unwrap(),
            workspace_name
        );
        assert_eq!(
            git(&allocated.path, &["symbolic-ref", "HEAD"]),
            format!("refs/heads/{expected}\n")
        );
        f.manager
            .remove_workspace(
                &allocated.id,
                crate::removal::UnsavedWork::All,
                InspectionPolicy::IncludeDirectoryProcesses,
            )
            .await
            .unwrap();

        let reference = format!("refs/heads/{name}");
        git(&f.repo, &["update-ref", &reference, "refs/heads/main"]);
        let before = git(&f.repo, &["rev-parse", &reference]);
        let opened = f
            .manager
            .open_branch(&f.repo_id, &reference, None, None, None)
            .await;
        if reserved {
            let error = opened.unwrap_err();
            assert!(
                error.to_string().contains("reserved by Worktrunk"),
                "{name}: {error:#}"
            );
            assert!(f.manager.list_workspaces().await.unwrap().is_empty());
        } else {
            let workspace = opened.unwrap().workspace;
            assert_eq!(workspace.branch, name);
            assert_eq!(workspace.name, workspace_name);
            assert_eq!(workspace.path, allocated.path);
            assert_eq!(
                git(&workspace.path, &["symbolic-ref", "HEAD"]),
                format!("{reference}\n")
            );
        }
        assert_eq!(git(&f.repo, &["rev-parse", &reference]), before);
    }
}

#[tokio::test]
async fn branch_selection_rejects_revision_shorthand() {
    let f = Fixture::new().await;
    git(&f.repo, &["switch", "-c", "previous"]);
    git(&f.repo, &["switch", "main"]);
    assert_eq!(
        git(&f.repo, &["check-ref-format", "--branch", "@{-1}"]),
        "previous\n"
    );
    for name in ["@{-1}", "main~0", "main^0", "@{0}"] {
        assert!(
            crate::git::check_branch_name(Some(&f.repo), name)
                .await
                .is_err(),
            "{name}"
        );
        assert!(
            f.manager
                .open_branch(&f.repo_id, name, None, None, None)
                .await
                .is_err(),
            "{name}"
        );
    }
    assert!(f.manager.list_workspaces().await.unwrap().is_empty());
}

#[tokio::test]
async fn existing_branch_opens_without_suffix_and_reuses_owned_workspace() {
    let f = Fixture::new().await;
    git(&f.repo, &["branch", "coworker/topic"]);
    let opened = f
        .manager
        .open_branch(&f.repo_id, "coworker/topic", None, None, None)
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
        .open_branch(&f.repo_id, "refs/heads/coworker/topic", None, None, None)
        .await
        .unwrap();
    assert!(again.reused);
    assert_eq!(again.workspace.id, opened.workspace.id);
    assert_eq!(f.manager.usage.activity(&opened.workspace.id), 1);
    git(&opened.workspace.path, &["switch", "--detach"]);
    assert!(
        f.manager
            .open_branch(&f.repo_id, "coworker/topic", None, None, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn existing_branch_records_an_explicit_base_for_new_worktrees() {
    let f = Fixture::new().await;
    git(&f.repo, &["switch", "-c", "stack/base"]);
    fs::write(f.repo.join("base"), "base\n").unwrap();
    commit(&f.repo, "base");
    git(&f.repo, &["switch", "-c", "stack/top"]);
    fs::write(f.repo.join("top"), "top\n").unwrap();
    commit(&f.repo, "top");
    git(&f.repo, &["switch", "main"]);
    let opened = f
        .manager
        .open_branch(
            &f.repo_id,
            "stack/top",
            None,
            None,
            Some("stack/base".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        opened.workspace.base_ref.as_deref(),
        Some("refs/heads/stack/base")
    );
    assert_eq!(
        f.manager
            .diff_base(&opened.workspace.id)
            .await
            .unwrap()
            .commit,
        git(&f.repo, &["rev-parse", "stack/base"]).trim()
    );
    let error = f
        .manager
        .open_branch(&f.repo_id, "stack/top", None, None, Some("main".into()))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("only to new worktrees"),
        "{error}"
    );
}

#[tokio::test]
async fn existing_branch_refuses_other_checkouts_and_name_collisions() {
    let f = Fixture::new().await;
    let error = f
        .manager
        .open_branch(&f.repo_id, "main", None, None, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already checked out"));
    assert!(f.manager.list_workspaces().await.unwrap().is_empty());
    git(&f.repo, &["branch", "topic/one"]);
    f.add("topic-one").await;
    assert!(
        f.manager
            .open_branch(&f.repo_id, "topic/one", None, None, None)
            .await
            .is_err()
    );
    assert_eq!(f.manager.list_workspaces().await.unwrap().len(), 1);
    git(&f.repo, &["switch", "--detach"]);
    let main = f
        .manager
        .open_branch(&f.repo_id, "main", None, None, None)
        .await
        .unwrap();
    assert_eq!(main.workspace.branch, "main");
    f.manager
        .remove_workspace(
            &main.workspace.id,
            crate::removal::UnsavedWork::default(),
            InspectionPolicy::IncludeDirectoryProcesses,
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
async fn creation_check_refuses_what_creation_refuses_without_creating() {
    let f = Fixture::new().await;
    git(&f.repo, &["branch", "topic"]);
    let owned = f.add("owned").await;
    let options = |base, git_profile, path| NewWorktreeOptions {
        git_profile,
        path,
        base,
    };
    let none = NewWorktreeOptions::default();
    let new_path = f.root.path().join("elsewhere");
    for (branch, existing, options) in [
        (None, None, none),
        (
            Some("new"),
            None,
            options(Some("main"), None, Some(&*new_path)),
        ),
        (None, Some("topic"), options(Some("main"), None, None)),
        (None, Some("owned"), none),
        // The branch is unknown, so it may reopen the workspace at its path.
        (None, None, options(None, None, Some(&*owned.path))),
    ] {
        f.manager
            .check_creation(&f.repo_id, branch, existing, options)
            .await
            .unwrap();
    }
    for (repository, branch, existing, options, refusal) in [
        ("missing", None, None, none, "not registered"),
        (&*f.repo_id, Some("bad..name"), None, none, "branch name"),
        (
            &*f.repo_id,
            None,
            Some("absent"),
            none,
            "missing or ambiguous",
        ),
        (
            &*f.repo_id,
            None,
            Some("owned"),
            options(Some("main"), None, None),
            "only to new worktrees",
        ),
        (
            &*f.repo_id,
            None,
            None,
            options(Some("absent"), None, None),
            "not a commit",
        ),
        (
            &*f.repo_id,
            Some("new"),
            None,
            options(None, Some("absent"), None),
            "git profile absent",
        ),
        (
            &*f.repo_id,
            Some("new"),
            None,
            options(None, None, Some(&*f.repo)),
            "overlaps",
        ),
        // A path that no workspace can use is refused before the branch is known.
        (
            &*f.repo_id,
            None,
            None,
            options(None, None, Some(&*f.repo)),
            "overlaps",
        ),
        (
            &*f.repo_id,
            Some("owned"),
            None,
            options(None, None, Some(&*new_path)),
            "name already exists",
        ),
    ] {
        let error = f
            .manager
            .check_creation(repository, branch, existing, options)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains(refusal),
            "{refusal}: {error:#}"
        );
    }
    assert_eq!(f.manager.list_workspaces().await.unwrap().len(), 1);
    assert!(!new_path.exists());
    assert!(git(&f.repo, &["branch", "--list", "new"]).is_empty());

    git(&owned.path, &["switch", "--detach"]);
    assert!(
        f.manager
            .check_creation(&f.repo_id, None, Some("owned"), none)
            .await
            .is_err()
    );

    // A registration without a reserved directory keeps it unreserved.
    let id = f.repo_id.clone();
    f.manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE repositories SET workspaces_dir=NULL WHERE id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    f.manager
        .check_creation(&f.repo_id, Some("other"), None, none)
        .await
        .unwrap();
    let repo = f.manager.repository(&f.repo_id).await.unwrap();
    assert_eq!(repo.workspaces_dir, None);

    // A record keeps its path after its directory and worktree are gone.
    let gone = f.add("gone").await;
    fs::remove_dir_all(&gone.path).unwrap();
    git(&f.repo, &["worktree", "prune"]);
    let error = f
        .manager
        .check_creation(
            &f.repo_id,
            Some("fresh"),
            None,
            options(None, None, Some(&*gone.path)),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("overlaps a recorded workspace"),
        "{error:#}"
    );

    let id = f.repo_id.clone();
    f.manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO repository_removals(repository_id) VALUES (?1)",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let error = f
        .manager
        .check_creation(&f.repo_id, None, None, none)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("removal is incomplete"),
        "{error:#}"
    );
}

#[tokio::test]
async fn creation_check_asks_the_remote_for_a_new_branch_base_only() {
    let f = Fixture::new().await;
    let author = f.remote();
    git(&author, &["push", "origin", "HEAD:refs/heads/unfetched"]);
    // A tracking ref the remote no longer has.
    git(
        &f.repo,
        &["update-ref", "refs/remotes/origin/stale", "HEAD"],
    );
    git(&f.repo, &["branch", "topic"]);
    let base = |base| NewWorktreeOptions {
        base: Some(base),
        ..NewWorktreeOptions::default()
    };
    for (branch, existing, options) in [
        (Some("new"), None, base("origin/unfetched")),
        (None, None, base("origin/unfetched")),
        (None, Some("topic"), base("origin/main")),
        // An unsettled branch may still open a local branch, which uses the
        // stale ref without fetching.
        (None, None, base("origin/stale")),
        (None, Some("topic"), base("origin/stale")),
        // Opening a remote branch fetches and creates it before resolving the
        // base, which may name it in any form.
        (None, Some("origin/unfetched"), base("origin/unfetched")),
        (None, Some("origin/unfetched"), base("unfetched~1")),
    ] {
        f.manager
            .check_creation(&f.repo_id, branch, existing, options)
            .await
            .unwrap();
    }
    for (branch, existing, options, refusal) in [
        (
            Some("new"),
            None,
            base("origin/missing"),
            "not a branch on origin",
        ),
        (
            Some("new"),
            None,
            base("origin/stale"),
            "not a branch on origin",
        ),
        (None, None, base("origin/missing"), "not a branch on origin"),
        // Opening an existing branch compares against the base without fetching it.
        (
            None,
            Some("topic"),
            base("origin/unfetched"),
            "not a commit",
        ),
    ] {
        let error = f
            .manager
            .check_creation(&f.repo_id, branch, existing, options)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains(refusal),
            "{refusal}: {error:#}"
        );
    }
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
        .open_branch(&f.repo_id, "origin/coworker/topic", None, None, None)
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
            .open_branch(&f.repo_id, "origin/coworker/topic", None, None, None)
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
            .open_branch(&f.repo_id, "topic", None, None, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    git(&f.repo, &["branch", "topic"]);
    assert!(
        f.manager
            .open_branch(&f.repo_id, "origin/topic", None, None, None)
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
    assert!(
        f.manager
            .open_branch(&f.repo_id, "topic", None, None, None)
            .await
            .is_ok()
    );
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
        .open_branch(&f.repo_id, "origin/topic", None, None, None)
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
            .open_branch(&f.repo_id, "origin/diverged", None, None, None)
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
    let opened = f
        .manager
        .open_branch(&f.repo_id, "main", None, None, None)
        .await
        .unwrap();
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
        .remove_workspace(
            &opened.workspace.id,
            crate::removal::UnsavedWork::default(),
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert!(!removed.branch_outcome.is_deleted());
    assert!(!opened.workspace.path.exists());
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
    assert!(
        f.manager
            .open_branch(&f.repo_id, "main", None, None, None)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn land_fast_forwards_merges_and_aborts_conflicts_without_a_remote() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let landed = f.manager.land_workspace("worker").await.unwrap();
    assert!(!landed.updated);
    assert_eq!(landed.commit, git(&f.repo, &["rev-parse", "main"]).trim());
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let expected = git(&workspace.path, &["rev-parse", "HEAD"]);
    let landed = f.manager.land_workspace("worker").await.unwrap();
    assert!(landed.updated && landed.fast_forward);
    assert_eq!(landed.default_branch, "main");
    assert_eq!(landed.commit, expected.trim());
    assert!(landed.default_refresh.skipped.is_some());
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
    assert_eq!(
        fs::read_to_string(f.repo.join("feature")).unwrap(),
        "work\n"
    );
    // Diverged histories get a merge commit in the default checkout.
    fs::write(f.repo.join("tracked"), "main side\n").unwrap();
    commit(&f.repo, "tracked");
    fs::write(workspace.path.join("feature"), "more\n").unwrap();
    commit(&workspace.path, "feature");
    let landed = f.manager.land_workspace("worker").await.unwrap();
    assert!(landed.updated && !landed.fast_forward);
    assert_eq!(
        git(&f.repo, &["rev-list", "--count", "--merges", "main"]),
        "1\n"
    );
    assert_eq!(
        fs::read_to_string(f.repo.join("feature")).unwrap(),
        "more\n"
    );
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), "");
    // Landed work counts as retained, and the merged branch is redundant.
    let check = f
        .manager
        .check_removal(&workspace.id, InspectionPolicy::IncludeDirectoryProcesses)
        .await
        .unwrap();
    assert_eq!(check.unpushed_commits, 0);
    assert!(check.branch_preserved());
    // A conflicting merge is aborted and leaves the default checkout untouched.
    fs::write(f.repo.join("tracked"), "main again\n").unwrap();
    commit(&f.repo, "tracked");
    let main = git(&f.repo, &["rev-parse", "main"]);
    fs::write(workspace.path.join("tracked"), "worker again\n").unwrap();
    commit(&workspace.path, "tracked");
    let error = f.manager.land_workspace("worker").await.unwrap_err();
    assert!(error.to_string().contains("git merge main"), "{error:#}");
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), "");
    assert!(!f.repo.join(".git/MERGE_HEAD").exists());
    assert_eq!(
        fs::read_to_string(f.repo.join("tracked")).unwrap(),
        "main again\n"
    );
    let check = f
        .manager
        .check_removal(&workspace.id, InspectionPolicy::IncludeDirectoryProcesses)
        .await
        .unwrap();
    assert_eq!(check.unpushed_commits, 1);
    assert!(!check.branch_preserved());
}

#[tokio::test]
async fn squash_merge_on_the_remote_default_branch_counts_as_retained() {
    let f = Fixture::new().await;
    let author = f.remote();
    let workspace = f.add("worker").await;
    for file in ["one", "two"] {
        fs::write(workspace.path.join(file), format!("{file}\n")).unwrap();
        commit(&workspace.path, file);
    }
    let check = async || {
        f.manager
            .check_removal(&workspace.id, InspectionPolicy::GitOnly)
            .await
            .unwrap()
    };
    assert_eq!(check().await.unpushed_commits, 2);
    assert!(!check().await.branch_preserved());
    // The forge squashes the branch onto main, which only the remote-tracking
    // branch has; the workspace's own commits are never pushed.
    git(&author, &["pull", "-q", "--no-rebase", "origin", "main"]);
    git(
        &author,
        &["fetch", "-q", workspace.path.to_str().unwrap(), "worker"],
    );
    git(&author, &["merge", "-q", "--squash", "FETCH_HEAD"]);
    git(
        &author,
        &[
            "-c",
            "user.name=Forge",
            "-c",
            "user.email=forge@example.invalid",
            "commit",
            "-q",
            "-m",
            "squash",
        ],
    );
    git(&author, &["push", "-q", "origin", "main"]);
    git(&f.repo, &["fetch", "-q", "origin"]);
    let merged = check().await;
    assert_eq!(merged.unpushed_commits, 0);
    assert!(merged.branch_preserved());
    // A clone may keep only the remote-tracking default branch.
    git(&f.repo, &["remote", "set-head", "origin", "main"]);
    git(&f.repo, &["checkout", "-q", "--detach"]);
    git(&f.repo, &["branch", "-q", "-D", "main"]);
    let merged = check().await;
    assert_eq!(merged.unpushed_commits, 0);
    assert!(merged.matches_default_branch);
}

#[tokio::test]
async fn default_branch_has_the_changes_of_a_branch_rebased_after_its_merge() {
    let f = Fixture::new().await;
    let author = f.remote();
    let workspace = f.add("worker").await;
    fs::write(workspace.path.join("one"), "one\n").unwrap();
    commit(&workspace.path, "one");
    let merged_head = git(&workspace.path, &["rev-parse", "HEAD"]);
    let forge = |args: &[&str]| {
        let mut command = vec![
            "-c",
            "user.name=Forge",
            "-c",
            "user.email=forge@example.invalid",
        ];
        command.extend(args);
        git(&author, &command);
    };
    fs::write(author.join("other"), "other\n").unwrap();
    forge(&["add", "other"]);
    forge(&["commit", "-q", "-m", "other"]);
    forge(&["push", "-q", "origin", "main"]);
    // The agent rebases onto that main while the forge merges the PR's
    // original commit, which only the remote has.
    git(&workspace.path, &["fetch", "-q", "origin"]);
    git(&workspace.path, &["rebase", "-q", "origin/main"]);
    forge(&[
        "fetch",
        "-q",
        workspace.path.to_str().unwrap(),
        merged_head.trim(),
    ]);
    forge(&["merge", "-q", "--no-ff", "-m", "merge", "FETCH_HEAD"]);
    forge(&["push", "-q", "origin", "main"]);
    assert!(f.manager.default_branch_has_head(&workspace).await.unwrap());
    // A change the default branch lacks keeps the workspace.
    fs::write(workspace.path.join("two"), "two\n").unwrap();
    commit(&workspace.path, "two");
    assert!(!f.manager.default_branch_has_head(&workspace).await.unwrap());
}

#[tokio::test]
async fn execution_scope_preserves_role_authorization_and_expires() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let other = f.add("other").await;
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    for kind in [
        ExecutionKind::Command,
        ExecutionKind::Land,
        ExecutionKind::Setup,
    ] {
        if kind == ExecutionKind::Setup {
            fs::write(
                workspace.path.join(".shoal.toml"),
                "setup.command = 'setup.sh'\n",
            )
            .unwrap();
        }
        let started = f
            .manager
            .begin_execution(&workspace.id, None, kind, None)
            .await
            .unwrap();
        let token = started.plan.scope_token.as_str();
        let caller = f.manager.caller(token).await.unwrap().unwrap();
        assert_eq!(caller.kind(), Some(kind));
        assert_eq!(caller.execution_id(), Some(started.plan.id.as_str()));
        assert_eq!(caller.workspace_id, workspace.id);

        for (mut method, allowed) in [
            (Operation::from(DaemonMethod::Status), true),
            (
                ExecutionMethod::CheckLanding.into(),
                kind == ExecutionKind::Land,
            ),
            (DaemonMethod::ReloadConfig.into(), false),
            (
                RemovalMethod::StopWorkspace {
                    workspace: workspace.id.clone(),
                    acknowledge_unproven: false,
                }
                .into(),
                false,
            ),
        ] {
            let result = scope::authorize(&f.manager, Some(token), &mut method).await;
            assert_eq!(result.is_ok(), allowed, "{kind:?}: {method:?}: {result:?}");
        }
        for requested in [
            ExecutionKind::Command,
            ExecutionKind::Setup,
            ExecutionKind::Land,
        ] {
            for target in [&workspace, &other] {
                let mut method = ExecutionMethod::Execute {
                    workspace: target.name.clone(),
                    kind: requested,
                    agent: None,
                    recover: false,
                    reattach: false,
                    queue_entry: None,
                    demo: None,
                    origin: None,
                    wrapper: wrapper.clone(),
                };
                let allowed = target.id == workspace.id
                    && match requested {
                        ExecutionKind::Command => true,
                        ExecutionKind::Setup => kind != ExecutionKind::Setup,
                        ExecutionKind::Land => true,
                    };
                let result = scope::authorize(&f.manager, Some(token), &mut method).await;
                assert_eq!(result.is_ok(), allowed, "{kind:?}: {method:?}: {result:?}");
                if allowed {
                    assert_eq!(result.unwrap().unwrap().kind(), Some(kind));
                    let ExecutionMethod::Execute {
                        workspace: target, ..
                    } = method
                    else {
                        unreachable!()
                    };
                    assert_eq!(target, workspace.id);
                }
            }
        }
        f.manager
            .finish_execution(started.plan.id.clone(), kind, Some(0))
            .await
            .unwrap();
        assert!(f.manager.caller(token).await.unwrap().is_none());
        assert!(
            scope::authorize(&f.manager, Some(token), &mut DaemonMethod::Status)
                .await
                .unwrap_err()
                .to_string()
                .contains("expired or unknown")
        );
    }
}

#[tokio::test]
async fn setup_releases_git_gate_after_registration_and_on_preparation_failure() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let guard = f.manager.gates.lock_git(&workspace.repository_id).await;
    let gate = tokio::sync::OwnedMutexGuard::mutex(&guard).clone();
    drop(guard);
    assert!(
        f.manager
            .begin_execution(&workspace.id, None, ExecutionKind::Setup, None)
            .await
            .is_err()
    );
    assert!(gate.try_lock().is_ok());
    fs::write(
        workspace.path.join(".shoal.toml"),
        "setup.command = 'setup.sh'\n",
    )
    .unwrap();
    let started = f
        .manager
        .begin_execution(&workspace.id, None, ExecutionKind::Setup, None)
        .await
        .unwrap();
    assert!(gate.try_lock().is_ok(), "setup only locks registration");
    f.manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Setup, Some(0))
        .await
        .unwrap();
}

#[tokio::test]
async fn incomplete_removal_allows_refresh_recovery_and_diagnosis() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let id = f.repo_id.clone();
    f.manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO repository_removals(repository_id) VALUES (?1)",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let error = f.manager.lock_repository(&f.repo_id).await.unwrap_err();
    assert!(error.to_string().contains("removal is incomplete"));
    f.manager
        .sync_repository(&f.repo_id.clone().into())
        .await
        .unwrap();
    let report = f
        .manager
        .reconcile(
            &workspace.id,
            crate::daemon::recovery::ReconcileOptions {
                repair: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(report.issues.is_empty(), "{:?}", report.issues);
}

#[tokio::test]
async fn land_execution_holds_git_gate_through_completion_and_releases_on_failure() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let guard = f.manager.gates.lock_git(&workspace.repository_id).await;
    let gate = tokio::sync::OwnedMutexGuard::mutex(&guard).clone();
    drop(guard);
    for exit in [Some(0), None] {
        let started = f
            .manager
            .begin_execution(&workspace.id, None, ExecutionKind::Land, None)
            .await
            .unwrap();
        assert!(started.plan.land.is_some());
        assert!(gate.try_lock().is_err());
        assert_eq!(
            f.manager
                .caller(&started.plan.scope_token)
                .await
                .unwrap()
                .unwrap()
                .kind(),
            Some(ExecutionKind::Land)
        );
        f.manager
            .finish_execution(started.plan.id.clone(), ExecutionKind::Land, exit)
            .await
            .unwrap();
        assert!(gate.try_lock().is_err(), "hold the gate through completion");
        drop(started);
        assert!(gate.try_lock().is_ok());
    }
    fs::write(workspace.path.join("dirty"), "preserve me").unwrap();
    assert!(
        f.manager
            .begin_execution(&workspace.id, None, ExecutionKind::Land, None)
            .await
            .is_err()
    );
    assert!(
        gate.try_lock().is_ok(),
        "failed preparation must release the gate"
    );
}

#[tokio::test]
async fn land_refuses_dirty_checkouts_and_other_branches() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    f.add("caller").await;
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let main = git(&f.repo, &["rev-parse", "main"]);
    fs::write(f.repo.join("scratch"), "").unwrap();
    let error = f.manager.land_workspace("worker").await.unwrap_err();
    assert!(
        format!("{error:#}").contains("uncommitted or untracked"),
        "{error:#}"
    );
    fs::remove_file(f.repo.join("scratch")).unwrap();
    fs::write(workspace.path.join("scratch"), "").unwrap();
    let error = f.manager.land_workspace("worker").await.unwrap_err();
    assert!(
        format!("{error:#}").contains("not ready to land"),
        "{error:#}"
    );
    fs::remove_file(workspace.path.join("scratch")).unwrap();
    git(&workspace.path, &["switch", "--detach"]);
    let error = f.manager.land_workspace("worker").await.unwrap_err();
    assert!(format!("{error:#}").contains("not on worker"), "{error:#}");
    git(&workspace.path, &["switch", "worker"]);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);
}

/// Begin a land execution as the workspace's agent.
async fn land_as_agent(
    f: &Fixture,
    workspace: &crate::model::Workspace,
) -> anyhow::Result<crate::daemon::workspace::StartedExecution> {
    let caller = scope::Caller {
        execution: None,
        workspace_id: workspace.id.clone(),
    };
    f.manager
        .begin_execution(&workspace.id, None, ExecutionKind::Land, Some(&caller))
        .await
}

/// The approval a scoped landing is waiting for or was refused.
async fn undecided_land(
    f: &Fixture,
    workspace: &crate::model::Workspace,
) -> crate::daemon::approvals::ApprovalRequest {
    let error = land_as_agent(f, workspace)
        .await
        .expect_err("landing needs a decision");
    *error
        .downcast::<crate::daemon::approvals::Undecided>()
        .expect("an approval request")
        .0
}

#[tokio::test]
async fn scoped_land_waits_for_approval_of_its_commit() {
    use crate::daemon::approvals::{ApprovalRequest, DecisionStatus, Specification};
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let main = git(&f.repo, &["rev-parse", "main"]);
    let head = git(&workspace.path, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let pending = undecided_land(&f, &workspace).await;
    assert_eq!(pending.status, DecisionStatus::Pending);
    assert_eq!(pending.subject(), format!("land worker at {head}"));
    assert_eq!(pending.reason, "merge worker into main");
    assert_eq!(undecided_land(&f, &workspace).await.id, pending.id);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);

    // Approval lets the agent's retry land the approved commit.
    f.manager
        .decide_approval(pending.id, true, None, Default::default())
        .await
        .unwrap();
    let started = land_as_agent(&f, &workspace).await.unwrap();
    assert_eq!(started.plan.land.as_ref().unwrap().source, head);
    f.manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Land, Some(0))
        .await
        .unwrap();
    drop(started);

    // A later commit needs a new request, which replaces the earlier one;
    // the earlier one stays outdated, keeping its number.
    fs::write(workspace.path.join("feature"), "more\n").unwrap();
    commit(&workspace.path, "feature");
    let next = undecided_land(&f, &workspace).await;
    assert_ne!(next.id, pending.id);
    assert_eq!(next.status, DecisionStatus::Pending);
    let statuses = |requests: Vec<ApprovalRequest>| -> Vec<_> {
        requests
            .into_iter()
            .map(|request| (request.id, request.status))
            .collect()
    };
    assert_eq!(
        statuses(f.manager.approval_requests(Some("worker")).await.unwrap()),
        [
            (pending.id, DecisionStatus::Outdated),
            (next.id, DecisionStatus::Pending)
        ]
    );

    // Approving after the branch moved retires the stale request and tells the agent.
    fs::write(workspace.path.join("feature"), "even more\n").unwrap();
    commit(&workspace.path, "feature");
    let error = f
        .manager
        .decide_approval(next.id, true, None, Default::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("is stale"), "{error:#}");
    assert_eq!(
        statuses(f.manager.approval_requests(Some("worker")).await.unwrap()),
        [
            (pending.id, DecisionStatus::Outdated),
            (next.id, DecisionStatus::Outdated)
        ]
    );
    let messages = f.manager.agent_messages("worker").await.unwrap();
    assert!(
        messages
            .iter()
            .any(|m| m.message.contains(&format!("Approval {} ", next.id))
                && m.message.contains("stale")),
        "{messages:?}"
    );

    // Denial leaves the branch unlanded and refuses the same commit again.
    let main = git(&f.repo, &["rev-parse", "main"]);
    let denied = undecided_land(&f, &workspace).await;
    let Specification::Land(land) = &denied.specification else {
        panic!("land specification expected");
    };
    assert_eq!(
        land.head,
        git(&workspace.path, &["rev-parse", "HEAD"]).trim()
    );
    f.manager
        .decide_approval(denied.id, false, None, Default::default())
        .await
        .unwrap();
    let refused = undecided_land(&f, &workspace).await;
    assert_eq!(
        (refused.id, refused.status),
        (denied.id, DecisionStatus::Denied)
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);
}

#[tokio::test]
async fn approving_a_land_request_keeps_it_when_the_branch_cannot_be_read() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let pending = undecided_land(&f, &workspace).await;
    let reference = f.repo.join(".git/refs/heads/worker");
    let saved = fs::read(&reference).unwrap();
    fs::write(&reference, "not a commit\n").unwrap();
    let error = f
        .manager
        .decide_approval(pending.id, true, None, Default::default())
        .await
        .unwrap_err();
    fs::write(&reference, saved).unwrap();
    assert!(!error.to_string().contains("stale"), "{error:#}");
    let requests = f.manager.approval_requests(Some("worker")).await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].status,
        crate::daemon::approvals::DecisionStatus::Pending
    );
    assert!(f.manager.agent_messages("worker").await.unwrap().is_empty());
    f.manager
        .decide_approval(pending.id, true, None, Default::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn scoped_land_skips_approval_when_configured() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[permissions]\nland = 'direct'\n",
    )
    .unwrap();
    commit(&workspace.path, ".shoal.toml");
    let started = land_as_agent(&f, &workspace).await.unwrap();
    assert!(started.plan.land.is_some());
    assert!(
        f.manager
            .approval_requests(Some("worker"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn land_refreshes_the_default_branch_and_needs_it_outside_workspaces() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let author = f.remote();
    let upstream = git(&author, &["rev-parse", "HEAD"]);
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let landed = f.manager.land_workspace("worker").await.unwrap();
    assert!(landed.default_refresh.updated);
    assert_eq!(landed.default_refresh.commit, upstream.trim());
    assert!(landed.updated && !landed.fast_forward);
    assert_eq!(
        git(&f.repo, &["rev-parse", "main^1"]).trim(),
        upstream.trim()
    );
    assert_eq!(
        fs::read_to_string(f.repo.join("feature")).unwrap(),
        "work\n"
    );
    assert_eq!(
        fs::read_to_string(f.repo.join("upstream")).unwrap(),
        "remote\n"
    );
    assert_eq!(git(&f.repo, &["for-each-ref", "refs/shoal/"]), "");
    // Without a checkout of the default branch only fast-forwards land.
    git(&f.repo, &["switch", "-c", "side"]);
    let main = git(&f.repo, &["rev-parse", "main"]);
    fs::write(workspace.path.join("feature"), "diverged\n").unwrap();
    commit(&workspace.path, "feature");
    let error = f.manager.land_workspace("worker").await.unwrap_err();
    assert!(error.to_string().contains("not checked out"), "{error:#}");
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);
    git(&workspace.path, &["merge", "--no-edit", "main"]);
    let expected = git(&workspace.path, &["rev-parse", "HEAD"]);
    let landed = f.manager.land_workspace("worker").await.unwrap();
    assert!(landed.updated && landed.fast_forward);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
    // A managed workspace holding the default branch blocks landing.
    let holder = f.add("holder").await;
    git(&holder.path, &["switch", "main"]);
    fs::write(workspace.path.join("feature"), "held\n").unwrap();
    commit(&workspace.path, "feature");
    let error = f.manager.land_workspace("worker").await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("checked out in workspace holder"),
        "{error:#}"
    );
    assert!(f.manager.land_workspace("holder").await.is_err());
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
}

#[tokio::test]
async fn land_pushes_the_default_branch_to_its_upstream() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let main = git(&f.repo, &["rev-parse", "main"]);
    let error = f.manager.land_and_push("worker", true).await.unwrap_err();
    assert!(error.to_string().contains("no upstream"), "{error:#}");
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);
    f.remote();
    let origin = f.root.path().join("origin.git");
    let landed = f.manager.land_and_push("worker", true).await.unwrap();
    assert_eq!(landed.pushed_to.as_deref(), Some("origin"));
    assert_eq!(git(&origin, &["rev-parse", "main"]).trim(), landed.commit);
    assert_eq!(
        git(&f.repo, &["rev-parse", "origin/main"]).trim(),
        landed.commit
    );
    // A rejected push keeps the landing and says so.
    let hook = origin.join("hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(workspace.path.join("feature"), "more\n").unwrap();
    commit(&workspace.path, "feature");
    let expected = git(&workspace.path, &["rev-parse", "HEAD"]);
    let error = f.manager.land_and_push("worker", true).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("but pushing main to origin failed"),
        "{error:#}"
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main^2"]), expected);
    assert_eq!(git(&origin, &["rev-parse", "main"]).trim(), landed.commit);
}

impl Manager {
    async fn land_workspace(&self, selector: &str) -> anyhow::Result<crate::model::LandedBranch> {
        self.land_and_push(selector, false).await
    }

    async fn land_and_push(
        &self,
        selector: &str,
        push: bool,
    ) -> anyhow::Result<crate::model::LandedBranch> {
        let workspace = self.workspace(selector).await?;
        let _guard = self.gates.lock_git(&workspace.repository_id).await;
        crate::git::repo::finish_land(self.prepare_land(selector).await?, push).await
    }
}

#[tokio::test]
async fn copy_adoption_records_ownership_and_finishes_ready() {
    let f = Fixture::new().await;
    let source = f.add("source").await;
    fs::write(source.path.join("copied"), "copied files\n").unwrap();
    let copied = f
        .manager
        .adopt_workspace(&f.repo_id, &source.path, true)
        .await
        .unwrap();

    assert_eq!(copied.branch, "source-2");
    assert_eq!(copied.state, crate::state::WorkspaceState::Ready);
    assert_eq!(
        fs::read_to_string(copied.path.join("copied")).unwrap(),
        "copied files\n"
    );
    assert!(copied.git_dir.is_some() && copied.git_dir_id.is_some());
    assert_eq!(
        git(&f.repo, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        3
    );
}

#[tokio::test]
async fn adoption_replaces_markers_of_forgotten_workspaces_only() {
    let f = Fixture::new().await;
    let marked = |branch: &str, owner: &str| {
        let path = f.root.path().join(branch.replace('/', "-"));
        git(
            &f.repo,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
        let marker =
            PathBuf::from(git(&path, &["rev-parse", "--path-format=absolute", "--git-dir"]).trim())
                .join("shoal-workspace");
        fs::write(&marker, format!("{owner}\n")).unwrap();
        (path, marker)
    };
    // A marker outliving its record, e.g. after a state reset, is replaced.
    let (path, marker) = marked("adopt/stale", "forgotten-workspace");
    let w = f
        .manager
        .adopt_workspace(&f.repo_id, &path, false)
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&marker).unwrap(), format!("{}\n", w.id));
    // One naming a recorded workspace refuses adoption and stays in place.
    let (path, marker) = marked("adopt/owned", &w.id);
    let error = f
        .manager
        .adopt_workspace(&f.repo_id, &path, false)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("already owned by adopt-stale"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), format!("{}\n", w.id));
}

#[tokio::test]
async fn adoption_preserves_dirty_worktree_and_persists_identity_and_readiness() {
    let f = Fixture::new().await;
    let path = f.root.path().join("external");
    git(
        &f.repo,
        &[
            "worktree",
            "add",
            "-b",
            "adopt/topic",
            path.to_str().unwrap(),
        ],
    );
    fs::write(path.join("tracked"), "unfinished work\n").unwrap();
    fs::write(
        path.join(".shoal.toml"),
        "setup.command = 'missing'\nhooks.post_setup = 'missing'\ngit_profile = 'missing'\n",
    )
    .unwrap();
    let before = git(&path, &["status", "--porcelain"]);
    let w = f
        .manager
        .adopt_workspace(&f.repo_id, &path, false)
        .await
        .unwrap();
    assert_eq!(w.name, "adopt-topic");
    assert_eq!(w.state, crate::state::WorkspaceState::Ready);
    assert_eq!(w.base_ref.as_deref(), Some("refs/heads/main"));
    assert!(w.git_dir.is_some() && w.git_dir_id.is_some());
    assert_eq!(
        fs::read_to_string(w.git_dir.as_ref().unwrap().join("shoal-workspace")).unwrap(),
        format!("{}\n", w.id)
    );
    assert_eq!(git(&path, &["status", "--porcelain"]), before);
    assert_eq!(
        f.manager
            .adopt_workspace(&f.repo_id, &path, false)
            .await
            .unwrap()
            .id,
        w.id
    );
    assert_eq!(f.manager.usage.activity(&w.id), 1);
    let restored = Manager::open(Paths::for_test(f.root.path())).await.unwrap();
    assert_eq!(
        restored.workspace(&w.id).await.unwrap().git_dir_id,
        w.git_dir_id
    );
    assert!(restored.verify_worktree(&w).await.is_ok());
    assert!(f.manager.verify_worktree(&w).await.is_ok());
    let mut method = WorkspaceMethod::AdoptWorkspace {
        repository: f.repo_id.clone().into(),
        path,
        copy: false,
    };
    f.manager
        .executions
        .issue_scope(
            "adoption-test".into(),
            scope::Caller {
                workspace_id: w.id,
                execution: Some(scope::ScopedExecution {
                    id: "test".into(),
                    kind: ExecutionKind::Command,
                    agent: None,
                }),
            },
        )
        .await;
    assert!(
        scope::authorize(&f.manager, Some("adoption-test"), &mut method)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn adoption_refuses_invalid_roots_and_moved_owned_worktrees() {
    let f = Fixture::new().await;
    assert!(
        f.manager
            .adopt_workspace(&f.repo_id, &f.repo, false)
            .await
            .is_err()
    );
    let path = f.root.path().join("external");
    git(
        &f.repo,
        &["worktree", "add", "--detach", path.to_str().unwrap()],
    );
    assert!(
        f.manager
            .adopt_workspace(&f.repo_id, &path, false)
            .await
            .is_err()
    );
    git(&path, &["switch", "-c", "adopt/topic"]);
    git(&f.repo, &["worktree", "lock", path.to_str().unwrap()]);
    assert!(
        f.manager
            .adopt_workspace(&f.repo_id, &path, false)
            .await
            .is_err()
    );
    git(&f.repo, &["worktree", "unlock", path.to_str().unwrap()]);
    fs::create_dir(path.join("child")).unwrap();
    assert!(
        f.manager
            .adopt_workspace(&f.repo_id, &path.join("child"), false)
            .await
            .is_err()
    );
    let other = Fixture::new().await;
    assert!(
        other
            .manager
            .adopt_workspace(&other.repo_id, &path, false)
            .await
            .is_err()
    );
    let owned = f.add("owned").await;
    let moved = f.root.path().join("moved");
    git(
        &f.repo,
        &[
            "worktree",
            "move",
            owned.path.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    git(&moved, &["switch", "-c", "renamed"]);
    assert!(
        f.manager
            .adopt_workspace(&f.repo_id, &moved, false)
            .await
            .is_err()
    );
    f.add("adopt-topic").await;
    assert!(
        f.manager
            .adopt_workspace(&f.repo_id, &path, false)
            .await
            .is_err()
    );
    assert_eq!(f.manager.list_workspaces().await.unwrap().len(), 2);
    assert!(path.join("tracked").exists());
}

#[tokio::test]
async fn adoption_of_default_branch_uses_opening_commit_and_retains_branch() {
    let f = Fixture::new().await;
    git(&f.repo, &["switch", "--detach"]);
    let path = f.root.path().join("external");
    git(
        &f.repo,
        &["worktree", "add", path.to_str().unwrap(), "main"],
    );
    // A symbolic remote HEAD lets default discovery work from the detached main checkout.
    git(
        &f.repo,
        &["remote", "add", "origin", "https://example.invalid/repo"],
    );
    git(
        &f.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let w = f
        .manager
        .adopt_workspace(&f.repo_id, &path, false)
        .await
        .unwrap();
    assert!(w.base_ref.is_none());
    assert_eq!(
        w.base_commit.as_deref(),
        Some(git(&path, &["rev-parse", "HEAD"]).trim())
    );
    f.manager
        .remove_workspace(
            &w.id,
            crate::removal::UnsavedWork::default(),
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert!(!path.exists());
    assert!(
        !git(&f.repo, &["branch", "--list", "main"])
            .trim()
            .is_empty()
    );
}
