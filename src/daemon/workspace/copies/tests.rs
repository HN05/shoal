use std::{fs, sync::Arc};

use super::*;
use crate::{
    daemon::scope::Caller,
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{git, manager, repository},
};

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

    async fn copy(&self, checkpoint: Option<u64>, place: CopyPlace) -> Result<Workspace> {
        let source = CopySource {
            workspace: self.source.name.clone(),
            checkpoint,
        };
        let repository = self.source.repository_id.as_str().into();
        (self.manager)
            .create_copy(repository, &source, place, NewWorkspace::default())
            .await
    }
}

const STATUS: &[&str] = &["status", "--porcelain", "--untracked-files=all"];

#[tokio::test]
async fn a_copy_takes_the_files_and_diff_base_of_its_source() {
    let f = Fixture::new().await;
    let copy = f.copy(None, CopyPlace::Branch(None)).await.unwrap();
    assert_eq!(copy.branch.as_deref(), Some("topic-copy"));
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
    let copied = copy.copied_from.unwrap();
    assert_eq!(copied.workspace.unwrap().id, f.source.id);
    assert_eq!((copied.checkpoint, copied.starting_checkpoint), (None, 1));
    assert_eq!(
        copied.head,
        git(&f.source.path, &["rev-parse", "HEAD"]).trim()
    );
}

#[tokio::test]
async fn a_copy_s_name_stays_its_requested_one_and_a_default_one_is_free_for_both() {
    let f = Fixture::new().await;
    let path = &f.source.path;
    // Branches only Git has, which no workspace owns.
    git(path, &["branch", "topic-copy"]);
    git(path, &["branch", "named"]);
    let copy = f.copy(None, CopyPlace::Branch(None)).await.unwrap();
    assert_eq!(
        (copy.name.as_str(), copy.branch.as_deref()),
        ("topic-copy-2", Some("topic-copy-2"))
    );
    let named = f
        .copy(None, CopyPlace::Branch(Some("named".into())))
        .await
        .unwrap();
    assert_eq!(
        (named.name.as_str(), named.branch.as_deref()),
        ("named", Some("named-2"))
    );
}

#[tokio::test]
async fn a_copy_takes_the_suffix_its_branch_takes_on_a_parent_component() {
    let f = Fixture::new().await;
    let repository = f.source.repository_id.clone();
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
        workspace: source.name.clone(),
        checkpoint: None,
    };
    let copy = f.manager.create_copy(
        repository.as_str().into(),
        &request,
        CopyPlace::Branch(None),
        NewWorkspace::default(),
    );
    // A hang guard: allocation once never ended here.
    let copy = tokio::time::timeout(std::time::Duration::from_secs(120), copy)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(copy.branch.as_deref(), Some("fix-2/thing-copy"));
}

#[tokio::test]
async fn a_copy_at_a_commit_has_no_branch() {
    let f = Fixture::new().await;
    let copy = f.copy(None, CopyPlace::Commit).await.unwrap();
    assert_eq!(copy.branch, None);
    assert_eq!(copy.name, "topic-copy");
    assert_eq!(git(&copy.path, STATUS), git(&f.source.path, STATUS));
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
    let checkpoint = f.manager.take_checkpoint("topic", None).await.unwrap();
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
        live.copied_from.unwrap().checkpoint,
        Some(checkpoint.number)
    );

    f.manager
        .remove_workspace(&f.source.id, UnsavedWork::All, InspectionPolicy::GitOnly)
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
    let copied = removed.copied_from.unwrap();
    assert_eq!(copied.workspace, None, "the source is removed");
    assert_eq!(copied.checkpoint, Some(checkpoint.number));
    assert_eq!(removed.base_ref, f.source.base_ref);
    // A removed workspace's current files are gone.
    assert!(f.copy(None, CopyPlace::Branch(None)).await.is_err());
}

#[tokio::test]
async fn a_checkpoint_that_kept_head_only_is_refused() {
    let f = Fixture::new().await;
    let checkpoint = f.manager.take_checkpoint("topic", None).await.unwrap();
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
async fn agents_copy_only_live_workspaces_of_their_own_tree() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    let other = manager
        .create_workspace(&f.source.repository_id, "other".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&f.source.id, "child".into(), None, Default::default())
        .await
        .unwrap();
    let caller = Caller {
        space_id: f.source.id.clone(),
        execution: None,
    };
    for (workspace, allowed) in [(&f.source, true), (&child, true), (&other, false)] {
        let mut source = CopySource {
            workspace: workspace.name.clone(),
            checkpoint: None,
        };
        let result = manager.copy_within(&caller, &mut source).await;
        assert_eq!(result.is_ok(), allowed, "{}", workspace.name);
        if allowed {
            assert_eq!(source.workspace, workspace.id);
        }
    }
    let checkpoint = manager.take_checkpoint("other", None).await.unwrap();
    manager
        .remove_workspace(&other.id, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    let mut removed = CopySource {
        workspace: other.name.clone(),
        checkpoint: Some(checkpoint.number),
    };
    assert!(manager.copy_within(&caller, &mut removed).await.is_err());
}
