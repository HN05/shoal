use crate::model::Workspace;
use crate::stand_in;
use crate::{
    protocol::{CopySource, CreationOptions},
    workspace::{CopyPlace, NewWorkspace},
};
use anyhow::Result;
use shoal_core::test_support::{commit, git};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceSelector;
use std::{fs, path::Path};

const STATUS: &[&str] = &["status", "--porcelain", "--untracked-files=all"];

struct Fixture {
    manager: stand_in::Fixture,
    repo: crate::model::Repository,
}

impl Fixture {
    /// A fixture whose checkout ignores `build/`, with the workspace
    /// `topic` to copy.
    async fn copied() -> (Self, Workspace) {
        let manager = stand_in::Fixture::new().await;
        let repo = manager.repository("repo").await;
        fs::write(repo.path.join(".gitignore"), "build/\n").unwrap();
        fs::write(repo.path.join("tracked"), "tracked\n").unwrap();
        git(&repo.path, &["add", "."]);
        git(&repo.path, &["commit", "-q", "-m", "files"]);
        let source = (manager.create_workspace(&repo.id, "topic".into(), None, None, None))
            .await
            .unwrap();
        (Self { manager, repo }, source)
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

    async fn refresh(&self, workspace: &Workspace) -> Result<crate::model::Refreshed> {
        (self.manager)
            .spaces
            .refresh_workspace(&*self.manager, &SpaceSelector::id(&workspace.id), None)
            .await
    }
}

fn add_commit(path: &Path, name: &str) -> String {
    fs::write(path.join(name), format!("{name}\n")).unwrap();
    commit(path, name);
    git(path, &["rev-parse", "HEAD"]).trim().to_owned()
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
    let now = refreshed.workspace.part.copied_from.unwrap();
    assert_ne!(now.starting_checkpoint, started.starting_checkpoint);
    assert_eq!(now.head, committed);
    // A checkpoint still keeps what it had, which restore returns to.
    let checkpoints = (f
        .manager
        .spaces
        .checkpoints(&SpaceSelector::id(&copy.id), None))
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
async fn a_copy_of_a_checkpoint_is_up_to_date() {
    let (f, source) = Fixture::copied().await;
    let checkpoint =
        (f.manager
            .spaces
            .take_checkpoint(&*f.manager, &SpaceSelector::id(&source.id), None))
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
