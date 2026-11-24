use crate::cleanup::merged::*;
use crate::execution::recovery;
use crate::model::Workspace;
use crate::notifications::AlertKind;
use crate::paths::StateFiles;
use crate::stand_in;
use crate::time::UnixTime;
use shoal_core::test_support::{commit, git, repository};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const HOUR: Duration = Duration::from_secs(3600);

/// Local repository config that turns merged cleanup on.
const ENABLED: &str = "[cleanup.merged]\nenabled = true\n";

struct Fixture {
    manager: stand_in::Fixture,
    checkout: PathBuf,
    workspace: Workspace,
}

impl Fixture {
    /// A workspace with two commits and a linked issue, which keeps it from
    /// idle cleanup, in a repository with merged cleanup on.
    async fn new() -> Self {
        let manager = stand_in::Fixture::new().await;
        let checkout = repository(manager.root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        (manager.write_local_config(&repo.id, Some(ENABLED.into())))
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "topic".into(), None, None, None)
            .await
            .unwrap();
        for file in ["one", "two"] {
            fs::write(workspace.path.join(file), format!("{file}\n")).unwrap();
            commit(&workspace.path, file);
        }
        let id = workspace.id.clone();
        (manager.store.run(move |db| {
            db.execute(
                "INSERT INTO space_issue(space_id,url) VALUES (?1,'http://127.0.0.1:1/team/repo/issues/1')",
                [id],
            )?;
            Ok(())
        }))
        .await
        .unwrap();
        Self {
            manager,
            checkout,
            workspace,
        }
    }

    /// Squash the branch onto the default branch, as a forge merge would.
    fn squash_merge(&self) {
        git(&self.checkout, &["merge", "-q", "--squash", "topic"]);
        git(&self.checkout, &["commit", "-q", "-m", "squash"]);
    }

    /// Run a pass `hours` from now and report whether the workspace remains.
    async fn kept_after(&self, hours: u32) -> bool {
        let mut state = State::default();
        sweep_at(
            &*self.manager,
            &mut state,
            UnixTime::now() + hours * HOUR,
            &Default::default(),
        )
        .await
        .unwrap();
        self.manager
            .spaces
            .list_workspaces()
            .await
            .unwrap()
            .iter()
            .any(|workspace| workspace.id == self.workspace.id)
    }
}

#[tokio::test]
async fn merged_workspace_is_removed_once_idle_whatever_its_assignment() {
    let f = Fixture::new().await;
    assert!(f.kept_after(24).await, "unmerged work stays");
    assert!(
        f.manager
            .spaces
            .cleanup_candidacy(&*f.manager, &f.workspace.id)
            .await
            .unwrap()
            .is_err(),
        "idle cleanup keeps a workspace with a linked issue"
    );
    f.squash_merge();
    // Files changed just now, and the default idle period is four hours.
    assert!(f.kept_after(3).await);
    assert!(!f.kept_after(5).await);
    assert!(!f.workspace.path.exists());
    assert_eq!(
        git(&f.checkout, &["branch", "--list", "topic"]),
        "",
        "the merged branch is redundant"
    );
    let notifications = f
        .manager
        .spaces
        .notifications
        .list(true, 100)
        .await
        .unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == AlertKind::SpaceRemoved)
    );
}

#[tokio::test]
async fn saved_agents_do_not_hide_active_commands_from_merged_cleanup() {
    let f = Fixture::new().await;
    f.squash_merge();
    let path = recovery::record_path(
        &f.manager.paths,
        &f.workspace.id,
        &ExecutionId::from("saved"),
    );
    fs::create_dir_all(f.manager.paths.space_state(&f.workspace.id)).unwrap();
    fs::write(
        path,
        r#"{"agent":"claude","mode":"worker","stage":"stopped"}"#,
    )
    .unwrap();
    let id = f.workspace.id.clone();
    f.manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state) VALUES ('unknown',?1,'unknown')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    assert!(
        f.manager
            .spaces
            .merged_snapshot(&*f.manager, &f.workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.manager.spaces.cleanup_at(&f.workspace.id).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn holds_waiting_threads_running_work_and_unsaved_state_retain_a_merged_workspace() {
    let f = Fixture::new().await;
    f.squash_merge();
    let id = &f.workspace.id;
    let manager = &f.manager;

    manager
        .spaces
        .acquire_hold(
            &**manager,
            &SpaceSelector::id(id),
            "review".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(id),
            "review".into(),
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();

    let thread = manager.ask(id, "Anything else?").await;
    assert!(f.kept_after(5).await, "a thread that waits");
    manager.answer(thread.number, "No").await;

    let repo_path = f.checkout.to_str().unwrap().to_owned();
    let stacked = manager
        .create_workspace(repo_path.as_str(), "stacked".into(), None, None, None)
        .await
        .unwrap();
    manager
        .spaces
        .acquire_hold(
            &**manager,
            &SpaceSelector::id(&stacked.id),
            "busy".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    manager
        .spaces
        .set_base_workspace(&SpaceSelector::id(&stacked.id), Some("topic".into()))
        .await
        .unwrap();
    assert!(f.kept_after(5).await, "a base of another workspace");
    manager
        .spaces
        .set_base_workspace(&SpaceSelector::id(&stacked.id), None)
        .await
        .unwrap();

    let git_dir = f.workspace.checkout.git_dir().unwrap().to_owned();
    fs::write(git_dir.join("MERGE_HEAD"), "").unwrap();
    assert!(f.kept_after(5).await, "a merge under way");
    fs::remove_file(git_dir.join("MERGE_HEAD")).unwrap();

    let nested = repository(&f.workspace.path, "nested");
    assert!(
        f.kept_after(5).await,
        "unsaved files no checkpoint can keep"
    );
    fs::remove_dir_all(nested).unwrap();

    let repo = f.workspace.repository().clone();

    manager
        .write_local_config(&repo, Some("[cleanup.merged]\nenabled = false\n".into()))
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    manager
        .write_local_config(&repo, Some(format!("{ENABLED}idle_minutes = 600\n")))
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    assert!(!f.kept_after(11).await);
}
