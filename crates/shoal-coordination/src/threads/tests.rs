use shoal_protocol::reviews::{ReviewId, Severity};
use shoal_protocol::threads::ThreadOpening;
use shoal_protocol::{
    space_names::SpaceSelector,
    surfaces::Surface,
    threads::{
        Answer, NewPost, Outcome, OutcomeKind, Resolving, Sender, Side, Thread, ThreadKind,
        ThreadNumber,
    },
};

use crate::{
    alerts::AlertKind,
    model::Workspace,
    scope::{Access, Caller},
    stand_in::Fixture,
};

/// A workspace `parent` whose agent runs, two workspaces below it whose
/// agents run, and a workspace beside it without one.
pub(crate) struct Tree {
    pub(crate) fixture: Fixture,
    pub(crate) parent: Workspace,
    pub(crate) child: Workspace,
    pub(crate) sibling: Workspace,
    pub(crate) beside: Workspace,
}

/// The tree, each workspace in a Git repository, where the code a thread
/// points at is followed.
pub(crate) async fn tree() -> Tree {
    let fixture = Fixture::new().await;
    for name in ["parent", "child", "sibling", "beside"] {
        shoal_core::test_support::repository(fixture.root.path(), name);
    }
    let parent = fixture.workspace("parent", None).await;
    let child = fixture.workspace("child", Some(&parent.id)).await;
    let sibling = fixture.workspace("sibling", Some(&parent.id)).await;
    let beside = fixture.workspace("beside", None).await;
    // A space's agent asks its coordinator; without one, whoever acted
    // there asks the user.
    for agent in [&parent, &child, &sibling] {
        fixture.run_agent(&agent.id);
    }
    Tree {
        fixture,
        parent,
        child,
        sibling,
        beside,
    }
}

pub(crate) fn caller(workspace: &Workspace) -> Caller {
    Caller::of_space(workspace.id.clone())
}

pub(crate) fn post(text: &str) -> NewPost {
    NewPost {
        text: text.into(),
        ..NewPost::default()
    }
}

pub(crate) fn full(thread: &Thread) -> ThreadNumber {
    ThreadNumber::Full(thread.number)
}

pub(crate) async fn open(
    fixture: &Fixture,
    from: Option<&Caller>,
    to: Option<&Workspace>,
) -> Thread {
    let to = to.map(|workspace| SpaceSelector::id(&workspace.id));
    fixture
        .coordination
        .open_thread(
            &**fixture,
            from,
            ThreadOpening::named("Which database?"),
            Some(ThreadKind::Question),
            to.as_ref(),
        )
        .await
        .unwrap()
}

pub(crate) async fn send(
    fixture: &Fixture,
    from: Option<&Caller>,
    thread: &Thread,
    text: &str,
) -> Thread {
    fixture
        .coordination
        .post_to_thread(
            &**fixture,
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

/// The alerts about threads queued for `workspace`'s agent.
pub(crate) async fn alerts(fixture: &Fixture, workspace: &Workspace) -> Vec<String> {
    (fixture.queued(&workspace.id).await.into_iter())
        .filter(|message| message.contains("hread"))
        .collect()
}

/// The user's unread alerts of `kind`.
async fn users(fixture: &Fixture, kind: AlertKind) -> Vec<String> {
    let unread = fixture.notifications.list(true, 50).await.unwrap();
    (unread.into_iter())
        .filter(|alert| alert.kind == kind)
        .map(|alert| alert.message)
        .collect()
}

#[tokio::test]
async fn an_empty_thread_reaches_its_holder_with_its_first_post() {
    let tree = tree().await;
    let (fixture, child) = (&tree.fixture, caller(&tree.child));
    let thread = open(fixture, Some(&child), None).await;
    // Its coordinator holds it; until a post nobody hears of it, and it
    // keeps nothing.
    assert_eq!(
        thread.holder.space().map(|holder| &holder.id),
        Some(&tree.parent.id)
    );
    assert_eq!(thread.number.space, tree.child.number);
    assert!(alerts(fixture, &tree.parent).await.is_empty());
    assert_eq!(
        fixture
            .coordination
            .waiting_here(&tree.child.id)
            .await
            .unwrap(),
        0
    );

    send(fixture, Some(&child), &thread, "Postgres or SQLite?").await;
    let received = alerts(fixture, &tree.parent).await;
    assert_eq!(received.len(), 1, "{received:?}");
    assert!(received[0].contains(&thread.number.to_string()));
    assert_eq!(
        fixture
            .coordination
            .waiting_here(&tree.child.id)
            .await
            .unwrap(),
        1
    );

    let answered = send(fixture, Some(&caller(&tree.parent)), &thread, "SQLite").await;
    assert_eq!(answered.awaits(), Some(Side::Opener));
    assert_eq!(
        fixture
            .coordination
            .waiting_here(&tree.child.id)
            .await
            .unwrap(),
        0
    );
    let replies = alerts(fixture, &tree.child).await;
    assert!(
        replies.iter().any(|reply| reply.contains("SQLite")),
        "{replies:?}"
    );
}

#[tokio::test]
async fn the_user_asks_a_space_and_hears_its_reply() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let thread = open(fixture, None, Some(&tree.beside)).await;
    assert_eq!(thread.opener, Sender::User);
    assert_eq!(thread.number.space, tree.beside.number);
    send(fixture, None, &thread, "How far along are you?").await;
    assert_eq!(alerts(fixture, &tree.beside).await.len(), 1);

    let replied = send(fixture, Some(&caller(&tree.beside)), &thread, "Halfway").await;
    assert!(replied.waits_for_user());
    let told = users(fixture, AlertKind::ThreadPosted).await;
    assert!(
        told.iter().any(|alert| alert.contains("Halfway")),
        "{told:?}"
    );
    // A space outside the thread neither reads it nor posts in it.
    let outsider = caller(&tree.child);
    assert!(
        fixture
            .coordination
            .thread(&**fixture, Some(&outsider), full(&thread))
            .await
            .is_err()
    );
    let posted = fixture
        .coordination
        .post_to_thread(
            &**fixture,
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
    let opened = (tree.fixture)
        .coordination
        .open_thread(
            &*tree.fixture,
            None,
            ThreadOpening::named("Which?"),
            Some(ThreadKind::Question),
            None,
        )
        .await;
    assert!(opened.is_err());
}

#[tokio::test]
async fn a_review_thread_is_never_opened_by_its_kind() {
    let tree = tree().await;
    let to = SpaceSelector::id(&tree.child.id);
    let opened = (tree.fixture)
        .coordination
        .open_thread(
            &*tree.fixture,
            None,
            ThreadOpening::named("Unchecked length"),
            Some(ThreadKind::Review),
            Some(&to),
        )
        .await;
    assert!(opened.is_err());
}

#[tokio::test]
async fn a_holder_forwards_its_thread_up_and_down() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let thread = open(fixture, None, Some(&tree.child)).await;
    send(fixture, None, &thread, "Which port?").await;
    let child = caller(&tree.child);

    // Up, to its coordinator, which reads what came before.
    let forwarded = fixture
        .coordination
        .forward_thread(
            &**fixture,
            Some(&child),
            full(&thread),
            None,
            Some("You chose it".into()),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        forwarded.holder.space().map(|holder| &holder.id),
        Some(&tree.parent.id)
    );
    let received = alerts(fixture, &tree.parent).await;
    assert!(
        received
            .iter()
            .any(|alert| alert.contains("Which port?") && alert.contains("You chose it")),
        "{received:?}"
    );
    // The earlier holder reads it but no longer holds it.
    assert!(
        fixture
            .coordination
            .thread(&**fixture, Some(&child), full(&thread))
            .await
            .is_ok()
    );
    let again = fixture.coordination.forward_thread(
        &**fixture,
        Some(&child),
        full(&thread),
        None,
        None,
        false,
    );
    assert!(again.await.is_err());

    // Down, to a space below the new holder, which receives the posts
    // from then on, as the child that forwarded it up still does, while
    // the parent that unlinked does not.
    let sibling = SpaceSelector::id(&tree.sibling.id);
    let parent = caller(&tree.parent);
    fixture
        .coordination
        .forward_thread(
            &**fixture,
            Some(&parent),
            full(&thread),
            Some(&sibling),
            None,
            true,
        )
        .await
        .unwrap();
    send(fixture, None, &thread, "Any news?").await;
    for linked in [&tree.sibling, &tree.child] {
        let news = alerts(fixture, linked).await;
        assert!(
            news.iter().any(|alert| alert.contains("Any news?")),
            "{news:?}"
        );
    }
    let unlinked = alerts(fixture, &tree.parent).await;
    assert!(
        !unlinked.iter().any(|alert| alert.contains("Any news?")),
        "{unlinked:?}"
    );
}

async fn forward(
    fixture: &Fixture,
    from: Option<&Caller>,
    thread: &Thread,
    to: Option<&Workspace>,
) -> anyhow::Result<Thread> {
    let to = to.map(|workspace| SpaceSelector::id(&workspace.id));
    (fixture.coordination)
        .forward_thread(&**fixture, from, full(thread), to.as_ref(), None, false)
        .await
}

async fn resolve(
    fixture: &Fixture,
    from: Option<&Caller>,
    number: ThreadNumber,
) -> anyhow::Result<Thread> {
    let answered = Resolving {
        outcome: Some(OutcomeKind::Answered),
        ..Resolving::default()
    };
    (fixture.coordination)
        .resolve_thread(&**fixture, from, number, answered)
        .await
}

#[tokio::test]
async fn an_opener_passes_its_role_on_and_the_thread_its_number() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let below = fixture.workspace("below", Some(&tree.child.id)).await;
    let child = caller(&tree.child);
    let thread = open(fixture, Some(&child), Some(&below)).await;
    send(fixture, Some(&child), &thread, "Which port?").await;
    // Never to the holder, which cannot open it too.
    assert!(
        forward(fixture, Some(&child), &thread, Some(&below))
            .await
            .is_err()
    );

    // Up, to the child's coordinator, in whose space it is numbered now.
    let moved = forward(fixture, Some(&child), &thread, None).await.unwrap();
    assert_eq!(moved.opener_id(), Some(&tree.parent.id));
    assert_eq!(moved.number.space, tree.parent.number);
    let told = alerts(fixture, &tree.parent).await;
    assert!(
        told.iter().any(|alert| alert.contains("Which port?")),
        "{told:?}"
    );
    let heard = alerts(fixture, &below).await;
    assert!(
        heard
            .iter()
            .any(|alert| alert.contains(&moved.number.to_string())),
        "{heard:?}"
    );

    // The earlier opener still posts on the opener's side, though its scope
    // reaches the holder, but only the opener now resolves it.
    let earlier = (fixture.coordination)
        .post_to_thread(
            &**fixture,
            Some(&child),
            full(&moved),
            post("8080, if free"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(earlier.last_post().unwrap().side, Some(Side::Opener));
    assert!(resolve(fixture, Some(&child), full(&moved)).await.is_err());
    let resolved = resolve(fixture, Some(&caller(&tree.parent)), full(&moved))
        .await
        .unwrap();
    assert!(!resolved.is_open());
}

#[tokio::test]
async fn the_user_passes_the_opener_s_role_of_its_thread_to_a_space() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let thread = open(fixture, None, Some(&tree.child)).await;
    send(fixture, None, &thread, "Which port?").await;
    // The user has no coordinator to pass it up to.
    assert!(forward(fixture, None, &thread, None).await.is_err());

    let moved = forward(fixture, None, &thread, Some(&tree.sibling))
        .await
        .unwrap();
    assert_eq!(moved.opener, Sender::Space);
    assert_eq!(moved.number.space, tree.sibling.number);
    // The user posts on the side it opened.
    let earlier = send(fixture, None, &moved, "Any news?").await;
    assert_eq!(earlier.last_post().unwrap().side, Some(Side::Opener));
    let news = alerts(fixture, &tree.child).await;
    assert!(
        news.iter().any(|alert| alert.contains("Any news?")),
        "{news:?}"
    );
}

#[tokio::test]
async fn an_earlier_opener_that_holds_the_thread_now_answers_as_its_holder() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let thread = open(fixture, None, Some(&tree.child)).await;
    send(fixture, None, &thread, "Which port?").await;
    let moved = forward(fixture, None, &thread, Some(&tree.sibling))
        .await
        .unwrap();
    // The thread goes up from the child to the parent, then to the user,
    // which opened it before.
    forward(fixture, Some(&caller(&tree.child)), &moved, None)
        .await
        .unwrap();
    let held = forward(fixture, Some(&caller(&tree.parent)), &moved, None)
        .await
        .unwrap();
    assert_eq!(held.holder, shoal_protocol::threads::Holder::User);

    let answered = send(fixture, None, &held, "8080").await;
    assert_eq!(answered.last_post().unwrap().side, Some(Side::Holder));
    assert_eq!(answered.awaits(), Some(Side::Opener));
}

#[tokio::test]
async fn a_thread_the_user_opened_never_goes_back_to_the_user() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let thread = open(fixture, None, Some(&tree.beside)).await;
    send(fixture, None, &thread, "Which port?").await;
    // The space has no coordinator, so passing the thread up would hand it
    // to the user who asked.
    let beside = caller(&tree.beside);
    let up = fixture.coordination.forward_thread(
        &**fixture,
        Some(&beside),
        full(&thread),
        None,
        None,
        false,
    );
    assert!(up.await.is_err());
    let kept = fixture
        .coordination
        .thread(&**fixture, None, full(&thread))
        .await
        .unwrap();
    assert_eq!(
        kept.holder.space().map(|holder| &holder.id),
        Some(&tree.beside.id)
    );
}

#[tokio::test]
async fn a_post_that_would_outgrow_one_response_is_refused_whole() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let thread = open(fixture, None, Some(&tree.beside)).await;
    let long = "x".repeat(shoal_protocol::threads::MAX_POST_LEN);
    let mut posted = 0;
    let refused = loop {
        let post = fixture.coordination.post_to_thread(
            &**fixture,
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
    let kept = fixture
        .coordination
        .thread(&**fixture, None, full(&thread))
        .await
        .unwrap();
    assert_eq!(kept.posts.len(), posted);
    let bytes = serde_json::to_vec(&kept).unwrap().len();
    assert!(
        bytes <= shoal_protocol::threads::MAX_THREAD_BYTES,
        "{bytes}"
    );
}

#[tokio::test]
async fn the_opener_resolves_its_thread_and_takes_back_an_unread_notice() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let (child, parent) = (caller(&tree.child), caller(&tree.parent));
    let thread = open(fixture, Some(&child), None).await;
    send(fixture, Some(&child), &thread, "Postgres or SQLite?").await;
    assert_eq!(alerts(fixture, &tree.parent).await.len(), 1);

    let withdraw = OutcomeKind::Withdrawn;
    let by_holder = fixture
        .coordination
        .resolve_thread(
            &**fixture,
            Some(&parent),
            full(&thread),
            Resolving {
                outcome: Some(withdraw),
                ..Resolving::default()
            },
        )
        .await;
    assert!(by_holder.is_err());
    let withdrawn = fixture
        .coordination
        .resolve_thread(
            &**fixture,
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
    assert!(alerts(fixture, &tree.parent).await.is_empty());
    let resolved = fixture.coordination.post_to_thread(
        &**fixture,
        Some(&child),
        full(&thread),
        post("More"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(resolved.await.is_err());

    let reopened = fixture
        .coordination
        .resolve_thread(
            &**fixture,
            Some(&child),
            full(&thread),
            Resolving::default(),
        )
        .await
        .unwrap();
    assert!(reopened.is_open());
    assert_eq!(
        fixture
            .coordination
            .waiting_here(&tree.child.id)
            .await
            .unwrap(),
        1
    );
}

/// A post asking `text` with the surface `--choice` builds for `choices`.
fn asking(text: &str, choices: &[&str], recommended: Option<&str>) -> NewPost {
    let choices: Vec<String> = choices.iter().map(|choice| (*choice).to_owned()).collect();
    NewPost {
        ui: Some(Surface::choices(&choices, recommended, None)),
        ..post(text)
    }
}

/// Post `post` in `thread` as `caller`.
async fn post_as(
    fixture: &Fixture,
    thread: &Thread,
    caller: &Caller,
    post: NewPost,
) -> anyhow::Result<Thread> {
    (fixture.coordination.post_to_thread(
        &**fixture,
        Some(caller),
        full(thread),
        post,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
    .await
}

/// A post answering the action `action` of the post `on`, or else the
/// latest surface of the other side.
fn answering(action: &str, on: Option<i64>) -> NewPost {
    NewPost {
        answer: Some(Answer {
            post: on,
            action: action.to_owned(),
            ..Answer::default()
        }),
        ..NewPost::default()
    }
}

#[tokio::test]
async fn a_long_post_keeps_the_commands_that_answer_its_surface() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let child = caller(&tree.child);
    let thread = open(fixture, Some(&child), None).await;
    let long = "word ".repeat(shoal_protocol::threads::MAX_POST_LEN / 5 - 1);
    let asked = asking(&long, &["Postgres", "SQLite"], None);
    post_as(fixture, &thread, &child, asked).await.unwrap();
    let delivered = alerts(fixture, &tree.parent).await;
    let alert = delivered.last().unwrap();
    assert!(alert.len() <= crate::validate::MAX_AGENT_MESSAGE_LEN);
    assert!(alert.contains("shows the rest"), "{alert}");
    assert!(alert.contains("--action 2"), "{alert}");
    assert!(alert.contains("thread forward"), "{alert}");
}

#[tokio::test]
async fn an_alert_with_more_actions_than_fit_keeps_its_reply_hint() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let child = caller(&tree.child);
    let thread = open(fixture, Some(&child), None).await;
    let choices: Vec<String> = (0..60)
        .map(|number| format!("{number} {}", "option ".repeat(20)))
        .collect();
    let choices: Vec<&str> = choices.iter().map(String::as_str).collect();
    let asked = asking("Which?", &choices, None);
    post_as(fixture, &thread, &child, asked).await.unwrap();
    let delivered = alerts(fixture, &tree.parent).await;
    let alert = delivered.last().unwrap();
    assert!(alert.len() <= crate::validate::MAX_AGENT_MESSAGE_LEN);
    assert!(alert.contains("--action 1"), "{alert}");
    assert!(!alert.contains("--action 60"), "{alert}");
    assert!(alert.contains("shows the rest"), "{alert}");
    assert!(alert.contains("thread forward"), "{alert}");
}

#[tokio::test]
async fn an_answer_checked_before_a_newer_surface_arrived_is_refused() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let child = caller(&tree.child);
    let thread = open(fixture, Some(&child), None).await;
    let asked = asking("Which?", &["Postgres", "SQLite"], None);
    let first = post_as(fixture, &thread, &child, asked).await.unwrap();
    let first = first.last_post().unwrap().id;
    // The parent checked its answer against the first surface, and the
    // child posted a newer one before the answer was recorded.
    let asked = asking("Which cache?", &["Redis", "None"], None);
    let newer = post_as(fixture, &thread, &child, asked).await.unwrap();
    let poster = super::posting::Poster {
        side: Some(Side::Holder),
        sender: Sender::Space,
        space: Some(shoal_protocol::threads::ThreadSpace::here(
            tree.parent.id.clone(),
        )),
        seen: newer.holder.clone(),
    };
    let stale = NewPost {
        answer: Some(Answer {
            post: Some(first),
            action: "1".into(),
            ..Answer::default()
        }),
        ..post("Postgres")
    };
    let (id, records) = (thread.id, fixture.coordination.records.clone());
    let recorded = fixture.coordination.store.write(move |tx| {
        let attached = super::posting::Attached::default();
        super::posting::record_post(tx, id, &poster, &stale, attached, false, &*records).map(|_| ())
    });
    let refused = recorded.await.unwrap_err();
    assert!(refused.to_string().contains("answer #2"), "{refused:#}");
}

#[tokio::test]
async fn an_answer_takes_an_action_of_the_other_side_s_latest_surface() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let (child, parent) = (caller(&tree.child), caller(&tree.parent));
    let thread = open(fixture, Some(&child), None).await;
    let asked = post_as(
        fixture,
        &thread,
        &child,
        asking("Which database?", &["Postgres", "SQLite"], Some("SQLite")),
    );
    let first = asked.await.unwrap().last_post().unwrap().id;
    let unknown = post_as(fixture, &thread, &parent, answering("3", None))
        .await
        .unwrap_err();
    assert!(unknown.to_string().contains("it has 1, 2"), "{unknown:#}");
    // The opener answers no surface of its own.
    let own = post_as(fixture, &thread, &child, answering("1", None))
        .await
        .unwrap_err();
    assert!(own.to_string().contains("no surface from"), "{own:#}");

    let asked = post_as(
        fixture,
        &thread,
        &child,
        asking("And the cache?", &["Redis", "None"], None),
    );
    let second = asked.await.unwrap().last_post().unwrap().id;
    let older = post_as(fixture, &thread, &parent, answering("2", Some(first)))
        .await
        .unwrap_err();
    assert!(older.to_string().contains("answer #2"), "{older:#}");
    let answered = post_as(fixture, &thread, &parent, answering("1", Some(second)))
        .await
        .unwrap();
    let answer = answered.last_post().unwrap();
    assert_eq!(answer.post.text, "Redis");
    assert_eq!(
        answer.post.answer.as_ref().map(|answer| answer.post),
        Some(Some(second))
    );
}

#[tokio::test]
async fn a_space_that_linked_a_thread_receives_its_new_posts() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let thread = open(fixture, None, Some(&tree.child)).await;
    let watcher = SpaceSelector::id(&tree.beside.id);
    // A space reads the threads below it, so it links them.
    let parent = caller(&tree.parent);
    let beside = caller(&tree.beside);
    let outside =
        fixture
            .coordination
            .link_thread(&**fixture, Some(&beside), &watcher, thread.number, true);
    assert!(outside.await.is_err());
    (fixture.coordination.link_thread(
        &**fixture,
        Some(&parent),
        &SpaceSelector::id(&tree.parent.id),
        thread.number,
        true,
    ))
    .await
    .unwrap();
    (fixture
        .coordination
        .link_thread(&**fixture, None, &watcher, thread.number, true))
    .await
    .unwrap();
    send(fixture, None, &thread, "Which port?").await;
    for linker in [&tree.beside, &tree.parent] {
        let linked = alerts(fixture, linker).await;
        assert!(
            linked.iter().any(|alert| alert.contains("Which port?")),
            "{linked:?}"
        );
    }
    (fixture
        .coordination
        .link_thread(&**fixture, None, &watcher, thread.number, false))
    .await
    .unwrap();
    send(fixture, None, &thread, "Any news?").await;
    let unlinked = alerts(fixture, &tree.beside).await;
    assert!(!unlinked.iter().any(|alert| alert.contains("Any news?")));
}

#[tokio::test]
async fn a_space_that_forwarded_a_question_still_answers_it_for_the_holder() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let (child, parent) = (caller(&tree.child), caller(&tree.parent));
    let thread = open(fixture, Some(&child), None).await;
    let asked = asking("Which database?", &["Postgres", "SQLite"], None);
    (fixture.coordination.post_to_thread(
        &**fixture,
        Some(&child),
        full(&thread),
        asked,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
    .await
    .unwrap();
    // The parent has no coordinator, so the thread goes up to the user.
    (fixture.coordination.forward_thread(
        &**fixture,
        Some(&parent),
        full(&thread),
        None,
        None,
        false,
    ))
    .await
    .unwrap();
    let before = alerts(fixture, &tree.child).await.len();

    let answered = fixture
        .coordination
        .post_to_thread(
            &**fixture,
            Some(&parent),
            full(&thread),
            answering("1", None),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .await
        .unwrap();
    let answer = answered.last_post().unwrap();
    assert_eq!(answer.side, Some(Side::Holder));
    assert_eq!(answer.post.text, "Postgres");
    assert_eq!(answered.awaits(), Some(Side::Opener));
    let replies = alerts(fixture, &tree.child).await;
    assert_eq!(replies.len(), before + 1, "{replies:?}");
    // The user, who holds it, hears that the parent answered for it.
    let told = users(fixture, AlertKind::ThreadPosted).await;
    assert!(
        told.iter().any(|alert| alert.contains("Postgres")),
        "{told:?}"
    );
}

#[tokio::test]
async fn a_participant_posts_to_both_sides_without_taking_a_turn() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    // The space beside has no agent, so a thread of the space below it goes
    // to the user, and the space beside takes neither side.
    let below = fixture.workspace("below", Some(&tree.beside.id)).await;
    fixture.run_agent(&below.id);
    let thread = open(fixture, Some(&caller(&below)), None).await;
    send(fixture, Some(&caller(&below)), &thread, "Which port?").await;
    let beside = caller(&tree.beside);
    let choosing = fixture.coordination.post_to_thread(
        &**fixture,
        Some(&beside),
        full(&thread),
        asking("Pick one", &["8080", "9090"], None),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(choosing.await.is_err());

    let joined = send(fixture, Some(&beside), &thread, "8080 is free here").await;
    assert_eq!(joined.last_post().unwrap().side, None);
    assert!(joined.waits_for_user());
    let heard = alerts(fixture, &below).await;
    assert!(
        heard
            .iter()
            .any(|alert| alert.contains("8080 is free here")),
        "{heard:?}"
    );
    let told = users(fixture, AlertKind::ThreadPosted).await;
    assert!(
        told.iter().any(|alert| alert.contains("8080 is free here")),
        "{told:?}"
    );
    // Posting linked the thread, so the participant hears the reply.
    send(fixture, None, &thread, "Take 8080").await;
    let replies = alerts(fixture, &tree.beside).await;
    assert!(
        replies.iter().any(|alert| alert.contains("Take 8080")),
        "{replies:?}"
    );
}

#[tokio::test]
async fn a_bare_number_names_a_thread_of_the_callers_own_space() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let child = caller(&tree.child);
    let thread = open(fixture, Some(&child), None).await;
    let own = ThreadNumber::Own(thread.number.item);
    assert_eq!(
        fixture
            .coordination
            .thread(&**fixture, Some(&child), own)
            .await
            .unwrap()
            .id,
        thread.id
    );
    assert!(
        fixture
            .coordination
            .thread(&**fixture, None, own)
            .await
            .is_err()
    );
}

/// The thread of a finding of a review Shoal finished of `workspace`,
/// which holds it.
async fn finding(fixture: &Fixture, workspace: &Workspace) -> Thread {
    let id = workspace.id.clone();
    (fixture.store())
        .write(move |tx| {
            tx.execute(
                "INSERT INTO reviews(workspace_id,target,opener,agent,state,created_at)
                    VALUES (?1,'{}','shoal','claude','finished',1)",
                [&id],
            )?;
            let review = ReviewId(tx.last_insert_rowid());
            let thread = super::findings::open(
                tx,
                &super::findings::FindingThread {
                    review,
                    workspace: &id,
                    severity: Severity::ShouldFix,
                    title: "Unclear",
                    body: "Name it.",
                    code: Vec::new(),
                },
            )?;
            super::findings::release_of_review(tx, review)?;
            Ok(super::findings::by_ids(tx, &[thread])?.remove(0))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn an_answer_run_posts_for_shoal_only_in_the_open_thread_it_answers() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let holder = caller(&tree.child);
    let thread = finding(fixture, &tree.child).await;
    send(fixture, Some(&holder), &thread, "Is this reachable?").await;
    let answering = Caller {
        access: Access::Answer(thread.id),
        ..caller(&tree.child)
    };
    let answered = send(fixture, Some(&answering), &thread, "It is, from parse.").await;
    let answer = answered.posts.last().unwrap();
    assert_eq!(
        (answer.side, answer.sender, answer.space.is_none()),
        (Some(Side::Opener), Sender::Shoal, true)
    );
    assert_eq!(alerts(fixture, &tree.child).await.len(), 1);

    let other = open(fixture, Some(&holder), None).await;
    let elsewhere = fixture.coordination.post_to_thread(
        &**fixture,
        Some(&answering),
        full(&other),
        post("Yes."),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(elsewhere.await.is_err());
    let fixed = Resolving {
        outcome: Some(OutcomeKind::Fixed),
        ..Resolving::default()
    };
    (fixture.coordination)
        .resolve_thread(&**fixture, Some(&holder), full(&thread), fixed)
        .await
        .unwrap();
    let resolved = fixture.coordination.post_to_thread(
        &**fixture,
        Some(&answering),
        full(&thread),
        post("Still."),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(resolved.await.is_err());
}

#[tokio::test]
async fn the_user_s_threads_list_past_larger_newer_threads() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let asked = open(fixture, None, Some(&tree.beside)).await;
    send(fixture, Some(&caller(&tree.beside)), &asked, "Which port?").await;
    // Newer threads that wait for spaces fill a response on their own.
    let long = "x".repeat(shoal_protocol::threads::MAX_POST_LEN);
    for space in [&tree.child, &tree.sibling] {
        let thread = open(fixture, None, Some(space)).await;
        while (fixture.coordination.post_to_thread(
            &**fixture,
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
    let all = fixture
        .coordination
        .threads(&**fixture, None, None, open, false)
        .await
        .unwrap();
    assert!(!all.threads.iter().any(|thread| thread.id == asked.id));
    let mine = fixture
        .coordination
        .threads(&**fixture, None, None, open, true)
        .await
        .unwrap();
    let numbers: Vec<_> = mine.threads.iter().map(|thread| thread.number).collect();
    assert_eq!(numbers, [asked.number]);
}

#[tokio::test]
async fn a_thread_that_holds_its_posts_reaches_nobody_and_its_holder_takes_no_side() {
    let tree = tree().await;
    let fixture = &tree.fixture;
    let thread = open(fixture, None, Some(&tree.child)).await;
    let id = thread.id;
    fixture
        .store()
        .write(move |tx| Ok(tx.execute("UPDATE threads SET held=1 WHERE id=?1", [id])?))
        .await
        .unwrap();
    let posted = send(fixture, None, &thread, "Call it retries").await;
    assert!(posted.held);
    assert!(alerts(fixture, &tree.child).await.is_empty());
    let child = caller(&tree.child);
    let replied = fixture.coordination.post_to_thread(
        &**fixture,
        Some(&child),
        full(&thread),
        post("Done"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(replied.await.is_err());
}

#[test]
fn a_thread_larger_than_a_frame_stays_listed_with_older_ones() {
    let thread = |id: i64, posts: usize| -> Thread {
        let choices: Vec<String> = (1..=20).map(|number| format!("choice {number}")).collect();
        let post = |post: usize| {
            serde_json::json!({"id": post, "created_at": 0, "side": "holder", "sender": "space",
                "text": "Which?", "ui": Surface::choices(&choices, None, None)})
        };
        serde_json::from_value(serde_json::json!({
            "id": id, "space_number": 2, "item": id, "space_id": "w1", "space": "cache",
            "kind": "question", "name": "Which?", "opener": "space",
            "holder": {"kind": "user"}, "created_at": 0,
            "posts": (1..=posts).map(post).collect::<Vec<_>>(),
        }))
        .unwrap()
    };
    let migrated = thread(3, 40);
    assert!(serde_json::to_vec(&migrated).unwrap().len() > crate::protocol::MAX_FRAME);
    let listing =
        super::reading::newest_that_fit(vec![thread(1, 0), thread(2, 0), migrated]).unwrap();
    let ids: Vec<i64> = listing.threads.iter().map(|thread| thread.id).collect();
    assert_eq!((ids, listing.omitted), (vec![1, 2, 3], 0));
}
