use crate::daemon::manager::Manager;
use crate::daemon::scope::Caller;
use crate::model::Space;
use crate::model::Workspace;
use crate::state::WorkspaceState;
use std::{fs, sync::Arc};

use crate::daemon::test_support::{git, manager, repository};
use shoal_protocol::space_names::SpaceSelector;

struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    checkout: std::path::PathBuf,
    workspace: Workspace,
}

impl Fixture {
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .repositories
            .register(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "topic".into(), None, None, None)
            .await
            .unwrap();
        Self {
            _root: root,
            manager,
            checkout,
            workspace,
        }
    }

    fn write(&self, file: &str, contents: &str) {
        fs::write(self.workspace.path.join(file), contents).unwrap();
    }

    fn git(&self, args: &[&str]) -> String {
        git(&self.workspace.path, args)
    }
}

#[tokio::test]
async fn an_agent_exit_leaves_a_checkpoint_only_once_its_processes_are_proven_stopped() {
    let f = Fixture::new().await;
    f.write("scratch", "notes\n");
    let count = async || {
        f.manager
            .spaces
            .checkpoints(&f.manager.named("topic").await, None)
            .await
            .unwrap()
            .unwrap()
            .len()
    };
    f.manager
        .agents
        .post_agent_exit(
            &*f.manager,
            &Space::from(f.workspace.clone()),
            &"claude".into(),
            Some(0),
            false,
        )
        .await;
    assert_eq!(count().await, 0);
    f.manager
        .agents
        .post_agent_exit(
            &*f.manager,
            &Space::from(f.workspace.clone()),
            &"claude".into(),
            Some(0),
            true,
        )
        .await;
    assert_eq!(count().await, 1);
}

#[tokio::test]
async fn links_cannot_change_while_a_checkpoint_is_restored() {
    use crate::{forge::items::link::LinkKind, model::Relation};
    let f = Fixture::new().await;
    git(
        &f.checkout,
        &["remote", "add", "origin", "https://github.com/acme/repo"],
    );
    f.manager
        .spaces
        .reserve_lifecycle(&f.workspace.id, WorkspaceState::Restoring)
        .await
        .unwrap();
    f.manager
        .links
        .link_item(
            &*f.manager,
            &SpaceSelector::id(&f.workspace.id),
            LinkKind::Pr,
            &"5".parse().unwrap(),
            Relation::Related,
            None,
            None,
        )
        .await
        .unwrap_err();
}

#[tokio::test]
async fn restore_refuses_what_it_would_lose_or_disturb() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    let checkpoint = manager
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    let refused = async |own: Option<&Caller>| {
        manager
            .spaces
            .restore_checkpoint(
                &**manager,
                &manager.named("topic").await,
                checkpoint.number,
                own,
            )
            .await
            .unwrap_err();
        let workspace = manager
            .spaces
            .workspace(&manager.named("topic").await)
            .await
            .unwrap();
        assert_eq!(workspace.state, WorkspaceState::Ready);
    };

    // Another execution runs; the caller's own does not count.
    let id = f.workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state) VALUES ('agent',?1,'running')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    refused(None).await;
    let own = Caller {
        space_id: f.workspace.id.clone(),
        execution: Some(shoal_coordination::scope::ScopedExecution {
            id: "agent".into(),
            kind: crate::daemon::executions::ExecutionKind::Command,
            agent_execution: None,
        }),
        granted: None,
        read_only: false,
        vouched: None,
    };
    manager
        .spaces
        .restore_checkpoint(
            &**manager,
            &manager.named("topic").await,
            checkpoint.number,
            Some(&own),
        )
        .await
        .unwrap();

    // Unsaved files no checkpoint can keep.
    repository(&f.workspace.path, "nested");
    refused(Some(&own)).await;
    fs::remove_dir_all(f.workspace.path.join("nested")).unwrap();

    // A Git operation under way.
    let git_dir = f.git(&["rev-parse", "--absolute-git-dir"]);
    let merge_head = std::path::Path::new(git_dir.trim()).join("MERGE_HEAD");
    fs::write(&merge_head, format!("{}\n", checkpoint.head)).unwrap();
    refused(Some(&own)).await;
    fs::remove_file(merge_head).unwrap();

    // Another branch.
    f.git(&["switch", "-q", "-c", "other"]);
    refused(Some(&own)).await;
}
