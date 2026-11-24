use std::sync::Arc;

use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::{
    space_names::{SpaceName, SpaceSelector},
    threads::{
        CloseCause, NewPost, Outcome, OutcomeKind, Resolving, Sender, Side, Thread, ThreadKind,
        ThreadNumber, ThreadOpening, ThreadSpace,
    },
};
use shoal_reviews::written::Pending;

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
use shoal_protocol::reviews::{Severity, severity_counts};

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
                .spaces
                .create_child(&*manager, &parent, name, None, Default::default())
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
        .coordination
        .open_thread(
            manager,
            from,
            ThreadOpening::named("Which database?"),
            Some(ThreadKind::Question),
            to.as_ref(),
        )
        .await
        .unwrap()
}

async fn send(manager: &Manager, from: Option<&Caller>, thread: &Thread, text: &str) -> Thread {
    manager
        .coordination
        .post_to_thread(
            manager,
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
async fn a_rename_keeps_a_full_thread_within_one_response() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.beside)).await;
    let beside = caller(&tree.beside);
    // Short posts from the space, until the thread is full.
    for index in 0.. {
        let text = format!("Step {index}");
        let posted = manager.coordination.post_to_thread(
            &**manager,
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
    (manager
        .spaces
        .rename_space(&SpaceSelector::id(&tree.beside.id), &renamed.unwrap()))
    .await
    .unwrap();
    let kept = manager
        .coordination
        .thread(&**manager, None, full(&thread))
        .await
        .unwrap();
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
    let _removal = manager
        .spaces
        .space_removals
        .reserve(&tree.child.id)
        .unwrap();
    let asked = manager.coordination.post_to_thread(
        &**manager,
        Some(&child),
        full(&thread),
        post("Which?"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(asked.await.is_err());
    assert_eq!(
        manager
            .coordination
            .waiting_here(&tree.child.id)
            .await
            .unwrap(),
        0
    );
    // Its holder's post makes nothing wait.
    let parent = caller(&tree.parent);
    let told = manager.coordination.post_to_thread(
        &**manager,
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
    (manager
        .spaces
        .mark_chat_post(&**manager, Some(&agent), &selector, full(&thread)))
    .await
    .unwrap();
    let child = SpaceSelector::id(&tree.child.id);
    (manager.coordination.forward_thread(
        &**manager,
        Some(&agent),
        full(&thread),
        Some(&child),
        None,
        false,
    ))
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
    let kept = manager
        .coordination
        .thread(&**manager, None, full(&thread))
        .await
        .unwrap();
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
    let poster = shoal_coordination::threads::posting::Poster {
        side: Some(Side::Holder),
        sender: Sender::Space,
        space: Some(ThreadSpace::here(tree.child.id.clone())),
        seen: thread.holder.clone(),
    };
    let child = caller(&tree.child);
    (manager.coordination.forward_thread(
        &**manager,
        Some(&child),
        full(&thread),
        None,
        None,
        false,
    ))
    .await
    .unwrap();
    let removing =
        crate::daemon::coordination::CoordinationRecords(manager.spaces.space_removals.clone());
    let (id, reply) = (thread.id, post("8080"));
    let posted = manager
        .store
        .write(move |tx| {
            let attached = shoal_coordination::threads::posting::Attached::default();
            shoal_coordination::threads::posting::record_post(
                tx, id, &poster, &reply, attached, false, &removing,
            )
        })
        .await;
    assert!(posted.is_err());
    let kept = manager
        .coordination
        .thread(&**manager, None, full(&thread))
        .await
        .unwrap();
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
        while (manager.coordination.post_to_thread(
            &**manager,
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
    let all = manager
        .coordination
        .threads(&**manager, None, None, open, false)
        .await
        .unwrap();
    assert!(!all.threads.iter().any(|thread| thread.id == asked.id));
    let mine = manager
        .coordination
        .threads(&**manager, None, None, open, true)
        .await
        .unwrap();
    let numbers: Vec<_> = mine.threads.iter().map(|thread| thread.number).collect();
    assert_eq!(numbers, [asked.number]);
}

/// Remove the space `id`, its unsaved work and all.
async fn remove(manager: &Manager, id: &shoal_ids::space_ids::SpaceId) {
    use crate::removal::{InspectionPolicy, UnsavedWork};
    (manager.spaces.remove_selected(
        manager,
        &SpaceSelector::id(id),
        UnsavedWork::All,
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn restoring_a_child_then_its_parent_reopens_their_thread() {
    assert!(restored_after_removals(true).await.is_open());
}

#[tokio::test]
async fn restoring_a_parent_then_its_child_reopens_their_thread() {
    assert!(restored_after_removals(false).await.is_open());
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
        .spaces
        .create_child(
            &*manager,
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
        (manager
            .spaces
            .restore_space(&*manager, &SpaceSelector::id(id), None))
        .await
        .unwrap();
    }
    let parent = manager
        .spaces
        .space(&SpaceSelector::id(&parent.id))
        .await
        .unwrap();
    let number = ThreadNumber::Full(shoal_protocol::item_numbers::ItemNumber {
        space: parent.number,
        item: thread.number.item,
    });
    manager
        .coordination
        .thread(&*manager, None, number)
        .await
        .unwrap()
}

#[tokio::test]
async fn done_closes_the_threads_a_space_holds_and_its_undo_reopens_them() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.beside)).await;
    send(manager, None, &thread, "How far along are you?").await;
    let selector = SpaceSelector::id(&tree.beside.id);

    (manager
        .spaces
        .mark_space_done(&**manager, &selector, Done::default(), &Attacher::User))
    .await
    .unwrap();
    let closed = manager
        .coordination
        .thread(&**manager, None, full(&thread))
        .await
        .unwrap();
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

    manager
        .spaces
        .undo_space_done(&**manager, &selector)
        .await
        .unwrap();
    let reopened = manager
        .coordination
        .thread(&**manager, None, full(&thread))
        .await
        .unwrap();
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
    (manager
        .spaces
        .mark_space_done(&**manager, &selector, Done::default(), &Attacher::User))
    .await
    .unwrap();
    let answered = Some(OutcomeKind::Answered);
    (manager.coordination.resolve_thread(
        &**manager,
        None,
        full(&thread),
        Resolving {
            outcome: answered,
            ..Resolving::default()
        },
    ))
    .await
    .unwrap();
    manager
        .spaces
        .undo_space_done(&**manager, &selector)
        .await
        .unwrap();
    let kept = manager
        .coordination
        .thread(&**manager, None, full(&thread))
        .await
        .unwrap();
    assert_eq!(
        kept.resolution.map(|resolution| resolution.outcome.kind()),
        Some(OutcomeKind::Answered)
    );
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
    (manager
        .spaces
        .mark_chat_post(&**manager, Some(&agent), &selector, full(&thread)))
    .await
    .unwrap();
    end_turn("Postgres or SQLite?\n\nI lean towards SQLite.").await;
    // The mark lasts one turn.
    end_turn("Still waiting.").await;
    let posted = manager
        .coordination
        .thread(&**manager, None, full(&thread))
        .await
        .unwrap();
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

    (manager.spaces.reply_in_chat(
        &**manager,
        &selector,
        Some(&tree.agent),
        Some("SQLite it is".into()),
    ))
    .await
    .unwrap();
    let replied = manager
        .coordination
        .thread(&**manager, None, full(&thread))
        .await
        .unwrap();
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

    let opened = (manager.coordination.open_thread(
        &**manager,
        Some(&parent),
        ThreadOpening::named("Which database?"),
        Some(ThreadKind::Question),
        Some(&away),
    ))
    .await
    .unwrap();
    assert_eq!(opened.holder.label(), "90 away");

    let given = open(manager, None, Some(&tree.parent)).await;
    let forwarded = (manager.coordination.forward_thread(
        &**manager,
        Some(&parent),
        full(&given),
        Some(&away),
        None,
        false,
    ))
    .await
    .unwrap();
    assert_eq!(forwarded.holder.label(), "90 away");
    let forward = forwarded.posts.last().unwrap();
    assert_eq!(forward.forwarded_to.as_ref().unwrap().label(), "90 away");
}

/// Open a thread named "Unclear name" without a kind from `tree`'s
/// parent towards the space `away` lists on its offline machine, as `kind`.
async fn open_towards_offline(tree: &Tree, kind: &str) -> anyhow::Result<Thread> {
    let manager = &tree.manager;
    away_on_offline_machine(manager, Some(&tree.parent.id)).await;
    let kind = kind.to_owned();
    (manager.store)
        .write(move |tx| {
            tx.execute(
                "UPDATE hub_spaces SET summary=json_set(summary,'$.kind',?1)",
                [kind],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let away = SpaceSelector::Number(90.into());
    (manager.coordination.open_thread(
        &**manager,
        Some(&caller(&tree.parent)),
        ThreadOpening::named("Unclear name"),
        None,
        Some(&away),
    ))
    .await
}

#[tokio::test]
async fn a_thread_without_a_kind_is_refused_while_the_reviewing_machine_is_offline() {
    let tree = tree().await;
    let error = format!(
        "{:#}",
        open_towards_offline(&tree, "workspace").await.unwrap_err()
    );
    assert!(error.contains("--kind question"), "{error}");
    let open = tree.manager.coordination.open_threads().await.unwrap();
    assert!(open.is_empty(), "{open:?}");
}

#[tokio::test]
async fn a_thread_without_a_kind_to_a_chat_on_an_offline_machine_is_a_question() {
    let tree = tree().await;
    let opened = open_towards_offline(&tree, "chat").await.unwrap();
    assert_eq!(opened.kind, ThreadKind::Question);
}

/// Start `writer`'s pending review of `workspace`.
async fn start_review(
    manager: &Manager,
    workspace: &Workspace,
    writer: shoal_reviews::written::Writer,
) -> shoal_protocol::reviews::Review {
    let selector = SpaceSelector::id(&workspace.id);
    (manager.reviews.start_written(manager, &selector, writer))
        .await
        .unwrap()
}

/// Open a thread named `name` of `kind`, or of none, from `from` towards
/// `to`.
async fn open_kind(
    manager: &Manager,
    from: Option<&Caller>,
    to: &Workspace,
    name: &str,
    kind: Option<ThreadKind>,
) -> Thread {
    let to = SpaceSelector::id(&to.id);
    (manager
        .coordination
        .open_thread(manager, from, ThreadOpening::named(name), kind, Some(&to)))
    .await
    .unwrap()
}

/// A comment that gives its severity and body opens with the body as its
/// one post and counts in the review's tallies at that severity.
#[tokio::test]
async fn a_comments_severity_and_body_make_one_post_of_its_severity() {
    let tree = tree().await;
    let (manager, parent) = (&tree.manager, caller(&tree.parent));
    let writer = shoal_reviews::written::Writer::Space(tree.parent.id.clone());
    start_review(manager, &tree.child, writer).await;
    let opening = ThreadOpening {
        body: Some("Call it retries".into()),
        severity: Some(Severity::ShouldFix),
        ..ThreadOpening::named("Unclear name")
    };
    let to = SpaceSelector::id(&tree.child.id);
    let thread =
        (manager
            .coordination
            .open_thread(&**manager, Some(&parent), opening, None, Some(&to)))
        .await
        .unwrap();
    assert_eq!(thread.name, "Unclear name");
    let posts: Vec<_> = (thread.posts.iter()).map(|post| &post.post.text).collect();
    assert_eq!(posts, ["Call it retries"]);
    assert_eq!(
        severity_counts(std::slice::from_ref(&thread)),
        "1 should-fix"
    );
}

/// A severity marks only a comment of a review: a thread that would be a
/// question is refused, and none is left.
#[tokio::test]
async fn a_severity_outside_a_review_is_refused() {
    let tree = tree().await;
    let (manager, parent) = (&tree.manager, caller(&tree.parent));
    let opening = ThreadOpening {
        severity: Some(Severity::Nit),
        ..ThreadOpening::named("Unclear name")
    };
    let to = SpaceSelector::id(&tree.child.id);
    let opened =
        manager
            .coordination
            .open_thread(&**manager, Some(&parent), opening, None, Some(&to));
    assert!(opened.await.is_err());
    let open = manager.coordination.open_threads().await.unwrap();
    assert!(open.is_empty(), "{open:?}");
}

#[tokio::test]
async fn a_parents_threads_towards_a_child_it_reviews_join_the_review_and_reach_nobody() {
    let tree = tree().await;
    let (manager, parent) = (&tree.manager, caller(&tree.parent));
    let writer = shoal_reviews::written::Writer::Space(tree.parent.id.clone());
    let review = start_review(manager, &tree.child, writer).await;
    let thread = open_kind(manager, Some(&parent), &tree.child, "Unclear name", None).await;
    assert_eq!(thread.kind, ThreadKind::Review);
    assert_eq!(thread.finding.and_then(|f| f.review), Some(review.id));
    assert!(thread.held);
    send(manager, Some(&parent), &thread, "Call it retries").await;
    assert!(alerts(manager, &tree.child).await.is_empty());
    // Its holder has not heard of it, so it neither posts nor resolves.
    let child = caller(&tree.child);
    let posted = manager.coordination.post_to_thread(
        &**manager,
        Some(&child),
        full(&thread),
        post("Done"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(posted.await.is_err());
    assert!(
        manager
            .coordination
            .thread(&**manager, Some(&child), full(&thread))
            .await
            .is_err()
    );
    let open = shoal_protocol::threads::ThreadState::Open;
    let own = SpaceSelector::id(&tree.child.id);
    let listed = (manager
        .coordination
        .threads(&**manager, Some(&child), Some(&own), open, false))
    .await;
    assert!(
        listed
            .unwrap()
            .threads
            .iter()
            .all(|listed| listed.id != thread.id)
    );
    // Nor does the user take its holder's side.
    let by_user = manager.coordination.post_to_thread(
        &**manager,
        None,
        full(&thread),
        post("Done"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(by_user.await.is_err());
    // Another kind towards the child is delivered at once, outside the
    // review, as is a thread towards a space it does not review.
    let asked = open_kind(
        manager,
        Some(&parent),
        &tree.child,
        "Which?",
        Some(ThreadKind::Question),
    )
    .await;
    assert!(!asked.held && asked.finding.is_none());
    send(manager, Some(&parent), &asked, "Postgres?").await;
    assert_eq!(alerts(manager, &tree.child).await.len(), 1);
    let elsewhere = open_kind(manager, Some(&parent), &tree.sibling, "Hi", None).await;
    assert_eq!(elsewhere.kind, ThreadKind::Question);
}

#[tokio::test]
async fn cancelling_a_pending_review_deletes_its_threads() {
    let tree = tree().await;
    let manager = &tree.manager;
    let review = start_review(manager, &tree.child, shoal_reviews::written::Writer::User).await;
    let thread = open_kind(manager, None, &tree.child, "Unclear name", None).await;
    let child = SpaceSelector::id(&tree.child.id);
    let cancelled = (manager.reviews).cancel(&**manager, Some(&child), None, None);
    assert_eq!(cancelled.await.unwrap().id, review.id);
    assert!(
        manager
            .coordination
            .thread(&**manager, None, full(&thread))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_thread_that_holds_its_posts_reaches_nobody_and_its_holder_takes_no_side() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.child)).await;
    let id = thread.id;
    (manager.store)
        .write(move |tx| Ok(tx.execute("UPDATE threads SET held=1 WHERE id=?1", [id])?))
        .await
        .unwrap();
    let posted = send(manager, None, &thread, "Call it retries").await;
    assert!(posted.held);
    assert!(alerts(manager, &tree.child).await.is_empty());
    let child = caller(&tree.child);
    let replied = manager.coordination.post_to_thread(
        &**manager,
        Some(&child),
        full(&thread),
        post("Done"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(replied.await.is_err());
}

#[tokio::test]
async fn a_submitted_review_reaches_its_holder_once_and_its_comments_then_post_as_any_thread() {
    let tree = tree().await;
    let (manager, parent) = (&tree.manager, caller(&tree.parent));
    let writer = shoal_reviews::written::Writer::Space(tree.parent.id.clone());
    start_review(manager, &tree.child, writer.clone()).await;
    let thread = open_kind(manager, Some(&parent), &tree.child, "Unclear name", None).await;
    send(manager, Some(&parent), &thread, "Call it retries").await;
    // Its writer's only pending review, which a submit without a space takes.
    let pending = Pending::new(&manager.reviews, &**manager, Some(&parent), false);
    pending.submit_only(false, None).await.unwrap();
    let received = alerts(manager, &tree.child).await;
    assert_eq!(received.len(), 1, "{received:?}");
    assert!(received[0].contains("Unclear name"), "{received:?}");
    let replied = send(manager, Some(&caller(&tree.child)), &thread, "Renamed").await;
    assert!(!replied.held);
    let heard = alerts(manager, &tree.parent).await;
    assert!(
        heard.iter().any(|alert| alert.contains("Renamed")),
        "{heard:?}"
    );
}

#[tokio::test]
async fn a_bare_cancel_in_the_writers_own_space_drops_its_only_pending_review() {
    let tree = tree().await;
    let manager = &tree.manager;
    let writer = shoal_reviews::written::Writer::Space(tree.parent.id.clone());
    let review = start_review(manager, &tree.child, writer).await;
    let own = SpaceSelector::id(&tree.parent.id);
    // Named, the space has nothing to cancel, and nothing else goes.
    let named =
        (manager.reviews).cancel(&**manager, Some(&own), None, Some(tree.parent.id.clone()));
    assert!(named.await.is_err());
    let parent = caller(&tree.parent);
    let pending = Pending::new(&manager.reviews, &**manager, Some(&parent), false);
    assert_eq!(pending.cancel_here(&own).await.unwrap().id, review.id);
}

#[tokio::test]
async fn a_pending_reviews_comments_show_in_the_review_to_its_writers_side_only() {
    let tree = tree().await;
    let (manager, parent) = (&tree.manager, caller(&tree.parent));
    let writer = shoal_reviews::written::Writer::Space(tree.parent.id.clone());
    let review = start_review(manager, &tree.child, writer).await;
    open_kind(manager, Some(&parent), &tree.child, "Unclear name", None).await;
    let review = manager.reviews.review(&**manager, review.id).await.unwrap();
    let seen = |caller: Option<Caller>| {
        let review = review.clone();
        async move {
            let seen = crate::daemon::requests::reviews::seen_by(manager, caller.as_ref(), review);
            seen.await.unwrap().threads.len()
        }
    };
    assert_eq!(seen(Some(caller(&tree.child))).await, 0);
    assert_eq!(seen(Some(parent.clone())).await, 1);
    assert_eq!(seen(None).await, 1);
}
