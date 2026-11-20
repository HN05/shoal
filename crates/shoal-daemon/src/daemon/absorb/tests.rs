use std::{fs, path::Path, sync::Arc};

use crate::{
    daemon::{
        events::{EventItem, EventKind},
        manager::Manager,
        notifications::AlertKind,
        scope::Caller,
        test_support::{chat, manager, repository},
        workspace_files::Attacher,
    },
    forge::items::action::MergeMethod,
    model::{LinkedItem, Relation, Workspace},
    protocol::RemovalMethod,
    state::WorkspaceState,
    test_support::{commit, git},
};
use shoal_protocol::{
    absorb::Absorb,
    assignments::Done,
    files::NewFile,
    space_names::SpaceSelector,
    threads::{NewPost, ThreadKind, ThreadNumber, ThreadState},
};

const ISSUE: &str = "https://github.com/owner/repo/issues/7";

/// A workspace `parent`, `worker` below it with a commit of its own, and
/// `leaf` below `worker`, built on its branch.
struct Tree {
    _temp: tempfile::TempDir,
    manager: Arc<Manager>,
    parent: Workspace,
    worker: Workspace,
    leaf: Workspace,
}

async fn tree() -> Tree {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let worker = child(&manager, &parent, "worker", None).await;
    let leaf = child(&manager, &worker, "leaf", Some("worker")).await;
    fs::write(worker.path.join("feature"), "feature\n").unwrap();
    commit(&worker.path, "feature");
    Tree {
        _temp: temp,
        manager,
        parent,
        worker,
        leaf,
    }
}

async fn child(manager: &Manager, parent: &Workspace, name: &str, base: Option<&str>) -> Workspace {
    let parent = SpaceSelector::id(&parent.id);
    let base = base.map(str::to_owned);
    (manager.create_child(&parent, name.into(), base, Default::default()))
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

async fn done(manager: &Manager, space: &shoal_ids::space_ids::SpaceId) {
    let done = Done {
        cleanup: false,
        integrated: false,
        keep_demos: false,
        results: Vec::new(),
        summary: None,
    };
    manager
        .mark_space_done(&SpaceSelector::id(space), done, &Attacher::User)
        .await
        .unwrap();
}

fn head(path: &Path) -> String {
    git(path, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// The kinds of the events the space `id` recorded, oldest first.
async fn event_kinds(manager: &Manager, id: &shoal_ids::space_ids::SpaceId) -> Vec<EventKind> {
    let events = manager.space_events(None, 500).await.unwrap();
    (events.iter())
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.space_id == *id => Some(event.details.kind),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_done_workspace_folds_into_its_parent_which_takes_its_work_and_records() {
    let Tree {
        _temp,
        manager,
        parent,
        worker,
        leaf,
    } = tree().await;
    fs::write(worker.path.join("notes"), "unsaved notes\n").unwrap();
    fs::write(parent.path.join("draft"), "the parent's own draft\n").unwrap();
    let related = LinkedItem {
        url: ISSUE.parse().unwrap(),
        title: Some("Related issue".into()),
    };
    (manager.record_items(&worker.id, vec![(Relation::Related, related)]))
        .await
        .unwrap();
    let report = worker.path.join("report.md");
    fs::write(&report, "findings\n").unwrap();
    let attached = NewFile::from(report.clone());
    (manager.add_attached_files(
        &SpaceSelector::id(&worker.id),
        vec![attached],
        &Attacher::User,
    ))
    .await
    .unwrap();
    fs::remove_file(&report).unwrap();
    done(&manager, &worker.id).await;
    let worker_head = head(&worker.path);

    let absorbed = manager.absorb_space(&absorb(&worker), None).await.unwrap();

    assert_eq!(absorbed.into.id, parent.id);
    let merge = absorbed.merge.unwrap();
    assert_eq!(merge.method, MergeMethod::Merge);
    assert_eq!(merge.commits.len(), 1, "{:?}", merge.commits);
    assert_eq!(head(&parent.path), worker_head, "a fast-forward");
    assert_eq!(absorbed.carried, ["notes"]);
    assert_eq!(
        git(&parent.path, &["status", "--porcelain"]),
        "?? draft\n?? notes\n"
    );
    assert_eq!(absorbed.files, ["report.md"]);
    let files = manager
        .attached_files(&SpaceSelector::id(&parent.id))
        .await
        .unwrap();
    assert_eq!(files.len(), 1);
    let id = parent.id.clone();
    let links = (manager.store)
        .run(move |db| Ok(crate::daemon::links::linked::space_links(db, &id)?))
        .await
        .unwrap();
    assert_eq!(links.related.len(), 1);
    assert_eq!(absorbed.children.len(), 1);
    let leaf = manager.workspace_by_id(&leaf.id).await.unwrap();
    assert_eq!(
        leaf.parent.as_ref().map(|above| &above.id),
        Some(&parent.id)
    );
    let base = leaf.base_workspace.as_ref().map(|base| &base.id);
    assert_eq!(
        base,
        Some(&parent.id),
        "built on the branch that took the work"
    );

    drop(manager.gates.quiesce().await);
    assert!(manager.workspace_by_id(&worker.id).await.is_err());
    let worker_events = event_kinds(&manager, &worker.id).await;
    assert!(
        worker_events.contains(&EventKind::Absorbed),
        "{worker_events:?}"
    );
    assert!(
        worker_events.contains(&EventKind::Removed),
        "{worker_events:?}"
    );
    let parent_events = event_kinds(&manager, &parent.id).await;
    assert!(
        parent_events.contains(&EventKind::AbsorbedSpace),
        "{parent_events:?}"
    );
}

#[tokio::test]
async fn a_forced_squash_takes_the_open_threads_of_work_that_is_not_done() {
    let Tree {
        _temp,
        manager,
        parent,
        worker,
        ..
    } = tree().await;
    let screenshot = worker.path.join("choices.png");
    fs::write(&screenshot, "png").unwrap();
    let caller = crate::daemon::test_support::caller_in(&SpaceSelector::id(&worker.id));
    let thread = manager
        .open_thread(
            Some(&caller),
            "Which database?".into(),
            ThreadKind::Question,
            None,
        )
        .await
        .unwrap();
    let post = NewPost {
        text: "Which database?".into(),
        ..NewPost::default()
    };
    let attached = vec![NewFile::from(screenshot)];
    let number = ThreadNumber::Full(thread.number);
    (manager.post_to_thread(Some(&caller), number, post, attached))
        .await
        .unwrap();
    let parent_head = head(&parent.path);

    let refused = manager.absorb_space(&absorb(&worker), None).await;
    assert!(format!("{:#}", refused.unwrap_err()).contains("not done"));
    assert_eq!(head(&parent.path), parent_head);

    let request = Absorb {
        method: Some(MergeMethod::Squash),
        force: true,
        ..absorb(&worker)
    };
    let absorbed = manager.absorb_space(&request, None).await.unwrap();

    assert_eq!(absorbed.threads.len(), 1);
    let listing = manager
        .threads(Some(&SpaceSelector::id(&parent.id)), ThreadState::Open)
        .await
        .unwrap();
    assert_eq!(listing.threads.len(), 1);
    let files = manager
        .attached_files(&SpaceSelector::id(&parent.id))
        .await
        .unwrap();
    // Its holder hears of its new number.
    let notifications = manager.notifications.list(true, 50).await.unwrap();
    let moved: Vec<_> = (notifications.iter())
        .filter(|notification| notification.kind == AlertKind::ThreadResolved)
        .collect();
    assert_eq!(moved.len(), 1, "{moved:?}");
    let number = listing.threads[0].number.to_string();
    assert!(moved[0].message.contains(&number), "{}", moved[0].message);
    let sent: Vec<_> = listing.threads[0].posts[0]
        .files
        .iter()
        .map(|file| file.id)
        .collect();
    assert_eq!(sent, files.iter().map(|file| file.id).collect::<Vec<_>>());
    let parents = git(&parent.path, &["show", "-s", "--format=%P %s", "HEAD"]);
    assert_eq!(
        parents.trim(),
        format!("{parent_head} Absorb worker from branch 'worker'")
    );
}

#[tokio::test]
async fn refusals_fire_before_anything_changes() {
    let Tree {
        _temp,
        manager,
        parent,
        worker,
        leaf,
    } = tree().await;
    done(&manager, &worker.id).await;
    let into = |space: &Workspace| Absorb {
        into: Some(SpaceSelector::id(&space.id)),
        ..absorb(&worker)
    };
    let below = manager.absorb_space(&into(&leaf), None).await.unwrap_err();
    assert!(format!("{below:#}").contains("--into"), "{below:#}");

    // Another lifecycle operation under way in the target.
    let state = (manager.reserve_lifecycle(&worker.id, WorkspaceState::Stopping))
        .await
        .unwrap();
    let busy = manager.absorb_space(&absorb(&worker), None).await;
    assert!(busy.is_err());
    (manager.set_state(&worker.id, state, None)).await.unwrap();

    // The parent's own uncommitted edit to the file the merge brings.
    fs::write(parent.path.join("feature"), "the parent's own\n").unwrap();
    let before = (head(&parent.path), head(&worker.path));
    let touched = manager
        .absorb_space(&absorb(&worker), None)
        .await
        .unwrap_err();
    assert!(format!("{touched:#}").contains("feature"), "{touched:#}");
    assert_eq!((head(&parent.path), head(&worker.path)), before);
    let leaf = manager.workspace_by_id(&leaf.id).await.unwrap();
    assert_eq!(leaf.parent.map(|above| above.id), Some(worker.id.clone()));

    let repo = manager.repository(worker.repository()).await.unwrap();
    let chat = chat(&manager, "coordinator", "chat", Some(&repo), None).await;
    let code = Absorb {
        into: Some(SpaceSelector::id(&chat.id)),
        ..absorb(&worker)
    };
    let refused = manager.absorb_space(&code, None).await.unwrap_err();
    assert!(
        format!("{refused:#}").contains("shoal convert workspace"),
        "{refused:#}"
    );
    let discarded = Absorb {
        force: true,
        ..code
    };
    let absorbed = manager.absorb_space(&discarded, None).await.unwrap();
    assert!(absorbed.discarded && absorbed.merge.is_none());
}

#[tokio::test]
async fn a_scoped_caller_absorbs_only_the_spaces_below_its_own() {
    let Tree {
        _temp,
        manager,
        parent,
        worker,
        leaf,
    } = tree().await;
    let scoped = Caller {
        space_id: worker.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    (manager.executions)
        .issue_scope("scope".into(), scoped)
        .await;
    for (target, allowed) in [(&leaf, true), (&worker, false), (&parent, false)] {
        let mut method = RemovalMethod::AbsorbSpace(Box::new(absorb(target)));
        let authorized =
            crate::daemon::scope::authorize(&manager, Some(&"scope".into()), &mut method).await;
        assert_eq!(authorized.is_ok(), allowed, "{}", target.name);
    }
}

#[tokio::test]
async fn a_child_the_taking_branch_builds_on_goes_onto_what_the_absorbed_one_built_on() {
    let Tree {
        _temp,
        manager,
        parent,
        worker,
        leaf,
    } = tree().await;
    // The workspace taking the work builds on the absorbed one's child.
    let taker = child(&manager, &parent, "taker", Some("leaf")).await;
    done(&manager, &worker.id).await;
    let request = Absorb {
        into: Some(SpaceSelector::id(&taker.id)),
        ..absorb(&worker)
    };

    manager.absorb_space(&request, None).await.unwrap();

    let leaf = manager.workspace_by_id(&leaf.id).await.unwrap();
    assert_eq!(leaf.base_workspace, None, "onto the default branch");
    // The taker builds on it and holds the absorbed work, which it keeps.
    let told = manager
        .agent_messages(&SpaceSelector::id(&leaf.id))
        .await
        .unwrap();
    assert!(!told.is_empty());
    let rebases = told
        .iter()
        .any(|message| message.message.contains("git rebase"));
    assert!(!rebases, "{told:?}");
    let taker = manager.workspace_by_id(&taker.id).await.unwrap();
    let base = taker.base_workspace.as_ref().map(|base| &base.id);
    assert_eq!(base, Some(&leaf.id));
}

#[tokio::test]
async fn a_workspace_built_on_the_absorbed_branch_elsewhere_follows_its_work() {
    let Tree {
        _temp,
        manager,
        parent,
        worker,
        ..
    } = tree().await;
    // A sibling, not below it, built on its branch.
    let sibling = child(&manager, &parent, "sibling", Some("worker")).await;
    done(&manager, &worker.id).await;

    manager.absorb_space(&absorb(&worker), None).await.unwrap();

    let sibling = manager.workspace_by_id(&sibling.id).await.unwrap();
    let base = sibling.base_workspace.as_ref().map(|base| &base.id);
    assert_eq!(base, Some(&parent.id));
    assert_eq!(sibling.parent.map(|above| above.id), Some(parent.id));
}

#[tokio::test]
async fn an_absorber_built_on_the_absorbed_branch_takes_its_base_instead() {
    let Tree {
        _temp,
        manager,
        parent,
        worker,
        ..
    } = tree().await;
    let taker = child(&manager, &parent, "taker", Some("worker")).await;
    done(&manager, &worker.id).await;
    let request = Absorb {
        into: Some(SpaceSelector::id(&taker.id)),
        ..absorb(&worker)
    };

    manager.absorb_space(&request, None).await.unwrap();

    let taker = manager.workspace_by_id(&taker.id).await.unwrap();
    assert_eq!(taker.base_workspace, None, "never its own base");
    // Its branch has the absorbed work, which a rebase past it would drop.
    let told = manager
        .agent_messages(&SpaceSelector::id(&taker.id))
        .await
        .unwrap();
    assert!(!told.is_empty());
    let rebases = told
        .iter()
        .any(|message| message.message.contains("git rebase"));
    assert!(!rebases, "{told:?}");
}

#[tokio::test]
async fn a_forced_absorb_leaves_a_detached_workspaces_commits_to_its_removal() {
    let Tree {
        _temp,
        manager,
        parent,
        ..
    } = tree().await;
    let detached = manager
        .create_commit_workspace(
            parent.repository().into(),
            (Some(parent.id.clone()), None),
            shoal_protocol::protocol::CommitStart::Revision(None),
            Default::default(),
        )
        .await
        .unwrap();
    fs::write(detached.path.join("probe"), "probe\n").unwrap();
    commit(&detached.path, "probe");
    let repo = manager.repository(parent.repository()).await.unwrap();
    let chat = chat(&manager, "coordinator", "chat", Some(&repo), None).await;
    let request = Absorb {
        into: Some(SpaceSelector::id(&chat.id)),
        force: true,
        ..absorb(&detached)
    };

    let absorbed = manager.absorb_space(&request, None).await.unwrap();

    assert!(absorbed.discarded);
    drop(manager.gates.quiesce().await);
    assert!(manager.workspace_by_id(&detached.id).await.is_err());
}

#[tokio::test]
async fn the_chats_in_the_absorbed_worktree_go_with_the_spaces_below_them() {
    let Tree {
        _temp,
        manager,
        worker,
        ..
    } = tree().await;
    let in_worktree = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&worker.id),
            "reader".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let repo = manager.repository(worker.repository()).await.unwrap();
    let below = chat(
        &manager,
        "helper",
        "chat",
        Some(&repo),
        Some(&in_worktree.id),
    )
    .await;
    for space in [&worker.id, &in_worktree.id, &below.id] {
        done(&manager, space).await;
    }

    manager.absorb_space(&absorb(&worker), None).await.unwrap();

    drop(manager.gates.quiesce().await);
    for space in [&worker.id, &in_worktree.id, &below.id] {
        let left = manager.space(&SpaceSelector::id(space)).await;
        assert!(left.is_err(), "{space} stays");
    }
}

#[tokio::test]
async fn a_hold_on_a_chat_in_the_absorbed_worktree_asks_a_scoped_caller_first() {
    let Tree {
        _temp,
        manager,
        parent,
        worker,
        ..
    } = tree().await;
    let in_worktree = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&worker.id),
            "reader".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    for space in [&worker.id, &in_worktree.id] {
        done(&manager, space).await;
    }
    manager
        .acquire_hold(
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
        read_only: false,
        vouched: None,
    };
    let before = head(&parent.path);

    let held = manager.absorb_space(&absorb(&worker), Some(&scoped)).await;

    assert!(held.is_err());
    assert_eq!(head(&parent.path), before, "nothing merged");
    assert!(manager.workspace_by_id(&worker.id).await.is_ok());
}

#[tokio::test]
async fn a_space_below_a_chat_in_the_absorbed_worktree_cannot_take_the_work() {
    let Tree {
        _temp,
        manager,
        parent,
        worker,
        ..
    } = tree().await;
    let in_worktree = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&worker.id),
            "reader".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let repo = manager.repository(worker.repository()).await.unwrap();
    let below = chat(
        &manager,
        "helper",
        "chat",
        Some(&repo),
        Some(&in_worktree.id),
    )
    .await;
    done(&manager, &worker.id).await;
    let request = Absorb {
        into: Some(SpaceSelector::id(&below.id)),
        force: true,
        ..absorb(&worker)
    };
    let before = head(&parent.path);

    let refused = manager.absorb_space(&request, None).await.unwrap_err();

    assert!(format!("{refused:#}").contains("--into"), "{refused:#}");
    assert!(manager.workspace_by_id(&worker.id).await.is_ok());
    assert_eq!(head(&parent.path), before);
}

#[tokio::test]
async fn a_chat_in_the_absorbed_worktree_goes_with_its_whole_removal_tree() {
    let Tree {
        _temp,
        manager,
        worker,
        ..
    } = tree().await;
    let in_worktree = chat_in(&manager, &worker, "reader").await;
    let parent = SpaceSelector::id(&in_worktree.id);
    let below = (manager.create_child(&parent, "helper".into(), None, Default::default()))
        .await
        .unwrap();
    let nested = chat_in(&manager, &below, "nested").await;
    for space in [&worker.id, &in_worktree.id, &below.id, &nested.id] {
        done(&manager, space).await;
    }

    manager.absorb_space(&absorb(&worker), None).await.unwrap();

    drop(manager.gates.quiesce().await);
    for space in [&worker.id, &in_worktree.id, &below.id, &nested.id] {
        let left = manager.space(&SpaceSelector::id(space)).await;
        assert!(left.is_err(), "{space} stays");
    }
}

/// A new chat `name` in the worktree of `workspace`.
async fn chat_in(manager: &Arc<Manager>, workspace: &Workspace, name: &str) -> crate::model::Chat {
    let workspace = SpaceSelector::id(&workspace.id);
    (manager.start_chat_in_workspace(&workspace, name.into(), "claude".into(), None))
        .await
        .unwrap()
}
