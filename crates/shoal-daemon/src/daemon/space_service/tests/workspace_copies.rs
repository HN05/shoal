use crate::daemon::manager::Manager;
use crate::daemon::target_checks::Checked;
use crate::model::Workspace;
use crate::protocol::CopySource;
use std::{fs, sync::Arc};

use crate::{
    daemon::test_support::{git, manager, repository},
    protocol::{Operation, WorkspaceMethod},
    refusal::Reason,
    removal::{InspectionPolicy, UnsavedWork},
    state::WorkspaceState,
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
            .repositories
            .register(checkout.to_str().unwrap().into(), None, None)
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
}

#[tokio::test]
async fn a_copy_takes_a_ready_workspace_s_files_or_any_one_s_checkpoint_even_removed() {
    let f = Fixture::new().await;
    let source = |checkpoint| CopySource {
        workspace: SpaceSelector::id(&f.source.id),
        checkpoint,
    };
    let copying = |checkpoint| {
        Operation::Workspace(WorkspaceMethod::CopyOrigin {
            source: source(checkpoint),
        })
    };
    let checkpoint = f
        .manager
        .spaces
        .take_checkpoint(&*f.manager, &f.manager.named("topic").await, None)
        .await
        .unwrap()
        .number;
    let live = f.manager.spaces.copy_origin(&source(None)).await.unwrap();
    assert_eq!(live.workspace, f.source.id);
    assert_eq!(&live.repository, f.source.repository());
    f.manager
        .spaces
        .set_state(&f.source.id, WorkspaceState::Failed, None)
        .await
        .unwrap();
    let files = f
        .manager
        .spaces
        .check_target(
            &*f.manager,
            copying(None).target_check(),
            copying(None).target(),
            None,
        )
        .await;
    assert_eq!(Reason::of(&files.unwrap_err()), Some(Reason::NotReady));
    let kept = copying(Some(checkpoint));
    f.manager
        .spaces
        .check_target(&*f.manager, kept.target_check(), kept.target(), None)
        .await
        .unwrap();

    f.manager
        .spaces
        .remove_selected(
            &*f.manager,
            &SpaceSelector::id(&f.source.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let removed = f
        .manager
        .spaces
        .copy_origin(&source(Some(checkpoint)))
        .await;
    assert_eq!(removed.unwrap(), live);
    assert!(f.manager.spaces.copy_origin(&source(None)).await.is_err());
}
