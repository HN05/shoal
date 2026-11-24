//! Refreshing copies, which take their source's files again.
use super::*;
use crate::model::Workspace;
use crate::{
    daemon::workspace::{CopyPlace, NewWorkspace},
    protocol::{CopySource, CreationOptions},
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceSelector;

impl Fixture {
    /// A fixture whose checkout ignores `build/`, with the workspace
    /// `topic` to copy.
    async fn copied() -> (Self, Workspace) {
        let f = Self::new().await;
        fs::write(f.repo.path.join(".gitignore"), "build/\n").unwrap();
        fs::write(f.repo.path.join("tracked"), "tracked\n").unwrap();
        git(&f.repo.path, &["add", "."]);
        git(&f.repo.path, &["commit", "-q", "-m", "files"]);
        let source = f.workspace("topic", None).await;
        (f, source)
    }

    /// A copy of `source`'s files, or of its checkpoint `checkpoint`, on
    /// the branch `branch`.
    async fn copy(
        &self,
        source: &Workspace,
        branch: &str,
        checkpoint: Option<CheckpointNumber>,
        skip_setup: bool,
    ) -> Workspace {
        let copied = CopySource {
            workspace: SpaceSelector::id(&source.id),
            checkpoint,
        };
        let new = NewWorkspace {
            options: CreationOptions {
                skip_setup,
                ..Default::default()
            },
            ..Default::default()
        };
        let place = CopyPlace::Branch(Some(branch.into()));
        (self.manager)
            .spaces
            .create_copy(&*self.manager, (&self.repo.id).into(), &copied, place, new)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn a_copys_own_changes_and_commits_are_refused() {
    let (f, source) = Fixture::copied().await;
    let copy = f.copy(&source, "copy", None, false).await;
    add_commit(&source.path, "later");
    fs::write(copy.path.join("own"), "own\n").unwrap();

    let refused = f.refresh(&copy).await.unwrap_err().to_string();
    assert!(refused.contains("uncommitted changes to own"), "{refused}");
    assert!(refused.contains("shoal start --copy"), "{refused}");
    assert!(copy.path.join("own").exists());

    commit(&copy.path, "own");
    let refused = f.refresh(&copy).await.unwrap_err().to_string();
    assert!(
        refused.contains("1 commit(s) since it was copied"),
        "{refused}"
    );
}

#[tokio::test]
async fn a_copy_of_a_removed_workspace_has_nothing_to_refresh_from() {
    let (f, source) = Fixture::copied().await;
    let copy = f.copy(&source, "copy", None, false).await;
    (f.manager)
        .spaces
        .remove_selected(
            &*f.manager,
            &SpaceSelector::id(&source.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let refused = f.refresh(&copy).await.unwrap_err().to_string();
    assert!(refused.contains("source topic was removed"), "{refused}");
}

#[tokio::test]
async fn a_workspace_process_refreshes_a_copy_only_from_a_source_it_reaches() {
    let (f, source) = Fixture::copied().await;
    let copy = f.copy(&source, "copy", None, false).await;
    add_commit(&source.path, "later");
    let own = super::caller(&copy.id, "agent");

    let refused = (f.refresh_as(&copy, Some(&own)).await)
        .unwrap_err()
        .to_string();
    assert!(refused.contains("outside this one's scope"), "{refused}");
    assert!(!f.refresh(&copy).await.unwrap().up_to_date);
}

#[tokio::test]
async fn a_copy_keeps_its_own_worktree_config() {
    let (f, source) = Fixture::copied().await;
    git(
        &f.repo.path,
        &["config", "extensions.worktreeConfig", "true"],
    );
    let copy = f.copy(&source, "copy", None, false).await;
    git(&copy.path, &["config", "--worktree", "user.name", "Copy"]);
    add_commit(&source.path, "later");

    assert!(!f.refresh(&copy).await.unwrap().up_to_date);
    let name = git(&copy.path, &["config", "--worktree", "user.name"]);
    assert_eq!(name.trim(), "Copy");
}
