use crate::scope::Caller;
use crate::stand_in::Fixture;
use crate::{model::Workspace, workspace_files::Attacher};
use shoal_core::test_support::{commit, git};
use shoal_daemon_base::callers::Access;
use shoal_protocol::{absorb::Absorb, assignments::Done, space_names::SpaceSelector};
use std::{fs, path::Path};

/// A workspace `parent`, `worker` below it with a commit of its own, and
/// `leaf` below `worker`, built on its branch.
struct Tree {
    manager: Fixture,
    parent: Workspace,
    worker: Workspace,
}

async fn tree() -> Tree {
    let manager = Fixture::new().await;
    let repo = manager.repository("repo").await;
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let worker = child(&manager, &parent, "worker", None).await;
    child(&manager, &worker, "leaf", Some("worker")).await;
    fs::write(worker.path.join("feature"), "feature\n").unwrap();
    commit(&worker.path, "feature");
    Tree {
        manager,
        parent,
        worker,
    }
}

async fn child(manager: &Fixture, parent: &Workspace, name: &str, base: Option<&str>) -> Workspace {
    let parent = SpaceSelector::id(&parent.id);
    let base = base.map(str::to_owned);
    (manager
        .spaces
        .create_child(&**manager, &parent, name.into(), base, Default::default()))
    .await
    .unwrap()
}

fn absorb(space: &Workspace) -> Absorb {
    Absorb {
        space: SpaceSelector::id(&space.id),
        into: None,
        method: None,
        message: None,
        discard_uncommitted: false,
        force: false,
    }
}

async fn done(manager: &Fixture, space: &shoal_ids::space_ids::SpaceId) {
    let selector = SpaceSelector::id(space);
    (manager
        .spaces
        .mark_space_done(&**manager, &selector, Done::default(), &Attacher::User))
    .await
    .unwrap();
}

fn head(path: &Path) -> String {
    git(path, &["rev-parse", "HEAD"]).trim().to_owned()
}

async fn chat_in(manager: &Fixture, workspace: &Workspace, name: &str) -> crate::model::Chat {
    let workspace = SpaceSelector::id(&workspace.id);
    manager.chat_in(&workspace, name).await.unwrap()
}

#[tokio::test]
async fn a_hold_on_a_chat_in_the_absorbed_worktree_asks_a_scoped_caller_first() {
    let Tree {
        manager,
        parent,
        worker,
        ..
    } = tree().await;
    let in_worktree = manager
        .chat_in(&SpaceSelector::id(&worker.id), "reader")
        .await
        .unwrap();
    for space in [&worker.id, &in_worktree.id] {
        done(&manager, space).await;
    }
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&in_worktree.id),
            "reading".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let scoped = Caller {
        space_id: parent.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let before = head(&parent.path);

    let held = manager
        .spaces
        .absorb_space(&manager.daemon(), &absorb(&worker), Some(&scoped))
        .await;

    assert!(held.is_err());
    assert_eq!(head(&parent.path), before, "nothing merged");
    assert!(manager.spaces.workspace_by_id(&worker.id).await.is_ok());
}

#[tokio::test]
async fn a_chat_in_the_absorbed_worktree_goes_with_its_whole_removal_tree() {
    let Tree {
        manager, worker, ..
    } = tree().await;
    let in_worktree = chat_in(&manager, &worker, "reader").await;
    let parent = SpaceSelector::id(&in_worktree.id);
    let below = (manager.spaces.create_child(
        &*manager,
        &parent,
        "helper".into(),
        None,
        Default::default(),
    ))
    .await
    .unwrap();
    let nested = chat_in(&manager, &below, "nested").await;
    for space in [&worker.id, &in_worktree.id, &below.id, &nested.id] {
        done(&manager, space).await;
    }

    manager
        .spaces
        .absorb_space(&manager.daemon(), &absorb(&worker), None)
        .await
        .unwrap();

    drop(manager.gates.quiesce().await);
    for space in [&worker.id, &in_worktree.id, &below.id, &nested.id] {
        let left = manager.spaces.space(&SpaceSelector::id(space)).await;
        assert!(left.is_err(), "{space} stays");
    }
}
