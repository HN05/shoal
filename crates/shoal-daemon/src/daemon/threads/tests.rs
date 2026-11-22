use std::sync::Arc;

use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::{
    space_names::{SpaceName, SpaceSelector},
    threads::{
        CloseCause, NewPost, Outcome, OutcomeKind, Resolving, Sender, Side, Thread, ThreadKind,
        ThreadNumber, ThreadSpace,
    },
};

use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        notifications::AlertKind,
        scope::{Caller, ScopedExecution},
        test_support::{away_on_offline_machine, caller_in, manager, repository, track_agent},
        workspace_files::Attacher,
    },
    model::Workspace,
    protocol::TurnEnd,
};
use shoal_protocol::assignments::Done;

/// A workspace `parent` whose agent runs, two workspaces below it whose
/// agents run, and a workspace beside it without one.
pub(super) struct Tree {
    _root: tempfile::TempDir,
    pub(super) manager: Arc<Manager>,
    pub(super) parent: Workspace,
    pub(super) child: Workspace,
    sibling: Workspace,
    pub(super) beside: Workspace,
    /// The execution the parent's agent runs as.
    agent: ExecutionId,
}

pub(super) async fn tree() -> Tree {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let below = |name: &str| {
        let name = name.to_owned();
        let manager = manager.clone();
        let parent = SpaceSelector::id(&parent.id);
        async move {
            manager
                .create_child(&parent, name, None, Default::default())
                .await
                .unwrap()
        }
    };
    let (child, sibling) = (below("child").await, below("sibling").await);
    let beside = manager
        .create_workspace(&repo.id, "beside".into(), None, None, None)
        .await
        .unwrap();
    let agent = track_agent(&manager, &parent).await;
    // A space's agent asks its coordinator; without one, whoever acted
    // there asks the user.
    for below in [&child, &sibling] {
        track_agent(&manager, below).await;
    }
    Tree {
        _root: root,
        manager,
        parent,
        child,
        sibling,
        beside,
        agent,
    }
}

pub(super) fn caller(workspace: &Workspace) -> Caller {
    caller_in(&SpaceSelector::id(&workspace.id))
}

pub(super) fn post(text: &str) -> NewPost {
    NewPost {
        text: text.into(),
        ..NewPost::default()
    }
}

pub(super) fn full(thread: &Thread) -> ThreadNumber {
    ThreadNumber::Full(thread.number)
}

pub(super) async fn open(
    manager: &Manager,
    from: Option<&Caller>,
    to: Option<&Workspace>,
) -> Thread {
    let to = to.map(|workspace| SpaceSelector::id(&workspace.id));
    manager
        .open_thread(
            from,
            "Which database?".into(),
            ThreadKind::Question,
            to.as_ref(),
            Vec::new(),
        )
        .await
        .unwrap()
}

async fn send(manager: &Manager, from: Option<&Caller>, thread: &Thread, text: &str) -> Thread {
    manager
        .post_to_thread(
            from,
            full(thread),
            post(text),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .await
        .unwrap()
}

/// The unread alerts of `workspace`'s agent about threads.
pub(super) async fn alerts(manager: &Manager, workspace: &Workspace) -> Vec<String> {
    let unread = manager
        .agents
        .agent_messages(manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    (unread.into_iter())
        .filter(|alert| alert.message.contains("hread"))
        .map(|alert| alert.message)
        .collect()
}

/// The user's unread alerts of `kind`.
async fn users(manager: &Manager, kind: AlertKind) -> Vec<String> {
    let unread = manager.notifications.list(true, 50).await.unwrap();
    (unread.into_iter())
        .filter(|alert| alert.kind == kind)
        .map(|alert| alert.message)
        .collect()
}

#[tokio::test]
async fn an_empty_thread_reaches_its_holder_with_its_first_post() {
    let tree = tree().await;
    let (manager, child) = (&tree.manager, caller(&tree.child));
    let thread = open(manager, Some(&child), None).await;
    // Its coordinator holds it; until a post nobody hears of it, and it
    // keeps nothing.
    assert_eq!(
        thread.holder.space().map(|holder| &holder.id),
        Some(&tree.parent.id)
    );
    assert_eq!(thread.number.space, tree.child.number);
    assert!(alerts(manager, &tree.parent).await.is_empty());
    assert!(!manager.has_waiting_threads(&tree.child.id).await.unwrap());

    send(manager, Some(&child), &thread, "Postgres or SQLite?").await;
    let received = alerts(manager, &tree.parent).await;
    assert_eq!(received.len(), 1, "{received:?}");
    assert!(received[0].contains(&thread.number.to_string()));
    assert!(manager.has_waiting_threads(&tree.child.id).await.unwrap());

    let answered = send(manager, Some(&caller(&tree.parent)), &thread, "SQLite").await;
    assert_eq!(answered.awaits(), Some(Side::Opener));
    assert!(!manager.has_waiting_threads(&tree.child.id).await.unwrap());
    let replies = alerts(manager, &tree.child).await;
    assert!(
        replies.iter().any(|reply| reply.contains("SQLite")),
        "{replies:?}"
    );
}

#[tokio::test]
async fn the_user_asks_a_space_and_hears_its_reply() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.beside)).await;
    assert_eq!(thread.opener, Sender::User);
    assert_eq!(thread.number.space, tree.beside.number);
    send(manager, None, &thread, "How far along are you?").await;
    assert_eq!(alerts(manager, &tree.beside).await.len(), 1);

    let replied = send(manager, Some(&caller(&tree.beside)), &thread, "Halfway").await;
    assert!(replied.waits_for_user());
    let told = users(manager, AlertKind::ThreadPosted).await;
    assert!(
        told.iter().any(|alert| alert.contains("Halfway")),
        "{told:?}"
    );
    // A space outside the thread neither reads it nor posts in it.
    let outsider = caller(&tree.child);
    assert!(
        manager
            .thread(Some(&outsider), full(&thread))
            .await
            .is_err()
    );
    let posted = manager
        .post_to_thread(
            Some(&outsider),
            full(&thread),
            post("Hi"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .await;
    assert!(posted.is_err());
}

#[tokio::test]
async fn the_user_opens_a_thread_only_with_a_space() {
    let tree = tree().await;
    let opened = (tree.manager)
        .open_thread(
            None,
            "Which?".into(),
            ThreadKind::Question,
            None,
            Vec::new(),
        )
        .await;
    assert!(opened.is_err());
}

#[tokio::test]
async fn only_shoal_opens_a_review_thread() {
    let tree = tree().await;
    let to = SpaceSelector::id(&tree.child.id);
    let opened = (tree.manager)
        .open_thread(
            None,
            "Unchecked length".into(),
            ThreadKind::Review,
            Some(&to),
            Vec::new(),
        )
        .await;
    assert!(opened.is_err());
}

#[tokio::test]
async fn a_holder_forwards_its_thread_up_and_down() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.child)).await;
    send(manager, None, &thread, "Which port?").await;
    let child = caller(&tree.child);

    // Up, to its coordinator, which reads what came before.
    let forwarded = manager
        .forward_thread(
            Some(&child),
            full(&thread),
            None,
            Some("You chose it".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        forwarded.holder.space().map(|holder| &holder.id),
        Some(&tree.parent.id)
    );
    let received = alerts(manager, &tree.parent).await;
    assert!(
        received
            .iter()
            .any(|alert| alert.contains("Which port?") && alert.contains("You chose it")),
        "{received:?}"
    );
    // The earlier holder reads it but no longer holds it.
    assert!(manager.thread(Some(&child), full(&thread)).await.is_ok());
    let again = manager.forward_thread(Some(&child), full(&thread), None, None);
    assert!(again.await.is_err());

    // Down, to a space below the new holder, which receives the posts
    // from then on.
    let sibling = SpaceSelector::id(&tree.sibling.id);
    let parent = caller(&tree.parent);
    manager
        .forward_thread(Some(&parent), full(&thread), Some(&sibling), None)
        .await
        .unwrap();
    let before = alerts(manager, &tree.child).await.len();
    send(manager, None, &thread, "Any news?").await;
    let news = alerts(manager, &tree.sibling).await;
    assert!(
        news.iter().any(|alert| alert.contains("Any news?")),
        "{news:?}"
    );
    assert_eq!(alerts(manager, &tree.child).await.len(), before);
}

#[tokio::test]
async fn a_thread_the_user_opened_never_goes_back_to_the_user() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.beside)).await;
    send(manager, None, &thread, "Which port?").await;
    // The space has no coordinator, so passing the thread up would hand it
    // to the user who asked.
    let beside = caller(&tree.beside);
    let up = manager.forward_thread(Some(&beside), full(&thread), None, None);
    assert!(up.await.is_err());
    let kept = manager.thread(None, full(&thread)).await.unwrap();
    assert_eq!(
        kept.holder.space().map(|holder| &holder.id),
        Some(&tree.beside.id)
    );
}

#[tokio::test]
async fn a_post_that_would_outgrow_one_response_is_refused_whole() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.beside)).await;
    let long = "x".repeat(shoal_protocol::threads::MAX_POST_LEN);
    let mut posted = 0;
    let refused = loop {
        let post = manager.post_to_thread(
            None,
            full(&thread),
            post(&long),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        match post.await {
            Ok(_) => posted += 1,
            Err(error) => break error,
        }
    };
    assert!(format!("{refused:#}").contains("new thread"), "{refused:#}");
    // The refused post left nothing behind, and the thread still fits.
    let kept = manager.thread(None, full(&thread)).await.unwrap();
    assert_eq!(kept.posts.len(), posted);
    let bytes = serde_json::to_vec(&kept).unwrap().len();
    assert!(
        bytes <= shoal_protocol::threads::MAX_THREAD_BYTES,
        "{bytes}"
    );
}

#[tokio::test]
async fn a_rename_keeps_a_full_thread_within_one_response() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.beside)).await;
    let beside = caller(&tree.beside);
    // Short posts from the space, until the thread is full.
    for index in 0.. {
        let text = format!("Step {index}");
        let posted = manager.post_to_thread(
            Some(&beside),
            full(&thread),
            post(&text),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        if posted.await.is_err() {
            break;
        }
    }
    let renamed = SpaceName::parse(&"w".repeat(shoal_protocol::space_names::MAX_SPACE_NAME_LEN));
    (manager.rename_space(&SpaceSelector::id(&tree.beside.id), &renamed.unwrap()))
        .await
        .unwrap();
    let kept = manager.thread(None, full(&thread)).await.unwrap();
    let bytes = serde_json::to_vec(&kept).unwrap().len();
    assert!(
        bytes <= shoal_protocol::threads::MAX_THREAD_BYTES,
        "{bytes}"
    );
}

#[tokio::test]
async fn a_space_being_removed_starts_no_wait() {
    let tree = tree().await;
    let manager = &tree.manager;
    let child = caller(&tree.child);
    let thread = open(manager, Some(&child), None).await;
    let _removal = manager.space_removals.reserve(&tree.child.id).unwrap();
    let asked = manager.post_to_thread(
        Some(&child),
        full(&thread),
        post("Which?"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(asked.await.is_err());
    assert!(!manager.has_waiting_threads(&tree.child.id).await.unwrap());
    // Its holder's post makes nothing wait.
    let parent = caller(&tree.parent);
    let told = manager.post_to_thread(
        Some(&parent),
        full(&thread),
        post("FYI"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    told.await.unwrap();
}

#[tokio::test]
async fn a_marked_turn_posts_nothing_in_a_thread_that_went_elsewhere() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.parent)).await;
    send(manager, None, &thread, "Which database?").await;
    let selector = SpaceSelector::id(&tree.parent.id);
    let agent = Caller {
        execution: Some(ScopedExecution {
            id: tree.agent.clone(),
            kind: ExecutionKind::Command,
            agent_execution: None,
        }),
        ..caller(&tree.parent)
    };
    (manager.mark_chat_post(Some(&agent), &selector, full(&thread)))
        .await
        .unwrap();
    let child = SpaceSelector::id(&tree.child.id);
    (manager.forward_thread(Some(&agent), full(&thread), Some(&child), None))
        .await
        .unwrap();
    let ending = TurnEnd {
        final_message: Some("SQLite".into()),
        ..TurnEnd::default()
    };
    (manager.agents.end_agent_turn(
        &**manager,
        &selector,
        Some(tree.agent.clone()),
        None,
        ending,
    ))
    .await
    .unwrap();
    let kept = manager.thread(None, full(&thread)).await.unwrap();
    assert!(
        !kept.posts.iter().any(|post| post.in_chat),
        "{:?}",
        kept.posts
    );
}

#[tokio::test]
async fn a_post_from_a_side_lost_meanwhile_is_refused() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.child)).await;
    send(manager, None, &thread, "Which port?").await;
    // The child's side was checked while it held the thread, which then
    // went up before its post was recorded.
    let poster = super::posting::Poster {
        side: Side::Holder,
        space: Some(ThreadSpace::here(tree.child.id.clone())),
        seen: thread.holder.clone(),
    };
    let child = caller(&tree.child);
    (manager.forward_thread(Some(&child), full(&thread), None, None))
        .await
        .unwrap();
    let removing = manager.space_removals.clone();
    let (id, reply) = (thread.id, post("8080"));
    let posted = manager
        .store
        .write(move |tx| {
            let attached = super::posting::Attached::default();
            super::posting::record_post(tx, id, &poster, &reply, attached, false, &removing)
        })
        .await;
    assert!(posted.is_err());
    let kept = manager.thread(None, full(&thread)).await.unwrap();
    assert!(!kept.posts.iter().any(|post| post.post.text == "8080"));
}

#[tokio::test]
async fn the_user_s_threads_list_past_larger_newer_threads() {
    let tree = tree().await;
    let manager = &tree.manager;
    let asked = open(manager, None, Some(&tree.beside)).await;
    send(manager, Some(&caller(&tree.beside)), &asked, "Which port?").await;
    // Newer threads that wait for spaces fill a response on their own.
    let long = "x".repeat(shoal_protocol::threads::MAX_POST_LEN);
    for space in [&tree.child, &tree.sibling] {
        let thread = open(manager, None, Some(space)).await;
        while (manager.post_to_thread(
            None,
            full(&thread),
            post(&long),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ))
        .await
        .is_ok()
        {}
    }
    let open = shoal_protocol::threads::ThreadState::Open;
    let all = manager.threads(None, open, false).await.unwrap();
    assert!(!all.threads.iter().any(|thread| thread.id == asked.id));
    let mine = manager.threads(None, open, true).await.unwrap();
    let numbers: Vec<_> = mine.threads.iter().map(|thread| thread.number).collect();
    assert_eq!(numbers, [asked.number]);
}

/// Remove the space `id`, its unsaved work and all.
async fn remove(manager: &Manager, id: &shoal_ids::space_ids::SpaceId) {
    use crate::removal::{InspectionPolicy, UnsavedWork};
    (manager.remove_selected(
        &SpaceSelector::id(id),
        UnsavedWork::All,
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();
}

/// A thread the parent opened with its child, whose removals, the child's
/// then the parent's, closed it, restored with the child first when `child_first`.
async fn restored_after_removals(child_first: bool) -> Thread {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .create_child(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let thread = open(&manager, Some(&caller(&parent)), Some(&child)).await;
    send(&manager, Some(&caller(&parent)), &thread, "Which port?").await;
    remove(&manager, &child.id).await;
    remove(&manager, &parent.id).await;
    let order = match child_first {
        true => [&child.id, &parent.id],
        false => [&parent.id, &child.id],
    };
    for id in order {
        (manager.restore_space(&SpaceSelector::id(id), None))
            .await
            .unwrap();
    }
    let parent = manager.space(&SpaceSelector::id(&parent.id)).await.unwrap();
    let number = ThreadNumber::Full(shoal_protocol::item_numbers::ItemNumber {
        space: parent.number,
        item: thread.number.item,
    });
    manager.thread(None, number).await.unwrap()
}

#[tokio::test]
async fn restoring_a_child_then_its_parent_reopens_their_thread() {
    assert!(restored_after_removals(true).await.is_open());
}

#[tokio::test]
async fn restoring_a_parent_then_its_child_reopens_their_thread() {
    assert!(restored_after_removals(false).await.is_open());
}

#[tokio::test]
async fn a_coordinator_posts_in_its_chat_only_in_threads_it_takes_part_in() {
    let tree = tree().await;
    let manager = &tree.manager;
    // The child holds the thread, which its coordinator reads and posts in
    // with `send`, but not from its chat.
    let thread = open(manager, None, Some(&tree.child)).await;
    send(manager, None, &thread, "Which port?").await;
    let agent = Caller {
        execution: Some(ScopedExecution {
            id: tree.agent.clone(),
            kind: ExecutionKind::Command,
            agent_execution: None,
        }),
        ..caller(&tree.parent)
    };
    let selector = SpaceSelector::id(&tree.parent.id);
    let marked = manager.mark_chat_post(Some(&agent), &selector, full(&thread));
    assert!(marked.await.is_err());
}

#[tokio::test]
async fn the_opener_resolves_its_thread_and_takes_back_an_unread_notice() {
    let tree = tree().await;
    let manager = &tree.manager;
    let (child, parent) = (caller(&tree.child), caller(&tree.parent));
    let thread = open(manager, Some(&child), None).await;
    send(manager, Some(&child), &thread, "Postgres or SQLite?").await;
    assert_eq!(alerts(manager, &tree.parent).await.len(), 1);

    let withdraw = OutcomeKind::Withdrawn;
    let by_holder = manager
        .resolve_thread(
            Some(&parent),
            full(&thread),
            Resolving {
                outcome: Some(withdraw),
                ..Resolving::default()
            },
        )
        .await;
    assert!(by_holder.is_err());
    let withdrawn = manager
        .resolve_thread(
            Some(&child),
            full(&thread),
            Resolving {
                outcome: Some(withdraw),
                ..Resolving::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        withdrawn.resolution.map(|resolution| resolution.outcome),
        Some(Outcome::Withdrawn { reason: None })
    );
    assert!(alerts(manager, &tree.parent).await.is_empty());
    let resolved = manager.post_to_thread(
        Some(&child),
        full(&thread),
        post("More"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(resolved.await.is_err());

    let reopened = manager
        .resolve_thread(Some(&child), full(&thread), Resolving::default())
        .await
        .unwrap();
    assert!(reopened.is_open());
    assert!(manager.has_waiting_threads(&tree.child.id).await.unwrap());
}

#[tokio::test]
async fn an_answer_picks_one_of_the_choices_the_other_side_offered() {
    let tree = tree().await;
    let manager = &tree.manager;
    let child = caller(&tree.child);
    let thread = open(manager, Some(&child), None).await;
    let asked = NewPost {
        choices: vec!["Postgres".into(), "SQLite".into()],
        recommendation: Some("SQLite".into()),
        ..post("Which database?")
    };
    (manager.post_to_thread(
        Some(&child),
        full(&thread),
        asked,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
    .await
    .unwrap();
    let parent = caller(&tree.parent);
    let choose = |choice| NewPost {
        choice: Some(choice),
        ..NewPost::default()
    };
    let outside = manager.post_to_thread(
        Some(&parent),
        full(&thread),
        choose(3),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(outside.await.is_err());
    let chose = manager
        .post_to_thread(
            Some(&parent),
            full(&thread),
            choose(2),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(chose.last_post().unwrap().post.text, "SQLite");
    // The opener offered no choices to itself.
    let own = manager.post_to_thread(
        Some(&child),
        full(&thread),
        choose(1),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(own.await.is_err());
}

#[tokio::test]
async fn done_closes_the_threads_a_space_holds_and_its_undo_reopens_them() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.beside)).await;
    send(manager, None, &thread, "How far along are you?").await;
    let selector = SpaceSelector::id(&tree.beside.id);

    (manager.mark_space_done(&selector, Done::default(), &Attacher::User))
        .await
        .unwrap();
    let closed = manager.thread(None, full(&thread)).await.unwrap();
    assert_eq!(
        closed.resolution.map(|resolution| resolution.outcome),
        Some(Outcome::Closed {
            cause: CloseCause::Completion
        })
    );
    let told = users(manager, AlertKind::ThreadPosted).await;
    assert!(
        told.iter().any(|alert| alert.contains("finished its work")),
        "{told:?}"
    );

    manager.undo_space_done(&selector).await.unwrap();
    let reopened = manager.thread(None, full(&thread)).await.unwrap();
    assert!(reopened.is_open());
    assert!(
        (reopened.posts.iter()).all(|post| post.sender != Sender::Shoal),
        "{:?}",
        reopened.posts
    );
}

#[tokio::test]
async fn a_thread_resolved_otherwise_stays_resolved_when_done_is_undone() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.beside)).await;
    send(manager, None, &thread, "How far along are you?").await;
    let selector = SpaceSelector::id(&tree.beside.id);
    (manager.mark_space_done(&selector, Done::default(), &Attacher::User))
        .await
        .unwrap();
    let answered = Some(OutcomeKind::Answered);
    (manager.resolve_thread(
        None,
        full(&thread),
        Resolving {
            outcome: answered,
            ..Resolving::default()
        },
    ))
    .await
    .unwrap();
    manager.undo_space_done(&selector).await.unwrap();
    let kept = manager.thread(None, full(&thread)).await.unwrap();
    assert_eq!(
        kept.resolution.map(|resolution| resolution.outcome.kind()),
        Some(OutcomeKind::Answered)
    );
}

#[tokio::test]
async fn a_space_that_linked_a_thread_receives_its_new_posts() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.child)).await;
    let watcher = SpaceSelector::id(&tree.beside.id);
    // A space reads the threads below it, so it links them.
    let parent = caller(&tree.parent);
    let beside = caller(&tree.beside);
    let outside = manager.link_thread(Some(&beside), &watcher, thread.number, true);
    assert!(outside.await.is_err());
    (manager.link_thread(
        Some(&parent),
        &SpaceSelector::id(&tree.parent.id),
        thread.number,
        true,
    ))
    .await
    .unwrap();
    (manager.link_thread(None, &watcher, thread.number, true))
        .await
        .unwrap();
    send(manager, None, &thread, "Which port?").await;
    for linker in [&tree.beside, &tree.parent] {
        let linked = alerts(manager, linker).await;
        assert!(
            linked.iter().any(|alert| alert.contains("Which port?")),
            "{linked:?}"
        );
    }
    (manager.link_thread(None, &watcher, thread.number, false))
        .await
        .unwrap();
    send(manager, None, &thread, "Any news?").await;
    let unlinked = alerts(manager, &tree.beside).await;
    assert!(!unlinked.iter().any(|alert| alert.contains("Any news?")));
}

#[tokio::test]
async fn a_bare_number_names_a_thread_of_the_callers_own_space() {
    let tree = tree().await;
    let manager = &tree.manager;
    let child = caller(&tree.child);
    let thread = open(manager, Some(&child), None).await;
    let own = ThreadNumber::Own(thread.number.item);
    assert_eq!(
        manager.thread(Some(&child), own).await.unwrap().id,
        thread.id
    );
    assert!(manager.thread(None, own).await.is_err());
}

#[tokio::test]
async fn a_marked_turn_posts_its_final_message_which_a_reply_in_the_chat_answers() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.parent)).await;
    send(manager, None, &thread, "Which database?").await;
    let selector = SpaceSelector::id(&tree.parent.id);
    let agent = Caller {
        execution: Some(ScopedExecution {
            id: tree.agent.clone(),
            kind: ExecutionKind::Command,
            agent_execution: None,
        }),
        ..caller(&tree.parent)
    };
    let end_turn = async |message: &str| {
        let ending = TurnEnd {
            final_message: Some(message.into()),
            ..TurnEnd::default()
        };
        manager
            .agents
            .end_agent_turn(
                &**manager,
                &selector,
                Some(tree.agent.clone()),
                None,
                ending,
            )
            .await
            .unwrap();
    };
    (manager.mark_chat_post(Some(&agent), &selector, full(&thread)))
        .await
        .unwrap();
    end_turn("Postgres or SQLite?\n\nI lean towards SQLite.").await;
    // The mark lasts one turn.
    end_turn("Still waiting.").await;
    let posted = manager.thread(None, full(&thread)).await.unwrap();
    let posts: Vec<_> = (posted.posts.iter())
        .map(|post| (post.in_chat, post.post.text.as_str()))
        .collect();
    assert_eq!(
        posts,
        [
            (false, "Which database?"),
            (true, "Postgres or SQLite?\n\nI lean towards SQLite.")
        ]
    );
    assert!(posted.waits_for_user());

    (manager.reply_in_chat(&selector, Some(&tree.agent), Some("SQLite it is".into())))
        .await
        .unwrap();
    let replied = manager.thread(None, full(&thread)).await.unwrap();
    let reply = replied.last_post().unwrap();
    assert_eq!(
        (reply.sender, reply.in_chat, reply.post.text.as_str()),
        (Sender::User, true, "SQLite it is")
    );
    assert!(!replied.waits_for_user());
    // The user had not read the post, so its alert goes.
    assert!(
        !(users(manager, AlertKind::ThreadPosted).await.iter())
            .any(|alert| alert.contains("Postgres or SQLite?"))
    );
}

#[tokio::test]
async fn a_thread_names_a_space_on_another_machine_as_the_hub_lists_it() {
    let tree = tree().await;
    let manager = &tree.manager;
    away_on_offline_machine(manager, Some(&tree.parent.id)).await;
    let away = SpaceSelector::Number(90.into());
    let parent = caller(&tree.parent);

    let opened = (manager.open_thread(
        Some(&parent),
        "Which database?".into(),
        ThreadKind::Question,
        Some(&away),
        Vec::new(),
    ))
    .await
    .unwrap();
    assert_eq!(opened.holder.label(), "90 away");

    let given = open(manager, None, Some(&tree.parent)).await;
    let forwarded = (manager.forward_thread(Some(&parent), full(&given), Some(&away), None))
        .await
        .unwrap();
    assert_eq!(forwarded.holder.label(), "90 away");
    let forward = forwarded.posts.last().unwrap();
    assert_eq!(forward.forwarded_to.as_ref().unwrap().label(), "90 away");
}
