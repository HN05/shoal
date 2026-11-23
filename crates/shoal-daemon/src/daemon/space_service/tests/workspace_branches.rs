use shoal_ids::repository_ids::RepositoryId;
use std::{fs, path::PathBuf, sync::Arc};

use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        scope,
        test_support::{commit, git, install_test_script, manager, repository},
        workspace::NewWorktreeOptions,
    },
    paths::Paths,
    protocol::{DaemonMethod, ExecutionMethod, Operation, RemovalMethod, WorkspaceMethod},
    removal::InspectionPolicy,
};
use shoal_protocol::space_names::SpaceSelector;

struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    manager: Arc<Manager>,
    repo_id: RepositoryId,
}

impl Fixture {
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo with ' quotes & $literal");
        let repo_id = manager
            .repositories
            .register(repo.to_str().unwrap().into(), None, None)
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

    async fn sync(&self) -> anyhow::Result<crate::model::SyncedRepository> {
        self.manager
            .repositories
            .sync_repository(&*self.manager, &self.repo_id.clone().into())
            .await
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

/// Begin a merge execution as the workspace's agent.
async fn merge_as_agent(
    f: &Fixture,
    workspace: &crate::model::Workspace,
) -> anyhow::Result<crate::daemon::executions::StartedExecution> {
    begin_merge_as_agent(f, workspace, false).await
}

/// Begin a merge execution as the workspace's agent, which with `push`
/// pushes the default branch afterwards.
async fn begin_merge_as_agent(
    f: &Fixture,
    workspace: &crate::model::Workspace,
    push: bool,
) -> anyhow::Result<crate::daemon::executions::StartedExecution> {
    let options = crate::model::MergeOptions {
        push: Some(push),
        ..Default::default()
    };
    begin_merge_by_agent(f, workspace, options).await
}

/// Begin a merge execution as the workspace's agent, as `options` say.
async fn begin_merge_by_agent(
    f: &Fixture,
    workspace: &crate::model::Workspace,
    options: crate::model::MergeOptions,
) -> anyhow::Result<crate::daemon::executions::StartedExecution> {
    let caller = scope::Caller {
        execution: None,
        space_id: workspace.id.clone(),
        granted: None,
        read_only: false,
        vouched: None,
    };
    let request = crate::daemon::executions::ExecutionRequest {
        merge: Some(options),
        ..Default::default()
    };
    f.manager
        .agents
        .begin_execution_placed(
            &*f.manager,
            &SpaceSelector::id(&workspace.id),
            None,
            ExecutionKind::Merge,
            Some(&caller),
            request,
        )
        .await
}

/// The approval a scoped merge is waiting for or was refused.
async fn undecided_merge(
    f: &Fixture,
    workspace: &crate::model::Workspace,
) -> crate::daemon::approvals::ApprovalRequest {
    undecided(begin_merge_as_agent(f, workspace, false).await)
}

/// The approval request `begun` waits for or was refused.
fn undecided(
    begun: anyhow::Result<crate::daemon::executions::StartedExecution>,
) -> crate::daemon::approvals::ApprovalRequest {
    let error = begun.expect_err("merge needs a decision");
    *error
        .downcast::<crate::daemon::approvals::Undecided>()
        .expect("an approval request")
        .0
}

/// Commit to the checkout's default branch a config whose
/// `require_for_merge` is `holds`, which decides the merges into it.
fn require_merge_review(f: &Fixture, holds: &str) {
    let config = format!("[review]\nrequire_for_merge = '{holds}'\n");
    fs::write(f.repo.join(".shoal.toml"), config).unwrap();
    commit(&f.repo, ".shoal.toml");
}

impl Manager {
    async fn merge_workspace(&self, selector: &str) -> anyhow::Result<crate::model::MergedBranch> {
        self.merge_and_push(selector, false).await
    }

    async fn merge_and_push(
        &self,
        selector: &str,
        push: bool,
    ) -> anyhow::Result<crate::model::MergedBranch> {
        let selector = self.named(selector).await;
        let workspace = self.spaces.workspace(&selector).await?;
        let _guard = self.gates.lock_git(workspace.repository()).await;
        let options = crate::model::MergeOptions {
            push: Some(push),
            method: None,
            message: None,
            confirmed: None,
        };
        crate::git::repo::finish_merge(
            self.spaces
                .prepare_merge(self, &selector, &options, None)
                .await?,
        )
        .await
    }
}
#[tokio::test]
async fn remote_default_controls_creation_diff_and_removal() {
    for branch in ["develop", "release/current"] {
        let f = Fixture::new().await;
        git(&f.repo, &["branch", "-m", branch]);
        let before = git(&f.repo, &["rev-parse", "HEAD"]);
        let author = f.remote();
        // A branch called main and an unrelated checkout must not override HEAD.
        git(&f.repo, &["branch", "main"]);
        git(&f.repo, &["switch", "-c", "unrelated"]);
        let expected = git(&author, &["rev-parse", "HEAD"]);
        f.sync().await.unwrap();
        let workspace = f.add("henrik/topic").await;
        let base = format!("refs/heads/{branch}");
        assert_eq!(workspace.checkout.base_ref(), Some(base.as_str()));
        assert_eq!(
            workspace.checkout.base().map(|base| base.commit.as_str()),
            Some(expected.trim())
        );
        assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), expected);
        assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
        assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), before);
        assert_eq!(
            git(&f.repo, &["symbolic-ref", "refs/remotes/origin/HEAD"]),
            format!("refs/remotes/origin/{branch}\n")
        );
        assert!(
            f.manager
                .spaces
                .check_removal(
                    &*f.manager,
                    &SpaceSelector::id(&workspace.id),
                    InspectionPolicy::IncludeDirectoryProcesses
                )
                .await
                .unwrap()
                .branch_preserved()
        );
        assert_eq!(
            f.manager
                .spaces
                .diff_base(&*f.manager, &SpaceSelector::id(&workspace.id))
                .await
                .unwrap()
                .commit,
            expected.trim()
        );
    }
}

#[tokio::test]
async fn non_main_default_sync_preserves_a_dirty_checkout_creation_starts_from() {
    let f = Fixture::new().await;
    git(&f.repo, &["branch", "-m", "develop"]);
    git(&f.repo, &["branch", "main"]);
    f.remote();
    git(&f.repo, &["remote", "rename", "origin", "upstream"]);
    let before = git(&f.repo, &["rev-parse", "HEAD"]);
    fs::write(f.repo.join("tracked"), "dirty\n").unwrap();
    f.sync().await.unwrap_err();
    for (index, base) in [
        None,
        Some("develop"),
        Some("refs/heads/develop"),
        Some("main"),
    ]
    .into_iter()
    .enumerate()
    {
        let workspace = f
            .manager
            .create_workspace(
                &f.repo_id,
                format!("local-{index}"),
                base.map(str::to_owned),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), before);
    }
    assert_eq!(git(&f.repo, &["rev-parse", "develop"]), before);
    assert_eq!(
        fs::read_to_string(f.repo.join("tracked")).unwrap(),
        "dirty\n"
    );
}

#[tokio::test]
async fn creation_starts_from_the_synced_main_instead_of_checkout_head() {
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
        f.sync().await.unwrap();
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
        assert_eq!(
            workspace.checkout.base().map(|base| base.commit.as_str()),
            Some(expected.trim())
        );
        assert_eq!(workspace.checkout.base_ref(), Some("refs/heads/main"));
        assert_eq!(
            fs::read_to_string(&fetch_head).unwrap(),
            "unrelated fetch sentinel\n"
        );
        assert_eq!(git(&f.repo, &["for-each-ref", "refs/shoal/pull/"]), "");
    }
    // Creation itself never fetches.
    let synced = git(&f.repo, &["rev-parse", "main"]);
    fs::write(author.join("upstream"), "unsynced\n").unwrap();
    commit(&author, "upstream");
    git(&author, &["push", "origin", "main"]);
    assert_eq!(
        git(&f.add("unsynced").await.path, &["rev-parse", "HEAD"]),
        synced
    );
}

#[tokio::test]
async fn sync_leaves_main_it_cannot_fast_forward() {
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
        match failure {
            "dirty" => {
                fs::write(f.repo.join("tracked"), "local edits\n").unwrap();
            }
            "diverged" => {
                fs::write(f.repo.join("tracked"), "local commit\n").unwrap();
                commit(&f.repo, "tracked");
            }
            "missing-upstream" => {
                git(&f.repo, &["branch", "--unset-upstream", "main"]);
            }
            "managed-main" => {
                git(&f.repo, &["switch", "-c", "unrelated"]);
                git(&owned.path, &["switch", "main"]);
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
            }
        }
        let before = git(&f.repo, &["rev-parse", "main"]);
        assert!(f.sync().await.is_err(), "{failure}");
        assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
        assert_eq!(git(&f.repo, &["for-each-ref", "refs/shoal/pull/"]), "");
    }
}

#[tokio::test]
async fn creation_honors_explicit_history_and_sync_preserves_ahead_main() {
    let f = Fixture::new().await;
    let original = git(&f.repo, &["rev-parse", "HEAD"]);
    let author = f.remote();
    // Explicit history remains usable even when syncing main would fail.
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
    f.sync().await.unwrap();
    let workspace = f.add("updated").await;
    assert_eq!(
        git(&workspace.path, &["rev-parse", "HEAD"]),
        git(&author, &["rev-parse", "HEAD"])
    );
    fs::write(f.repo.join("tracked"), "ahead\n").unwrap();
    commit(&f.repo, "tracked");
    let ahead = git(&f.repo, &["rev-parse", "main"]);
    assert!(!f.sync().await.unwrap().default_branch.updated);
    assert_eq!(
        git(&f.add("ahead").await.path, &["rev-parse", "HEAD"]),
        ahead
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
        disposable: false,
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
            .spaces
            .check_creation(&*f.manager, &f.repo_id, branch, existing, options)
            .await
            .unwrap();
    }
    for (repository, branch, existing, options) in [
        ("missing", None, None, none),
        (f.repo_id.as_str(), Some("bad..name"), None, none),
        (f.repo_id.as_str(), None, Some("absent"), none),
        (
            f.repo_id.as_str(),
            None,
            Some("owned"),
            options(Some("main"), None, None),
        ),
        (
            f.repo_id.as_str(),
            None,
            Some("topic"),
            NewWorktreeOptions {
                disposable: true,
                ..none
            },
        ),
        (
            f.repo_id.as_str(),
            None,
            None,
            options(Some("absent"), None, None),
        ),
        (
            f.repo_id.as_str(),
            Some("new"),
            None,
            options(None, Some("absent"), None),
        ),
        (
            f.repo_id.as_str(),
            Some("new"),
            None,
            options(None, None, Some(&*f.repo)),
        ),
        // A path that no workspace can use is refused before the branch is known.
        (
            f.repo_id.as_str(),
            None,
            None,
            options(None, None, Some(&*f.repo)),
        ),
    ] {
        f.manager
            .spaces
            .check_creation(&*f.manager, repository, branch, existing, options)
            .await
            .unwrap_err();
    }
    assert_eq!(f.manager.spaces.list_workspaces().await.unwrap().len(), 1);
    assert!(!new_path.exists());
    assert!(git(&f.repo, &["branch", "--list", "new"]).is_empty());

    git(&owned.path, &["switch", "--detach"]);
    assert!(
        f.manager
            .spaces
            .check_creation(&*f.manager, &f.repo_id, None, Some("owned"), none)
            .await
            .is_err()
    );

    // A record keeps its path after its directory and worktree are gone.
    let gone = f.add("gone").await;
    fs::remove_dir_all(&gone.path).unwrap();
    git(&f.repo, &["worktree", "prune"]);
    f.manager
        .spaces
        .check_creation(
            &*f.manager,
            &f.repo_id,
            Some("fresh"),
            None,
            options(None, None, Some(&*gone.path)),
        )
        .await
        .unwrap_err();

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
    f.manager
        .spaces
        .check_creation(&*f.manager, &f.repo_id, None, None, none)
        .await
        .unwrap_err();
}

#[tokio::test]
async fn merge_fast_forwards_merges_and_aborts_conflicts_without_a_remote() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let merged = f.manager.merge_workspace("worker").await.unwrap();
    assert!(!merged.updated);
    assert_eq!(merged.commit, git(&f.repo, &["rev-parse", "main"]).trim());
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let expected = git(&workspace.path, &["rev-parse", "HEAD"]);
    let merged = f.manager.merge_workspace("worker").await.unwrap();
    assert!(merged.updated && merged.fast_forward);
    assert_eq!(merged.default_branch, "main");
    assert_eq!(merged.commit, expected.trim());
    assert!(merged.default_refresh.skipped.is_some());
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
    let merged = f.manager.merge_workspace("worker").await.unwrap();
    assert!(merged.updated && !merged.fast_forward);
    assert_eq!(
        git(&f.repo, &["rev-list", "--count", "--merges", "main"]),
        "1\n"
    );
    assert_eq!(
        fs::read_to_string(f.repo.join("feature")).unwrap(),
        "more\n"
    );
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), "");
    // Merged work counts as retained, and the merged branch is redundant.
    let check = f
        .manager
        .spaces
        .check_removal(
            &*f.manager,
            &SpaceSelector::id(&workspace.id),
            InspectionPolicy::IncludeDirectoryProcesses,
        )
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
    f.manager.merge_workspace("worker").await.unwrap_err();
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), "");
    assert!(!f.repo.join(".git/MERGE_HEAD").exists());
    assert_eq!(
        fs::read_to_string(f.repo.join("tracked")).unwrap(),
        "main again\n"
    );
    let check = f
        .manager
        .spaces
        .check_removal(
            &*f.manager,
            &SpaceSelector::id(&workspace.id),
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert_eq!(check.unpushed_commits, 1);
    assert!(!check.branch_preserved());
}

#[tokio::test]
async fn squash_merge_on_the_remote_default_branch_counts_as_retained() {
    let f = Fixture::new().await;
    let author = f.remote();
    f.sync().await.unwrap();
    let workspace = f.add("worker").await;
    for file in ["one", "two"] {
        fs::write(workspace.path.join(file), format!("{file}\n")).unwrap();
        commit(&workspace.path, file);
    }
    let check = async || {
        f.manager
            .spaces
            .check_removal(
                &*f.manager,
                &SpaceSelector::id(&workspace.id),
                InspectionPolicy::GitOnly,
            )
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
    assert!(
        f.manager
            .spaces
            .default_branch_has_head(&*f.manager, &workspace)
            .await
            .unwrap()
    );
    // A change the default branch lacks keeps the workspace.
    fs::write(workspace.path.join("two"), "two\n").unwrap();
    commit(&workspace.path, "two");
    assert!(
        !f.manager
            .spaces
            .default_branch_has_head(&*f.manager, &workspace)
            .await
            .unwrap()
    );
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
        ExecutionKind::Merge,
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
            .agents
            .begin_execution(&*f.manager, workspace.id.as_str(), None, kind, None)
            .await
            .unwrap();
        let token = &started.plan.scope_token;
        let caller = f
            .manager
            .coordination
            .caller(&*f.manager, token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(caller.kind(), Some(kind));
        assert_eq!(caller.execution_id(), Some(&started.plan.id));
        assert_eq!(caller.space_id, workspace.id);

        for (mut method, allowed) in [
            (Operation::from(DaemonMethod::Status), true),
            (
                ExecutionMethod::CheckMerge.into(),
                kind == ExecutionKind::Merge,
            ),
            (DaemonMethod::ReloadConfig.into(), true),
            (
                RemovalMethod::StopSpace {
                    space: SpaceSelector::id(&workspace.id),
                    acknowledge_unproven: false,
                    launches: crate::protocol::QueuedLaunches::Cancel,
                    processes: crate::protocol::UntrackedProcesses::Keep,
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
            ExecutionKind::Merge,
        ] {
            for target in [&workspace, &other] {
                let mut method = ExecutionMethod::Execute {
                    new_session: false,
                    narrowing: Default::default(),
                    mode: None,
                    model: Default::default(),
                    space: SpaceSelector::id(&target.id),
                    kind: requested,
                    agent: None,
                    recover: false,
                    source: None,
                    reattach: false,
                    queue_entry: None,
                    demo: None,
                    origin: None,
                    merge: None,
                    wrapper: wrapper.clone(),
                };
                let allowed = target.id == workspace.id
                    && match requested {
                        ExecutionKind::Command => true,
                        ExecutionKind::Setup => kind != ExecutionKind::Setup,
                        ExecutionKind::Merge | ExecutionKind::Unmerge => true,
                    };
                let result = scope::authorize(&f.manager, Some(token), &mut method).await;
                assert_eq!(result.is_ok(), allowed, "{kind:?}: {method:?}: {result:?}");
                if allowed {
                    assert_eq!(result.unwrap().unwrap().kind(), Some(kind));
                    let ExecutionMethod::Execute { space: target, .. } = method else {
                        unreachable!()
                    };
                    assert_eq!(target, SpaceSelector::id(&workspace.id));
                }
            }
        }
        f.manager
            .agents
            .finish_execution(&*f.manager, started.plan.id.clone(), kind, Some(0))
            .await
            .unwrap();
        assert!(
            f.manager
                .coordination
                .caller(&*f.manager, token)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            scope::authorize(&f.manager, Some(token), &mut DaemonMethod::Status)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn setup_releases_git_gate_after_registration_and_on_preparation_failure() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let guard = f.manager.gates.lock_git(workspace.repository()).await;
    let gate = tokio::sync::OwnedMutexGuard::mutex(&guard).clone();
    drop(guard);
    assert!(
        f.manager
            .agents
            .begin_execution(
                &*f.manager,
                workspace.id.as_str(),
                None,
                ExecutionKind::Setup,
                None
            )
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
        .agents
        .begin_execution(
            &*f.manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Setup,
            None,
        )
        .await
        .unwrap();
    assert!(gate.try_lock().is_ok(), "setup only locks registration");
    f.manager
        .agents
        .finish_execution(
            &*f.manager,
            started.plan.id.clone(),
            ExecutionKind::Setup,
            Some(0),
        )
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
    f.manager.repositories.lock(&f.repo_id).await.unwrap_err();
    f.manager
        .repositories
        .sync_repository(&*f.manager, &f.repo_id.clone().into())
        .await
        .unwrap();
    let report = f
        .manager
        .spaces
        .reconcile(
            &*f.manager,
            &SpaceSelector::id(&workspace.id),
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
async fn merge_execution_holds_git_gate_through_completion_and_releases_on_failure() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let guard = f.manager.gates.lock_git(workspace.repository()).await;
    let gate = tokio::sync::OwnedMutexGuard::mutex(&guard).clone();
    drop(guard);
    for exit in [Some(0), None] {
        let started = f
            .manager
            .agents
            .begin_execution(
                &*f.manager,
                workspace.id.as_str(),
                None,
                ExecutionKind::Merge,
                None,
            )
            .await
            .unwrap();
        assert!(started.plan.merge.is_some());
        assert!(gate.try_lock().is_err());
        assert_eq!(
            f.manager
                .coordination
                .caller(&*f.manager, &started.plan.scope_token)
                .await
                .unwrap()
                .unwrap()
                .kind(),
            Some(ExecutionKind::Merge)
        );
        f.manager
            .agents
            .finish_execution(
                &*f.manager,
                started.plan.id.clone(),
                ExecutionKind::Merge,
                exit,
            )
            .await
            .unwrap();
        assert!(gate.try_lock().is_err(), "hold the gate through completion");
        drop(started);
        assert!(gate.try_lock().is_ok());
    }
    fs::write(workspace.path.join("dirty"), "preserve me").unwrap();
    assert!(
        f.manager
            .agents
            .begin_execution(
                &*f.manager,
                workspace.id.as_str(),
                None,
                ExecutionKind::Merge,
                None
            )
            .await
            .is_err()
    );
    assert!(
        gate.try_lock().is_ok(),
        "failed preparation must release the gate"
    );
}

#[tokio::test]
async fn merge_refuses_dirty_checkouts_and_other_branches() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    f.add("caller").await;
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let main = git(&f.repo, &["rev-parse", "main"]);
    fs::write(f.repo.join("scratch"), "").unwrap();
    f.manager.merge_workspace("worker").await.unwrap_err();
    fs::remove_file(f.repo.join("scratch")).unwrap();
    fs::write(workspace.path.join("scratch"), "").unwrap();
    f.manager.merge_workspace("worker").await.unwrap_err();
    fs::remove_file(workspace.path.join("scratch")).unwrap();
    git(&workspace.path, &["switch", "--detach"]);
    f.manager.merge_workspace("worker").await.unwrap_err();
    git(&workspace.path, &["switch", "worker"]);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);
}

#[tokio::test]
async fn scoped_merge_waits_for_approval_of_its_commit() {
    use crate::daemon::approvals::{ApprovalRequest, DecisionStatus, Specification};
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let main = git(&f.repo, &["rev-parse", "main"]);
    let head = git(&workspace.path, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let pending = undecided_merge(&f, &workspace).await;
    assert_eq!(pending.status, DecisionStatus::Pending);
    assert!(pending.subject().contains(&head));
    assert!(pending.reason.contains("1 commit)"), "{}", pending.reason);
    assert_eq!(undecided_merge(&f, &workspace).await.number, pending.number);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);

    // Approval lets the agent's retry merge the approved commit.
    f.manager
        .coordination
        .decide_approval(
            &*f.manager,
            pending.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    let started = merge_as_agent(&f, &workspace).await.unwrap();
    assert_eq!(started.plan.merge.as_ref().unwrap().source, head);
    f.manager
        .agents
        .finish_execution(
            &*f.manager,
            started.plan.id.clone(),
            ExecutionKind::Merge,
            Some(0),
        )
        .await
        .unwrap();
    drop(started);

    // A later commit needs a new request, which replaces the earlier one;
    // the earlier one stays outdated, keeping its number.
    fs::write(workspace.path.join("feature"), "more\n").unwrap();
    commit(&workspace.path, "feature");
    let next = undecided_merge(&f, &workspace).await;
    assert_ne!(next.number, pending.number);
    assert_eq!(next.status, DecisionStatus::Pending);
    let statuses = |requests: Vec<ApprovalRequest>| -> Vec<_> {
        requests
            .into_iter()
            .map(|request| (request.number, request.status))
            .collect()
    };
    assert_eq!(
        statuses(
            f.manager
                .coordination
                .approval_requests(&*f.manager, Some(&f.manager.named("worker").await))
                .await
                .unwrap()
        ),
        [
            (pending.number, DecisionStatus::Outdated),
            (next.number, DecisionStatus::Pending)
        ]
    );

    // Approving after the branch moved retires the stale request and tells the agent.
    fs::write(workspace.path.join("feature"), "even more\n").unwrap();
    commit(&workspace.path, "feature");
    f.manager
        .coordination
        .decide_approval(
            &*f.manager,
            next.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        statuses(
            f.manager
                .coordination
                .approval_requests(&*f.manager, Some(&f.manager.named("worker").await))
                .await
                .unwrap()
        ),
        [
            (pending.number, DecisionStatus::Outdated),
            (next.number, DecisionStatus::Outdated)
        ]
    );
    let messages = f
        .manager
        .agents
        .agent_messages(&*f.manager, &f.manager.named("worker").await)
        .await
        .unwrap();
    assert!(
        messages
            .iter()
            .any(|m| m.message.contains(&next.number.to_string())),
        "{messages:?}"
    );

    // Denial leaves the branch unmerged and refuses the same commit again.
    let main = git(&f.repo, &["rev-parse", "main"]);
    let denied = undecided_merge(&f, &workspace).await;
    let Specification::Merge(merge) = &denied.specification else {
        panic!("merge specification expected");
    };
    assert_eq!(
        merge.head,
        git(&workspace.path, &["rev-parse", "HEAD"]).trim()
    );
    f.manager
        .coordination
        .decide_approval(
            &*f.manager,
            denied.number,
            false,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    let refused = undecided_merge(&f, &workspace).await;
    assert_eq!(
        (refused.number, refused.status),
        (denied.number, DecisionStatus::Denied)
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);
}

#[tokio::test]
async fn approving_a_merge_request_keeps_it_when_the_branch_cannot_be_read() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let pending = undecided_merge(&f, &workspace).await;
    let reference = f.repo.join(".git/refs/heads/worker");
    let saved = fs::read(&reference).unwrap();
    fs::write(&reference, "not a commit\n").unwrap();
    f.manager
        .coordination
        .decide_approval(
            &*f.manager,
            pending.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap_err();
    fs::write(&reference, saved).unwrap();
    let requests = f
        .manager
        .coordination
        .approval_requests(&*f.manager, Some(&f.manager.named("worker").await))
        .await
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].status,
        crate::daemon::approvals::DecisionStatus::Pending
    );
    assert!(
        f.manager
            .agents
            .agent_messages(&*f.manager, &f.manager.named("worker").await)
            .await
            .unwrap()
            .is_empty()
    );
    f.manager
        .coordination
        .decide_approval(
            &*f.manager,
            pending.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn scoped_merge_skips_approval_when_configured() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[permissions]\nmerge = 'direct'\n",
    )
    .unwrap();
    commit(&workspace.path, ".shoal.toml");
    let started = merge_as_agent(&f, &workspace).await.unwrap();
    assert!(started.plan.merge.is_some());
    assert!(
        f.manager
            .coordination
            .approval_requests(&*f.manager, Some(&f.manager.named("worker").await))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn scoped_merge_needs_approval_while_pushes_do() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[permissions]\nmerge = 'direct'\npush = 'propose'\n",
    )
    .unwrap();
    commit(&workspace.path, ".shoal.toml");
    let pending = undecided_merge(&f, &workspace).await;
    assert_eq!(pending.target, crate::daemon::approvals::Target::Merge);
}

#[tokio::test]
async fn scoped_merge_approval_binds_and_describes_its_push() {
    use crate::daemon::approvals::{DecisionStatus, Specification};
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    f.remote();
    for file in ["one", "two"] {
        fs::write(workspace.path.join(file), "work\n").unwrap();
        commit(&workspace.path, file);
    }
    let head = git(&workspace.path, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let local = undecided_merge(&f, &workspace).await;
    assert!(local.reason.contains("2 commits"), "{}", local.reason);
    let pushing = undecided(begin_merge_as_agent(&f, &workspace, true).await);
    let Specification::Merge(merge) = &pushing.specification else {
        panic!("merge specification expected");
    };
    let push = merge.push.as_ref().expect("a push");
    assert_eq!(push.remote_branch(), "origin/main");
    assert!(pushing.reason.contains("origin"), "{}", pushing.reason);
    let subject = pushing.subject();
    assert!(
        subject.contains(&head) && subject.contains("origin/main"),
        "{subject}"
    );
    // A new commit needs a new request.
    fs::write(workspace.path.join("three"), "work\n").unwrap();
    commit(&workspace.path, "three");
    let newer = undecided(begin_merge_as_agent(&f, &workspace, true).await);
    // The local request no longer covers a merge that pushes.
    let requests = f
        .manager
        .coordination
        .approval_requests(&*f.manager, Some(&f.manager.named("worker").await))
        .await
        .unwrap();
    let replaced = requests
        .iter()
        .find(|request| request.number == local.number);
    assert!(replaced.is_none_or(|request| request.status == DecisionStatus::Outdated));
    // An approval covers its destination: another upstream branch needs another.
    f.manager
        .coordination
        .decide_approval(
            &*f.manager,
            newer.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    git(&f.repo, &["push", "origin", "main:release"]);
    git(&f.repo, &["fetch", "origin"]);
    git(
        &f.repo,
        &["branch", "--set-upstream-to=origin/release", "main"],
    );
    let moved = undecided(begin_merge_as_agent(&f, &workspace, true).await);
    assert_ne!(moved.number, newer.number);
    assert_eq!(moved.status, DecisionStatus::Pending);
    assert!(moved.reason.contains("origin/release"), "{}", moved.reason);
}

#[tokio::test]
async fn a_merge_warning_recorded_after_approval_needs_another_decision() {
    use crate::daemon::approvals::{DecisionStatus, Specification};
    let f = Fixture::new().await;
    fs::write(
        f.repo.join(".shoal.toml"),
        "[conflicts]\nsensitive = ['version']\n",
    )
    .unwrap();
    commit(&f.repo, ".shoal.toml");
    let workspace = f.add("worker").await;
    fs::write(workspace.path.join("version"), "2\n").unwrap();
    commit(&workspace.path, "version");
    let approved = undecided_merge(&f, &workspace).await;
    f.manager
        .coordination
        .decide_approval(
            &*f.manager,
            approved.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    // The default branch changes the same file, and a check at the same
    // HEAD records it after the decision.
    fs::write(f.repo.join("version"), "3\n").unwrap();
    commit(&f.repo, "version");
    f.manager
        .spaces
        .check_conflicts(&*f.manager, &SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    let warned = undecided_merge(&f, &workspace).await;
    assert_ne!(warned.number, approved.number);
    assert_eq!(warned.status, DecisionStatus::Pending);
    let Specification::Merge(merge) = &warned.specification else {
        panic!("merge specification expected");
    };
    assert_eq!(merge.changed_on_both_sides, ["version"]);
    assert!(warned.reason.ends_with(": version."), "{}", warned.reason);
}

#[tokio::test]
async fn an_approved_merge_lets_no_other_method_or_message_through() {
    use crate::{
        daemon::approvals::{DecisionStatus, Specification},
        forge::items::action::MergeMethod,
    };
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let approve = async |number| {
        f.manager
            .coordination
            .decide_approval(&*f.manager, number, true, None, Default::default(), None)
            .await
            .unwrap();
    };
    let squash = |message: &str| crate::model::MergeOptions {
        method: Some(MergeMethod::Squash),
        message: Some(message.into()),
        ..Default::default()
    };
    let approved = undecided_merge(&f, &workspace).await;
    approve(approved.number).await;
    let squashed = undecided(begin_merge_by_agent(&f, &workspace, squash("Fix login")).await);
    assert_ne!(squashed.number, approved.number);
    assert_eq!(squashed.status, DecisionStatus::Pending);
    let Specification::Merge(merge) = &squashed.specification else {
        panic!("merge specification expected");
    };
    assert_eq!(
        (merge.method, merge.message.as_deref()),
        (MergeMethod::Squash, Some("Fix login"))
    );
    assert!(
        squashed
            .reason
            .starts_with("Squash worker (1 commit) into main as \"Fix login\""),
        "{}",
        squashed.reason
    );
    approve(squashed.number).await;
    let reworded = undecided(begin_merge_by_agent(&f, &workspace, squash("Fix it")).await);
    assert_ne!(reworded.number, squashed.number);
    assert_eq!(reworded.status, DecisionStatus::Pending);
}

#[tokio::test]
async fn a_pushing_merge_approval_covers_no_other_local_commits_of_main() {
    use crate::daemon::approvals::{DecisionStatus, Specification};
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    f.remote();
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let approved = undecided(begin_merge_as_agent(&f, &workspace, true).await);
    let Specification::Merge(merge) = &approved.specification else {
        panic!("merge specification expected");
    };
    assert_eq!(merge.unpushed, None);
    f.manager
        .coordination
        .decide_approval(
            &*f.manager,
            approved.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    // A sync brings main up to its upstream, which the push publishes anyway.
    let author = f.root.path().join("author");
    fs::write(author.join("upstream"), "upstream\n").unwrap();
    commit(&author, "upstream");
    git(&author, &["push", "origin", "HEAD:main"]);
    git(&f.repo, &["pull", "--ff-only"]);
    let started = begin_merge_as_agent(&f, &workspace, true).await;
    assert!(started.is_ok(), "{:?}", started.err());
    drop(started);
    // Another merge into main locally, which the push would publish too.
    fs::write(f.repo.join("other"), "other\n").unwrap();
    commit(&f.repo, "other");
    let local = git(&f.repo, &["rev-parse", "main"]).trim().to_owned();
    let widened = undecided(begin_merge_as_agent(&f, &workspace, true).await);
    assert_ne!(widened.number, approved.number);
    assert_eq!(widened.status, DecisionStatus::Pending);
    let Specification::Merge(merge) = &widened.specification else {
        panic!("merge specification expected");
    };
    assert_eq!(merge.unpushed, Some(local.as_str().into()));
    assert!(
        widened.reason.contains("1 commit that origin/main lacks"),
        "{}",
        widened.reason
    );
}

#[tokio::test]
async fn unpushed_commits_count_against_a_remapped_upstream() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let author = f.remote();
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    // main tracks origin's main as origin/cache-main, and origin/main is gone.
    git(
        &f.repo,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/cache-*",
        ],
    );
    git(&f.repo, &["update-ref", "-d", "refs/remotes/origin/main"]);
    git(&f.repo, &["fetch", "origin"]);
    git(
        &f.repo,
        &["branch", "--set-upstream-to=origin/cache-main", "main"],
    );
    git(&f.repo, &["merge", "--ff-only", "origin/cache-main"]);
    // main takes a commit that only another of origin's branches has.
    fs::write(author.join("other"), "other\n").unwrap();
    commit(&author, "other");
    git(&author, &["push", "origin", "HEAD:other"]);
    git(&f.repo, &["fetch", "origin"]);
    git(&f.repo, &["merge", "--ff-only", "origin/cache-other"]);
    let preview = f
        .manager
        .spaces
        .preview_merge(
            &*f.manager,
            &f.manager.named("worker").await,
            &crate::model::MergeOptions {
                push: Some(true),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(preview.unpushed, 1);
    assert!(preview.unpushed_default().is_some());
}

#[tokio::test]
async fn a_confirmed_merge_refuses_a_moved_branch_or_destination() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    f.remote();
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let confirmed = async || crate::model::MergeOptions {
        push: Some(true),
        method: None,
        message: None,
        confirmed: Some(
            f.manager
                .spaces
                .preview_merge(
                    &*f.manager,
                    &f.manager.named("worker").await,
                    &crate::model::MergeOptions {
                        push: Some(true),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .unwrap()
                .confirmation(),
        ),
    };
    let refused = async |options: &crate::model::MergeOptions| {
        f.manager
            .spaces
            .prepare_merge(&*f.manager, &f.manager.named("worker").await, options, None)
            .await
            .is_err()
    };
    // The merge asks for another method than the one confirmed.
    let mut options = confirmed().await;
    options.method = Some(crate::forge::items::action::MergeMethod::Squash);
    assert!(refused(&options).await);
    // An agent commits while the question is open.
    let options = confirmed().await;
    fs::write(workspace.path.join("feature"), "more\n").unwrap();
    commit(&workspace.path, "feature");
    assert!(refused(&options).await);
    // Another workspace merges locally into main, which the push would publish.
    let options = confirmed().await;
    fs::write(f.repo.join("other"), "other\n").unwrap();
    commit(&f.repo, "other");
    assert!(refused(&options).await);
    git(&f.repo, &["reset", "--hard", "HEAD~1"]);
    // A sync that brings main up to its upstream publishes nothing new.
    let options = confirmed().await;
    let author = f.root.path().join("author");
    fs::write(author.join("upstream"), "upstream\n").unwrap();
    commit(&author, "upstream");
    git(&author, &["push", "origin", "HEAD:main"]);
    git(&f.repo, &["pull", "--ff-only"]);
    assert!(
        f.manager
            .spaces
            .prepare_merge(
                &*f.manager,
                &f.manager.named("worker").await,
                &options,
                None
            )
            .await
            .is_ok()
    );
    // The default branch's upstream moves to another branch.
    let options = confirmed().await;
    git(&f.repo, &["push", "origin", "main:release"]);
    git(&f.repo, &["fetch", "origin"]);
    git(
        &f.repo,
        &["branch", "--set-upstream-to=origin/release", "main"],
    );
    assert!(refused(&options).await);
    let plan = f
        .manager
        .spaces
        .prepare_merge(
            &*f.manager,
            &f.manager.named("worker").await,
            &confirmed().await,
            None,
        )
        .await;
    assert!(plan.unwrap().push.is_some());
}

#[tokio::test]
async fn a_confirmed_merge_refuses_a_warning_recorded_after_the_question() {
    let f = Fixture::new().await;
    fs::write(
        f.repo.join(".shoal.toml"),
        "[conflicts]\nsensitive = ['version']\n",
    )
    .unwrap();
    commit(&f.repo, ".shoal.toml");
    let workspace = f.add("worker").await;
    for (dir, file) in [(&workspace.path, "feature"), (&f.repo, "other")] {
        fs::write(dir.join(file), "work\n").unwrap();
        commit(dir, file);
        fs::write(dir.join("version"), "2\n").unwrap();
        commit(dir, "version");
    }
    let confirmed = async || crate::model::MergeOptions {
        push: Some(false),
        method: None,
        message: None,
        confirmed: Some(
            f.manager
                .spaces
                .preview_merge(
                    &*f.manager,
                    &f.manager.named("worker").await,
                    &crate::model::MergeOptions {
                        push: Some(false),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .unwrap()
                .confirmation(),
        ),
    };
    // A conflict check records the files while the question is open, with
    // the branch, main and the push unchanged.
    let options = confirmed().await;
    f.manager
        .spaces
        .check_conflicts(&*f.manager, &SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    f.manager
        .spaces
        .prepare_merge(
            &*f.manager,
            &f.manager.named("worker").await,
            &options,
            None,
        )
        .await
        .unwrap_err();
    // A question that named them lets the merge go ahead.
    let options = confirmed().await;
    let named = options.confirmed.as_ref().unwrap();
    assert_eq!(named.changed_on_both_sides, ["version"]);
    assert!(
        f.manager
            .spaces
            .prepare_merge(
                &*f.manager,
                &f.manager.named("worker").await,
                &options,
                None
            )
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn merge_turned_off_refuses_runs_and_agent_requests() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[merge]\nenabled = false\n",
    )
    .unwrap();
    commit(&workspace.path, ".shoal.toml");
    f.manager.merge_workspace("worker").await.unwrap_err();
    merge_as_agent(&f, &workspace).await.unwrap_err();
    assert!(
        f.manager
            .coordination
            .approval_requests(&*f.manager, Some(&f.manager.named("worker").await))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_merge_without_a_review_is_refused_before_its_approval() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    require_merge_review(&f, "agents");
    let head = git(&workspace.path, &["rev-parse", "--short", "HEAD"]);
    let refused = merge_as_agent(&f, &workspace).await.unwrap_err();
    let refused = format!("{refused:#}");
    assert!(refused.contains(head.trim()), "{refused}");
    // The review it misses is queued once the merge lets go of the Git gate.
    assert!(refused.contains("queued"), "{refused}");
    assert!(
        f.manager
            .coordination
            .approval_requests(&*f.manager, Some(&f.manager.named("worker").await))
            .await
            .unwrap()
            .is_empty()
    );
    // "agents" leaves the user's own merge alone.
    f.manager.merge_workspace("worker").await.unwrap();
}

#[tokio::test]
async fn all_refuses_the_users_merge_before_asking_about_it() {
    let f = Fixture::new().await;
    f.add("worker").await;
    require_merge_review(&f, "all");
    let main = git(&f.repo, &["rev-parse", "main"]);
    let selector = f.manager.named("worker").await;
    (f.manager.spaces.preview_merge(
        &*f.manager,
        &selector,
        &crate::model::MergeOptions {
            push: Some(false),
            ..Default::default()
        },
        None,
    ))
    .await
    .unwrap_err();
    f.manager.merge_workspace("worker").await.unwrap_err();
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);
}

#[tokio::test]
async fn merge_refreshes_the_default_branch_and_needs_it_outside_workspaces() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    let author = f.remote();
    let upstream = git(&author, &["rev-parse", "HEAD"]);
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let merged = f.manager.merge_workspace("worker").await.unwrap();
    assert!(merged.default_refresh.updated);
    assert_eq!(merged.default_refresh.commit, upstream.trim());
    assert!(merged.updated && !merged.fast_forward);
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
    // Without a checkout of the default branch the merge commit is made
    // aside and the default branch moves to it.
    git(&f.repo, &["switch", "-c", "side"]);
    let main = git(&f.repo, &["rev-parse", "main"]);
    fs::write(workspace.path.join("feature"), "diverged\n").unwrap();
    commit(&workspace.path, "feature");
    let merged = f.manager.merge_workspace("worker").await.unwrap();
    assert!(merged.updated && !merged.fast_forward);
    assert_eq!(
        git(&f.repo, &["rev-parse", "main"]).trim(),
        merged.commit.as_str()
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main^1"]), main);
    // A branch that has the default branch fast-forwards it.
    git(&workspace.path, &["merge", "--no-edit", "main"]);
    fs::write(workspace.path.join("feature"), "ahead\n").unwrap();
    commit(&workspace.path, "feature");
    let expected = git(&workspace.path, &["rev-parse", "HEAD"]);
    let merged = f.manager.merge_workspace("worker").await.unwrap();
    assert!(merged.updated && merged.fast_forward);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
    // A managed workspace holding the default branch blocks merging.
    let holder = f.add("holder").await;
    git(&holder.path, &["switch", "main"]);
    fs::write(workspace.path.join("feature"), "held\n").unwrap();
    commit(&workspace.path, "feature");
    f.manager.merge_workspace("worker").await.unwrap_err();
    assert!(f.manager.merge_workspace("holder").await.is_err());
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), expected);
}

#[tokio::test]
async fn merge_pushes_the_default_branch_to_its_upstream() {
    let f = Fixture::new().await;
    let workspace = f.add("worker").await;
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let main = git(&f.repo, &["rev-parse", "main"]);
    f.manager.merge_and_push("worker", true).await.unwrap_err();
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main);
    f.remote();
    let origin = f.root.path().join("origin.git");
    let merged = f.manager.merge_and_push("worker", true).await.unwrap();
    assert_eq!(merged.pushed_to.as_deref(), Some("origin"));
    assert_eq!(git(&origin, &["rev-parse", "main"]).trim(), merged.commit);
    assert_eq!(
        git(&f.repo, &["rev-parse", "origin/main"]).trim(),
        merged.commit
    );
    // A rejected push keeps the merge and says so.
    let hook = origin.join("hooks/pre-receive");
    install_test_script(&hook, "#!/bin/sh\nexit 1\n");
    fs::write(workspace.path.join("feature"), "more\n").unwrap();
    commit(&workspace.path, "feature");
    let expected = git(&workspace.path, &["rev-parse", "HEAD"]);
    f.manager.merge_and_push("worker", true).await.unwrap_err();
    assert_eq!(git(&f.repo, &["rev-parse", "main^2"]), expected);
    assert_eq!(git(&origin, &["rev-parse", "main"]).trim(), merged.commit);
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
        .spaces
        .adopt_workspace(&*f.manager, &f.repo_id, &path, false)
        .await
        .unwrap();
    assert_eq!(w.name, "adopt/topic");
    assert_eq!(w.state, crate::state::WorkspaceState::Ready);
    assert_eq!(w.checkout.base_ref(), Some("refs/heads/main"));
    assert!(w.checkout.git_dir().is_some());
    assert_eq!(
        fs::read_to_string(w.checkout.git_dir().unwrap().join("shoal-workspace")).unwrap(),
        format!("{}\n", w.id)
    );
    assert_eq!(git(&path, &["status", "--porcelain"]), before);
    assert_eq!(
        f.manager
            .spaces
            .adopt_workspace(&*f.manager, &f.repo_id, &path, false)
            .await
            .unwrap()
            .id,
        w.id
    );
    assert_eq!(f.manager.spaces.usage.activity(&w.id), 1);
    let restored = Manager::open(Paths::for_test(f.root.path())).await.unwrap();
    assert_eq!(
        restored
            .spaces
            .workspace_by_id(&w.id)
            .await
            .unwrap()
            .checkout
            .git_dir(),
        w.checkout.git_dir()
    );
    assert!(
        restored
            .spaces
            .verify_worktree(&*restored, &w)
            .await
            .is_ok()
    );
    assert!(
        f.manager
            .spaces
            .verify_worktree(&*f.manager, &w)
            .await
            .is_ok()
    );
    let mut method = WorkspaceMethod::AdoptWorkspace {
        repository: f.repo_id.clone().into(),
        path,
        copy: false,
    };
    f.manager
        .agents
        .executions()
        .issue_scope(
            "adoption-test".into(),
            scope::Caller {
                space_id: w.id,
                execution: Some(scope::ScopedExecution {
                    id: "test".into(),
                    kind: ExecutionKind::Command,
                    agent_execution: None,
                }),
                granted: None,
                read_only: false,
                vouched: None,
            },
        )
        .await;
    assert!(
        scope::authorize(&f.manager, Some(&"adoption-test".into()), &mut method)
            .await
            .is_err()
    );
}
