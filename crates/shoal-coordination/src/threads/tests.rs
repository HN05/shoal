use shoal_protocol::{
    space_names::SpaceSelector,
    threads::{
        NewPost, Outcome, OutcomeKind, Resolving, Sender, Side, Thread, ThreadKind, ThreadNumber,
    },
};

use crate::{alerts::AlertKind, model::Workspace, scope::Caller, stand_in::Fixture};

/// A workspace `parent` whose agent runs, two workspaces below it whose
/// agents run, and a workspace beside it without one.
struct Tree {
    manager: Fixture,
    parent: Workspace,
    child: Workspace,
    sibling: Workspace,
    beside: Workspace,
}

async fn tree() -> Tree {
    let manager = Fixture::new().await;
    let parent = manager.workspace("parent", None).await;
    let child = manager.workspace("child", Some(&parent.id)).await;
    let sibling = manager.workspace("sibling", Some(&parent.id)).await;
    let beside = manager.workspace("beside", None).await;
    // A space's agent asks its coordinator; without one, whoever acted
    // there asks the user.
    for agent in [&parent, &child, &sibling] {
        manager.run_agent(&agent.id);
    }
    Tree {
        manager,
        parent,
        child,
        sibling,
        beside,
    }
}

fn caller(workspace: &Workspace) -> Caller {
    Fixture::caller(&workspace.id)
}

fn post(text: &str) -> NewPost {
    NewPost {
        text: text.into(),
        ..NewPost::default()
    }
}

fn full(thread: &Thread) -> ThreadNumber {
    ThreadNumber::Full(thread.number)
}

async fn open(manager: &Fixture, from: Option<&Caller>, to: Option<&Workspace>) -> Thread {
    let to = to.map(|workspace| SpaceSelector::id(&workspace.id));
    manager
        .coordination
        .open_thread(
            &**manager,
            from,
            "Which database?".into(),
            ThreadKind::Question,
            to.as_ref(),
            Vec::new(),
        )
        .await
        .unwrap()
}

async fn send(manager: &Fixture, from: Option<&Caller>, thread: &Thread, text: &str) -> Thread {
    manager
        .coordination
        .post_to_thread(
            &**manager,
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
async fn alerts(manager: &Fixture, workspace: &Workspace) -> Vec<String> {
    (manager.queued(&workspace.id).await.into_iter())
        .filter(|message| message.contains("hread"))
        .collect()
}

/// The user's unread alerts of `kind`.
async fn users(manager: &Fixture, kind: AlertKind) -> Vec<String> {
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
    assert!(
        !manager
            .coordination
            .has_waiting_threads(&tree.child.id)
            .await
            .unwrap()
    );

    send(manager, Some(&child), &thread, "Postgres or SQLite?").await;
    let received = alerts(manager, &tree.parent).await;
    assert_eq!(received.len(), 1, "{received:?}");
    assert!(received[0].contains(&thread.number.to_string()));
    assert!(
        manager
            .coordination
            .has_waiting_threads(&tree.child.id)
            .await
            .unwrap()
    );

    let answered = send(manager, Some(&caller(&tree.parent)), &thread, "SQLite").await;
    assert_eq!(answered.awaits(), Some(Side::Opener));
    assert!(
        !manager
            .coordination
            .has_waiting_threads(&tree.child.id)
            .await
            .unwrap()
    );
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
            .coordination
            .thread(&**manager, Some(&outsider), full(&thread))
            .await
            .is_err()
    );
    let posted = manager
        .coordination
        .post_to_thread(
            &**manager,
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
        .coordination
        .open_thread(
            &*tree.manager,
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
        .coordination
        .open_thread(
            &*tree.manager,
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
        .coordination
        .forward_thread(
            &**manager,
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
    assert!(
        manager
            .coordination
            .thread(&**manager, Some(&child), full(&thread))
            .await
            .is_ok()
    );
    let again =
        manager
            .coordination
            .forward_thread(&**manager, Some(&child), full(&thread), None, None);
    assert!(again.await.is_err());

    // Down, to a space below the new holder, which receives the posts
    // from then on.
    let sibling = SpaceSelector::id(&tree.sibling.id);
    let parent = caller(&tree.parent);
    manager
        .coordination
        .forward_thread(
            &**manager,
            Some(&parent),
            full(&thread),
            Some(&sibling),
            None,
        )
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
    let up =
        manager
            .coordination
            .forward_thread(&**manager, Some(&beside), full(&thread), None, None);
    assert!(up.await.is_err());
    let kept = manager
        .coordination
        .thread(&**manager, None, full(&thread))
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
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.beside)).await;
    let long = "x".repeat(shoal_protocol::threads::MAX_POST_LEN);
    let mut posted = 0;
    let refused = loop {
        let post = manager.coordination.post_to_thread(
            &**manager,
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
    let kept = manager
        .coordination
        .thread(&**manager, None, full(&thread))
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
    let manager = &tree.manager;
    let (child, parent) = (caller(&tree.child), caller(&tree.parent));
    let thread = open(manager, Some(&child), None).await;
    send(manager, Some(&child), &thread, "Postgres or SQLite?").await;
    assert_eq!(alerts(manager, &tree.parent).await.len(), 1);

    let withdraw = OutcomeKind::Withdrawn;
    let by_holder = manager
        .coordination
        .resolve_thread(
            &**manager,
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
        .coordination
        .resolve_thread(
            &**manager,
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
    let resolved = manager.coordination.post_to_thread(
        &**manager,
        Some(&child),
        full(&thread),
        post("More"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(resolved.await.is_err());

    let reopened = manager
        .coordination
        .resolve_thread(
            &**manager,
            Some(&child),
            full(&thread),
            Resolving::default(),
        )
        .await
        .unwrap();
    assert!(reopened.is_open());
    assert!(
        manager
            .coordination
            .has_waiting_threads(&tree.child.id)
            .await
            .unwrap()
    );
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
    (manager.coordination.post_to_thread(
        &**manager,
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
    let outside = manager.coordination.post_to_thread(
        &**manager,
        Some(&parent),
        full(&thread),
        choose(3),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(outside.await.is_err());
    let chose = manager
        .coordination
        .post_to_thread(
            &**manager,
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
    let own = manager.coordination.post_to_thread(
        &**manager,
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
async fn a_space_that_linked_a_thread_receives_its_new_posts() {
    let tree = tree().await;
    let manager = &tree.manager;
    let thread = open(manager, None, Some(&tree.child)).await;
    let watcher = SpaceSelector::id(&tree.beside.id);
    // A space reads the threads below it, so it links them.
    let parent = caller(&tree.parent);
    let beside = caller(&tree.beside);
    let outside =
        manager
            .coordination
            .link_thread(&**manager, Some(&beside), &watcher, thread.number, true);
    assert!(outside.await.is_err());
    (manager.coordination.link_thread(
        &**manager,
        Some(&parent),
        &SpaceSelector::id(&tree.parent.id),
        thread.number,
        true,
    ))
    .await
    .unwrap();
    (manager
        .coordination
        .link_thread(&**manager, None, &watcher, thread.number, true))
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
    (manager
        .coordination
        .link_thread(&**manager, None, &watcher, thread.number, false))
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
        manager
            .coordination
            .thread(&**manager, Some(&child), own)
            .await
            .unwrap()
            .id,
        thread.id
    );
    assert!(
        manager
            .coordination
            .thread(&**manager, None, own)
            .await
            .is_err()
    );
}
