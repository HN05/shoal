//! Refreshing copies, which take their source's files again.
use super::*;
use crate::{
    daemon::workspace::{CopyPlace, NewWorkspace},
    model::CheckpointNumber,
    protocol::{CopySource, CreationOptions},
    removal::{InspectionPolicy, UnsavedWork},
};

const STATUS: &[&str] = &["status", "--porcelain", "--untracked-files=all"];

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
            .create_copy((&self.repo.id).into(), &copied, place, new)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn a_copy_takes_its_sources_files_again_and_keeps_what_it_had() {
    let (f, source) = Fixture::copied().await;
    let copy = f.copy(&source, "copy", None, false).await;
    let unset = f.copy(&source, "unset", None, true).await;
    let started = copy.copied_from.clone().unwrap();
    assert!(f.refresh(&copy).await.unwrap().up_to_date);

    let path = &source.path;
    let committed = add_commit(path, "committed");
    fs::write(path.join("staged"), "staged\n").unwrap();
    git(path, &["add", "staged"]);
    fs::write(path.join("tracked"), "unstaged\n").unwrap();
    fs::write(path.join("untracked"), "untracked\n").unwrap();
    fs::create_dir_all(path.join("build")).unwrap();
    fs::write(path.join("build/output"), "ignored\n").unwrap();

    let refreshed = f.refresh(&copy).await.unwrap();
    assert_eq!(
        (
            refreshed.target.as_str(),
            refreshed.up_to_date,
            refreshed.setup
        ),
        ("workspace topic", false, true)
    );
    assert_eq!(refreshed.to, committed);
    assert_eq!(git(&copy.path, STATUS), git(path, STATUS));
    assert_eq!(
        git(&copy.path, &["diff", "--cached"]),
        git(path, &["diff", "--cached"])
    );
    let output = fs::read_to_string(copy.path.join("build/output")).unwrap();
    assert_eq!(output, "ignored\n");
    assert_eq!(git(&copy.path, &["rev-parse", "copy"]).trim(), committed);
    let now = refreshed.workspace.copied_from.unwrap();
    assert_ne!(now.starting_checkpoint, started.starting_checkpoint);
    assert_eq!(now.head, committed);
    // A checkpoint still keeps what it had, which restore returns to.
    let checkpoints = (f.manager.checkpoints(&SpaceSelector::id(&copy.id), None))
        .await
        .unwrap()
        .unwrap();
    assert!(checkpoints.iter().any(|kept| kept.head == started.head));
    assert!(f.refresh(&copy).await.unwrap().up_to_date);

    let without_setup = f.refresh(&unset).await.unwrap();
    assert_eq!(
        (without_setup.up_to_date, without_setup.setup),
        (false, false)
    );
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
        .remove_workspace(
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
async fn a_copy_of_a_checkpoint_is_up_to_date() {
    let (f, source) = Fixture::copied().await;
    let checkpoint = (f
        .manager
        .take_checkpoint(&SpaceSelector::id(&source.id), None))
    .await
    .unwrap();
    let copy = f
        .copy(&source, "copy", Some(checkpoint.number), false)
        .await;
    add_commit(&source.path, "later");

    let refreshed = f.refresh(&copy).await.unwrap();
    assert!(refreshed.up_to_date);
    assert_eq!(
        refreshed.target,
        format!("checkpoint {} of topic", checkpoint.number)
    );
}
