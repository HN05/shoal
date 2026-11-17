use std::{fs, sync::Arc};

use super::*;
use crate::{
    daemon::{
        scope::Caller,
        test_support::{git, global_config, manager, repository},
    },
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_protocol::space_names::SpaceSelector;

struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    source: Workspace,
}

impl Fixture {
    /// A workspace `topic` with a staged, an unstaged, an untracked, a
    /// deleted and an ignored file.
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let checkout = repository(root.path(), "repo");
        fs::write(checkout.join(".gitignore"), "build/\n").unwrap();
        fs::write(checkout.join("deleted"), "deleted\n").unwrap();
        git(&checkout, &["add", "."]);
        git(&checkout, &["commit", "-m", "files"]);
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let source = manager
            .create_workspace(&repo.id, "topic".into(), None, None, None)
            .await
            .unwrap();
        let path = &source.path;
        fs::write(path.join("tracked"), "staged\n").unwrap();
        git(path, &["add", "tracked"]);
        fs::write(path.join("tracked"), "staged then changed\n").unwrap();
        fs::remove_file(path.join("deleted")).unwrap();
        fs::write(path.join("untracked"), "untracked\n").unwrap();
        fs::create_dir_all(path.join("build")).unwrap();
        fs::write(path.join("build/output"), "ignored\n").unwrap();
        Self {
            _root: root,
            manager,
            source,
        }
    }

    async fn copy(
        &self,
        checkpoint: Option<CheckpointNumber>,
        place: CopyPlace,
    ) -> Result<Workspace> {
        let source = CopySource {
            workspace: SpaceSelector::id(&self.source.id),
            checkpoint,
        };
        let repository = (self.source.repository()).into();
        (self.manager)
            .create_copy(repository, &source, place, NewWorkspace::default())
            .await
    }

    /// Record a copy of the source's files, or of its `checkpoint`, on a new
    /// branch with the Git profile `work`, which no config defines yet, and
    /// fail to create its worktree, which Git creates before the profile is
    /// looked up.
    async fn failed_pending_copy(&self, checkpoint: Option<CheckpointNumber>) -> Workspace {
        let source = CopySource {
            workspace: SpaceSelector::id(&self.source.id),
            checkpoint,
        };
        let new = NewWorkspace {
            git_profile: Some("work"),
            pending: true,
            ..Default::default()
        };
        let repository = (self.source.repository()).into();
        let place = CopyPlace::Branch(None);
        let copy = (self.manager.create_copy(repository, &source, place, new))
            .await
            .unwrap();
        assert!(copy.pending_worktree);
        assert!(!copy.path.exists());
        let selector = SpaceSelector::id(&copy.id);
        let failed = self.manager.create_pending_worktree(&selector).await;
        assert!(failed.is_err(), "{failed:?}");
        let copy = self.manager.workspace_by_id(&copy.id).await.unwrap();
        assert_eq!(copy.state, WorkspaceState::Failed);
        assert!(copy.pending_worktree && copy.path.exists());
        // Work done in the failed copy before its retry.
        fs::write(copy.path.join("mine"), "mine\n").unwrap();
        self.manager.publish_config(global_config(
            "[machine.git_profiles.work]\nuser.email = 'work@example.invalid'\n",
        ));
        copy
    }

    /// The text of `mine` in the checkpoint a retry of `copy` took before
    /// it replaced its files.
    async fn kept_before_retry(&self, copy: &Workspace) -> String {
        let checkpoints = (self.manager.checkpoints(&SpaceSelector::id(&copy.id), None))
            .await
            .unwrap()
            .unwrap();
        let kept = (checkpoints.iter())
            .find(|checkpoint| checkpoint.cause == CheckpointCause::Refresh)
            .expect("a checkpoint keeps what the retry replaced");
        git(&copy.path, &["show", &format!("{}:mine", kept.commit)])
    }

    async fn unsaved_work(&self, workspace: &Workspace) -> Vec<String> {
        let check = (self.manager)
            .check_removal(&SpaceSelector::id(&workspace.id), InspectionPolicy::GitOnly)
            .await
            .unwrap();
        let mut work = check.unsaved_work();
        if !check.commits_kept() {
            work.push(format!("{} commits", check.unpushed_commits));
        }
        work
    }
}

const STATUS: &[&str] = &["status", "--porcelain", "--untracked-files=all"];

#[tokio::test]
async fn a_copy_takes_the_files_and_diff_base_of_its_source() {
    let f = Fixture::new().await;
    let copy = f.copy(None, CopyPlace::Branch(None)).await.unwrap();
    assert_eq!(copy.branch().map(BranchName::as_str), Some("topic-copy"));
    assert_eq!(copy.state, WorkspaceState::Ready);
    assert_eq!(git(&copy.path, STATUS), git(&f.source.path, STATUS));
    assert_eq!(
        git(&copy.path, &["diff", "--cached"]),
        git(&f.source.path, &["diff", "--cached"])
    );
    assert_eq!(
        fs::read_to_string(copy.path.join("build/output")).unwrap(),
        "ignored\n"
    );
    assert_eq!(
        (&copy.base_commit, &copy.base_ref),
        (&f.source.base_commit, &f.source.base_ref)
    );
    let copied = copy.part.copied_from.unwrap();
    assert_eq!(copied.workspace.unwrap().id, f.source.id);
    assert_eq!(
        (copied.checkpoint, copied.starting_checkpoint),
        (None, 1.into())
    );
    assert_eq!(
        copied.head,
        git(&f.source.path, &["rev-parse", "HEAD"]).trim()
    );
}

#[tokio::test]
async fn a_retried_copy_takes_its_sources_files_as_they_are_then() {
    let f = Fixture::new().await;
    let copy = f.failed_pending_copy(None).await;
    let path = &f.source.path;
    git(path, &["add", "--all"]);
    git(path, &["commit", "-q", "-m", "later"]);
    fs::write(path.join("untracked"), "changed later\n").unwrap();
    let selector = SpaceSelector::id(&copy.id);
    let copy = f.manager.create_pending_worktree(&selector).await.unwrap();
    assert!(!copy.pending_worktree);
    let head = git(path, &["rev-parse", "HEAD"]);
    assert_eq!(git(&copy.path, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&copy.path, STATUS), git(path, STATUS));
    assert_eq!(
        fs::read_to_string(copy.path.join("untracked")).unwrap(),
        "changed later\n"
    );
    let email = git(&copy.path, &["config", "user.email"]);
    assert_eq!(email.trim(), "work@example.invalid");
    assert!(!copy.path.join("mine").exists());
    assert_eq!(f.kept_before_retry(&copy).await, "mine\n");
    assert_eq!(copy.part.copied_from.unwrap().head, head.trim());
}

#[tokio::test]
async fn a_retry_that_could_not_keep_the_failed_copys_files_is_refused() {
    let f = Fixture::new().await;
    let copy = f.failed_pending_copy(None).await;
    let nested = copy.path.join("nested");
    fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    fs::write(nested.join("file"), "nested\n").unwrap();
    let selector = SpaceSelector::id(&copy.id);
    let refused = f.manager.create_pending_worktree(&selector).await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(nested.join("file").exists());
    assert!(copy.path.join("mine").exists());
    let copy = f.manager.workspace_by_id(&copy.id).await.unwrap();
    assert!(copy.pending_worktree);
}

#[tokio::test]
async fn a_retried_copy_of_a_checkpoint_writes_its_files_again() {
    let f = Fixture::new().await;
    let checkpoint = f
        .manager
        .take_checkpoint(&f.manager.named("topic").await, None)
        .await
        .unwrap();
    let kept = git(&f.source.path, &["diff", "HEAD"]);
    let copy = f.failed_pending_copy(Some(checkpoint.number)).await;
    let selector = SpaceSelector::id(&copy.id);
    let copy = f.manager.create_pending_worktree(&selector).await.unwrap();
    assert!(!copy.pending_worktree);
    assert_eq!(git(&copy.path, &["diff", "HEAD"]), kept);
    let email = git(&copy.path, &["config", "user.email"]);
    assert_eq!(email.trim(), "work@example.invalid");
    assert_eq!(f.kept_before_retry(&copy).await, "mine\n");
    let copied = copy.part.copied_from.unwrap();
    assert_eq!(copied.checkpoint, Some(checkpoint.number));
}

#[tokio::test]
async fn a_copy_s_name_stays_its_requested_one() {
    let f = Fixture::new().await;
    let path = &f.source.path;
    // Branches only Git has, which no workspace owns.
    git(path, &["branch", "topic-copy"]);
    git(path, &["branch", "named"]);
    let copy = f.copy(None, CopyPlace::Branch(None)).await.unwrap();
    assert_eq!(
        (copy.name.as_str(), copy.branch().map(BranchName::as_str)),
        ("topic-copy", Some("topic-copy-2"))
    );
    // The branch's suffix leaves the directory, which is free, unsuffixed.
    assert_eq!(copy.path.file_name().unwrap(), "topic-copy");
    let named = f
        .copy(None, CopyPlace::Branch(Some("named".into())))
        .await
        .unwrap();
    assert_eq!(
        (named.name.as_str(), named.branch().map(BranchName::as_str)),
        ("named", Some("named-2"))
    );
}

#[tokio::test]
async fn a_copy_on_a_typed_branch_is_refused_rather_than_suffixed() {
    let f = Fixture::new().await;
    git(&f.source.path, &["branch", "taken"]);
    let source = CopySource {
        workspace: SpaceSelector::id(&f.source.id),
        checkpoint: None,
    };
    let typed = |branch: &str| {
        let new = NewWorkspace {
            exact_branch: true,
            ..NewWorkspace::default()
        };
        let place = CopyPlace::Branch(Some(branch.into()));
        let repository = (f.source.repository()).into();
        f.manager.create_copy(repository, &source, place, new)
    };
    typed("taken").await.unwrap_err();
    let copy = typed("fresh").await.unwrap();
    assert_eq!(copy.branch().map(BranchName::as_str), Some("fresh"));
}

#[tokio::test]
async fn a_copy_takes_the_suffix_its_branch_takes_on_a_parent_component() {
    let f = Fixture::new().await;
    let repository = f.source.repository().clone();
    let source = (f.manager)
        .create_workspace(&repository, "fix/thing".into(), None, None, None)
        .await
        .unwrap();
    // A remote branch named after the first component takes that name.
    let head = git(&source.path, &["rev-parse", "HEAD"]);
    git(
        &source.path,
        &["update-ref", "refs/remotes/origin/fix", head.trim()],
    );
    let request = CopySource {
        workspace: SpaceSelector::id(&source.id),
        checkpoint: None,
    };
    let copy = f.manager.create_copy(
        (&repository).into(),
        &request,
        CopyPlace::Branch(None),
        NewWorkspace::default(),
    );
    // A hang guard: allocation once never ended here.
    let copy = tokio::time::timeout(std::time::Duration::from_secs(120), copy)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        copy.branch().map(BranchName::as_str),
        Some("fix-2/thing-copy")
    );
}

#[tokio::test]
async fn a_copy_at_a_commit_has_no_branch() {
    let f = Fixture::new().await;
    let copy = f.copy(None, CopyPlace::Commit).await.unwrap();
    assert_eq!(copy.branch(), None);
    assert_eq!(copy.name, "topic-copy");
    assert_eq!(git(&copy.path, STATUS), git(&f.source.path, STATUS));
    assert_eq!(f.unsaved_work(&copy).await, Vec::<String>::new());
}

#[tokio::test]
async fn a_source_in_a_git_operation_is_refused() {
    let f = Fixture::new().await;
    let git_dir = git(&f.source.path, &["rev-parse", "--absolute-git-dir"]);
    fs::write(std::path::Path::new(git_dir.trim()).join("MERGE_HEAD"), "").unwrap();
    let error = f.copy(None, CopyPlace::Branch(None)).await.unwrap_err();
    assert!(format!("{error:#}").contains("Git operation"), "{error:#}");
}

#[tokio::test]
async fn a_copy_starts_from_a_checkpoint_of_a_live_or_removed_workspace() {
    let f = Fixture::new().await;
    let checkpoint = f
        .manager
        .take_checkpoint(&f.manager.named("topic").await, None)
        .await
        .unwrap();
    let kept = git(&f.source.path, &["diff", "HEAD"]);
    fs::write(f.source.path.join("untracked"), "changed later\n").unwrap();
    let live = f
        .copy(
            Some(checkpoint.number),
            CopyPlace::Branch(Some("from-live".into())),
        )
        .await
        .unwrap();
    assert_eq!(git(&live.path, &["diff", "HEAD"]), kept);
    assert_eq!(
        fs::read_to_string(live.path.join("untracked")).unwrap(),
        "untracked\n"
    );
    assert_eq!(
        live.part.copied_from.unwrap().checkpoint,
        Some(checkpoint.number)
    );

    f.manager
        .remove_selected(
            &SpaceSelector::id(&f.source.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let removed = f
        .copy(
            Some(checkpoint.number),
            CopyPlace::Branch(Some("from-removed".into())),
        )
        .await
        .unwrap();
    assert_eq!(git(&removed.path, &["diff", "HEAD"]), kept);
    let copied = removed.copied_from.clone().unwrap();
    assert_eq!(copied.workspace, None, "the source is removed");
    assert_eq!(copied.checkpoint, Some(checkpoint.number));
    assert_eq!(removed.base_ref, f.source.base_ref);
    // A removed workspace's current files are gone.
    assert!(f.copy(None, CopyPlace::Branch(None)).await.is_err());
}

#[tokio::test]
async fn a_checkpoint_that_kept_head_only_is_refused() {
    let f = Fixture::new().await;
    let checkpoint = f
        .manager
        .take_checkpoint(&f.manager.named("topic").await, None)
        .await
        .unwrap();
    let id = f.source.id.clone();
    f.manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE checkpoints SET record=json_set(record, '$.files_omitted', json('true'))
                    WHERE workspace_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let error = f
        .copy(Some(checkpoint.number), CopyPlace::Branch(None))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("could not keep"), "{error:#}");
}

#[tokio::test]
async fn only_a_copy_s_own_commits_and_changes_are_its_work() {
    let f = Fixture::new().await;
    let copy = f.copy(None, CopyPlace::Branch(None)).await.unwrap();
    assert!(
        f.unsaved_work(&f.source).await.len() == 1,
        "the source's files"
    );
    assert_eq!(f.unsaved_work(&copy).await, Vec::<String>::new());

    fs::write(copy.path.join("untracked"), "the copy's own\n").unwrap();
    assert_eq!(f.unsaved_work(&copy).await.len(), 1);
    fs::write(copy.path.join("untracked"), "untracked\n").unwrap();
    assert_eq!(f.unsaved_work(&copy).await, Vec::<String>::new());

    // A version only the index holds is the copy's own too.
    fs::write(copy.path.join("untracked"), "staged only\n").unwrap();
    git(&copy.path, &["add", "untracked"]);
    fs::write(copy.path.join("untracked"), "untracked\n").unwrap();
    assert_eq!(f.unsaved_work(&copy).await.len(), 1);
    git(
        &copy.path,
        &["rm", "--cached", "--force", "--quiet", "untracked"],
    );
    assert_eq!(f.unsaved_work(&copy).await, Vec::<String>::new());

    git(&copy.path, &["add", "--all"]);
    git(&copy.path, &["commit", "-m", "the copy's own"]);
    assert_eq!(f.unsaved_work(&copy).await, vec!["1 commits".to_owned()]);
}

#[tokio::test]
async fn an_untouched_copy_goes_without_consent_and_a_changed_one_does_not() {
    let f = Fixture::new().await;
    let untouched = f.copy(None, CopyPlace::Branch(None)).await.unwrap();
    let changed = f.copy(None, CopyPlace::Branch(None)).await.unwrap();
    fs::write(changed.path.join("untracked"), "the copy's own\n").unwrap();
    let remove = |workspace: &Workspace| {
        let id = workspace.id.clone();
        let manager = f.manager.clone();
        async move {
            manager
                .remove_selected(
                    &SpaceSelector::id(&id),
                    UnsavedWork::default(),
                    InspectionPolicy::GitOnly,
                )
                .await
        }
    };
    remove(&untouched).await.unwrap();
    assert!(!untouched.path.exists());
    assert!(remove(&changed).await.is_err());
    assert!(changed.path.join("untracked").exists());
}

#[tokio::test]
async fn agents_copy_only_live_workspaces_of_their_own_tree() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    let other = manager
        .create_workspace(f.source.repository(), "other".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&f.source.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let caller = Caller {
        space_id: f.source.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
    };
    for (workspace, allowed) in [(&f.source, true), (&child, true), (&other, false)] {
        let mut source = CopySource {
            workspace: SpaceSelector::id(&workspace.id),
            checkpoint: None,
        };
        let result = manager.copy_within(&caller, &mut source).await;
        assert_eq!(result.is_ok(), allowed, "{}", workspace.name);
        if allowed {
            assert_eq!(source.workspace, SpaceSelector::id(&workspace.id));
        }
    }
    let checkpoint = manager
        .take_checkpoint(&manager.named("other").await, None)
        .await
        .unwrap();
    manager
        .remove_selected(
            &SpaceSelector::id(&other.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let mut removed = CopySource {
        workspace: SpaceSelector::id(&other.id),
        checkpoint: Some(checkpoint.number),
    };
    assert!(manager.copy_within(&caller, &mut removed).await.is_err());
}

#[tokio::test]
async fn removing_an_untouched_copy_keeps_its_branch_when_only_it_has_the_commits() {
    let f = Fixture::new().await;
    git(&f.source.path, &["commit", "-m", "the source's own"]);
    let copy = f.copy(None, CopyPlace::Branch(None)).await.unwrap();
    let check = (f.manager)
        .check_removal(&SpaceSelector::id(&copy.id), InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert_eq!(check.unpushed_commits, 1);
    assert!(check.commits_kept() && !check.branch_preserved());
    f.manager
        .remove_selected(
            &SpaceSelector::id(&copy.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let branches = git(&f.source.path, &["branch", "--list", "topic-copy"]);
    assert!(branches.contains("topic-copy"), "{branches}");
}

#[tokio::test]
async fn a_copy_with_a_conflict_is_inspected_as_changed() {
    let f = Fixture::new().await;
    let copy = f.copy(None, CopyPlace::Branch(None)).await.unwrap();
    let blob = git(&copy.path, &["hash-object", "-w", "untracked"]);
    let stages: String = (1..=3)
        .map(|stage| format!("100644 {} {stage}\tconflicted\n", blob.trim()))
        .collect();
    // Unmerged stages, as a conflicting merge leaves them.
    let mut command = std::process::Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| name.starts_with("GIT_")) {
            command.env_remove(name);
        }
    }
    let mut index = command
        .args(["update-index", "--index-info"])
        .current_dir(&copy.path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    index
        .stdin
        .take()
        .unwrap()
        .write_all(stages.as_bytes())
        .unwrap();
    assert!(index.wait().unwrap().success());
    assert_eq!(f.unsaved_work(&copy).await.len(), 1);
}

#[tokio::test]
async fn an_agents_checkpoint_keeps_starting_files_above_the_archive_limit() {
    let f = Fixture::new().await;
    let copy = f.copy(None, CopyPlace::Branch(None)).await.unwrap();
    crate::daemon::test_support::configure(&f.manager, "[cleanup.restore]\narchive_max_mb = 0\n")
        .await;
    f.manager.checkpoint_after_agent(&copy.id).await;
    let checkpoints = (f.manager)
        .checkpoints(&SpaceSelector::id(&copy.id), None)
        .await
        .unwrap()
        .unwrap();
    assert!(!checkpoints.is_empty());
    assert!(
        checkpoints
            .iter()
            .all(|checkpoint| !checkpoint.files_omitted)
    );
}

#[tokio::test]
async fn a_copy_of_a_renamed_workspace_at_a_commit_branches_from_its_commit() {
    let f = Fixture::new().await;
    let detached = f
        .manager
        .create_commit_workspace(
            (f.source.repository()).into(),
            None,
            crate::protocol::CommitStart::Revision(None),
            Default::default(),
        )
        .await
        .unwrap();
    let selector = SpaceSelector::id(&detached.id);
    (f.manager
        .rename_space(&selector, &"Try the cache".parse().unwrap()))
    .await
    .unwrap();
    let source = CopySource {
        workspace: selector,
        checkpoint: None,
    };

    let copy = (f.manager.create_copy(
        (f.source.repository()).into(),
        &source,
        CopyPlace::Branch(None),
        NewWorkspace::default(),
    ))
    .await
    .unwrap();

    let head = git(&detached.path, &["rev-parse", "--short=7", "HEAD"]);
    assert_eq!(
        copy.branch().cloned(),
        (Some(format!("{}-copy", head.trim()))).map(BranchName::from)
    );
}
