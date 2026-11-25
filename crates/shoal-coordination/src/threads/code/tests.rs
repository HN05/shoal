use std::{fs, path::PathBuf};

use shoal_protocol::{
    code::{CodeState, Lines, NewCodeRef},
    model::Workspace,
    threads::{ThreadKind, ThreadNumber, ThreadOpening},
};

use crate::threads::tests::{Tree, alerts, caller, full, open, post, tree};

const PARSER: &str = "fn parse() {\n    tokens()\n}\n\nfn tokens() {}\n";

/// `parse.rs` written into `workspace`'s worktree; its path.
fn parser(workspace: &Workspace) -> PathBuf {
    let path = workspace.path.join("parse.rs");
    fs::write(&path, PARSER).unwrap();
    path
}

fn at(path: PathBuf, lines: &str) -> NewCodeRef {
    NewCodeRef {
        path,
        lines: Some(lines.parse().unwrap()),
        contents: None,
    }
}

/// Open a thread from the child about `lines` of its parser; its number.
async fn about_parser(tree: &Tree, lines: &str) -> ThreadNumber {
    let (fixture, child) = (&tree.fixture, caller(&tree.child));
    let thread = open(fixture, Some(&child), None).await;
    let code = vec![at(parser(&tree.child), lines)];
    (fixture.coordination.post_to_thread(
        &**fixture,
        Some(&child),
        full(&thread),
        post("Here"),
        Vec::new(),
        code,
        Vec::new(),
    ))
    .await
    .unwrap();
    full(&thread)
}

/// Where the first reference of the thread `number` is now, and its state.
async fn followed(tree: &Tree, number: ThreadNumber) -> (String, CodeState) {
    let fixture = &tree.fixture;
    let thread = fixture.coordination.thread(&**fixture, None, number);
    let thread = thread.await.unwrap();
    let code = &thread.posts[0].code[0];
    (code.latest.to_string(), code.state)
}

#[tokio::test]
async fn code_outside_the_caller_s_scope_is_refused() {
    let tree = tree().await;
    let (fixture, child) = (&tree.fixture, caller(&tree.child));
    let thread = open(fixture, Some(&child), None).await;
    let elsewhere = vec![at(parser(&tree.beside), "1-3")];
    let posted = fixture
        .coordination
        .post_to_thread(
            &**fixture,
            Some(&child),
            full(&thread),
            post("Look"),
            Vec::new(),
            elsewhere,
            Vec::new(),
        )
        .await;
    let refused = posted.unwrap_err().to_string();
    assert!(refused.contains("does not reach"), "{refused}");
    let past_the_end = vec![at(parser(&tree.child), "4-9")];
    let posted = fixture
        .coordination
        .post_to_thread(
            &**fixture,
            Some(&child),
            full(&thread),
            post("Look"),
            Vec::new(),
            past_the_end,
            Vec::new(),
        )
        .await;
    let refused = posted.unwrap_err().to_string();
    assert!(refused.contains("has 5 lines"), "{refused}");
    // A client sends lines as numbers, which may run backwards.
    let backwards = vec![NewCodeRef {
        lines: Some(Lines { start: 3, end: 1 }),
        ..at(parser(&tree.child), "1")
    }];
    let posted = fixture
        .coordination
        .post_to_thread(
            &**fixture,
            Some(&child),
            full(&thread),
            post("Look"),
            Vec::new(),
            backwards,
            Vec::new(),
        )
        .await;
    let refused = posted.unwrap_err().to_string();
    assert!(refused.contains("end before they start"), "{refused}");
}

#[tokio::test]
async fn a_thread_opened_about_code_posts_it_at_once_from_a_buffer() {
    let tree = tree().await;
    let (fixture, parent) = (&tree.fixture, caller(&tree.parent));
    let unsaved = NewCodeRef {
        path: tree.child.path.join("lexer.rs"),
        lines: Some("2".parse().unwrap()),
        contents: Some("fn lex() {\n    todo!()\n}\n".into()),
    };
    let to = shoal_protocol::space_names::SpaceSelector::id(&tree.child.id);
    let opened = fixture
        .coordination
        .open_thread(
            &**fixture,
            Some(&parent),
            ThreadOpening {
                code: vec![unsaved],
                ..ThreadOpening::named("Implement lex")
            },
            Some(ThreadKind::Request),
            Some(&to),
        )
        .await
        .unwrap();
    let [opening] = &opened.posts[..] else {
        panic!("{:?}", opened.posts)
    };
    assert_eq!(opening.post.text, "Implement lex");
    assert_eq!(opening.code[0].snippet.as_deref(), Some("    todo!()"));
    // The holder's own worktree needs no space named.
    let received = alerts(fixture, &tree.child).await;
    assert!(received[0].contains("Code: lexer.rs:2\n"), "{received:?}");
    let shown = fixture
        .coordination
        .thread(&**fixture, None, ThreadNumber::Full(opened.number));
    assert_eq!(shown.await.unwrap().posts[0].code.len(), 1);
}

#[tokio::test]
async fn a_reference_follows_its_code_through_edits_step_by_step() {
    let tree = tree().await;
    let number = about_parser(&tree, "5").await;
    let file = tree.child.path.join("parse.rs");
    fs::write(&file, format!("use x;\n\n{PARSER}")).unwrap();
    let moved = ("parse.rs:7".to_owned(), CodeState::Unchanged);
    assert_eq!(followed(&tree, number).await, moved);

    fs::write(
        &file,
        format!("use x;\n\n{PARSER}").replace("fn tokens() {}", "fn tokens() {\n}"),
    )
    .unwrap();
    let edited = ("parse.rs:7-8".to_owned(), CodeState::Changed);
    assert_eq!(followed(&tree, number).await, edited);

    fs::write(&file, "use x;\n").unwrap();
    let (_, state) = followed(&tree, number).await;
    assert_eq!(state, CodeState::Outdated);
    let thread = tree
        .fixture
        .coordination
        .thread(&*tree.fixture, None, number)
        .await
        .unwrap();
    assert_eq!(
        thread.posts[0].code[0].snippet.as_deref(),
        Some("fn tokens() {}")
    );
}

#[tokio::test]
async fn a_reference_that_cannot_be_followed_leaves_the_others_to_follow() {
    let tree = tree().await;
    let (fixture, child) = (&tree.fixture, caller(&tree.child));
    let thread = open(fixture, Some(&child), None).await;
    let lexer = tree.child.path.join("lexer.rs");
    fs::write(&lexer, "fn lex() {}\n").unwrap();
    let code = vec![at(lexer.clone(), "1"), at(parser(&tree.child), "5")];
    let posted = fixture
        .coordination
        .post_to_thread(
            &**fixture,
            Some(&child),
            full(&thread),
            post("Here"),
            Vec::new(),
            code,
            Vec::new(),
        )
        .await
        .unwrap();
    // The version the first was last followed to is gone from Git.
    let broken = posted.posts[0].code[0].id;
    fixture
        .store()
        .write(move |tx| {
            let missing = "0123456789abcdef0123456789abcdef01234567";
            let sql = "UPDATE thread_code SET latest_blob=?2 WHERE id=?1";
            Ok(tx.execute(sql, rusqlite::params![broken, missing])?)
        })
        .await
        .unwrap();
    fs::write(&lexer, "// Lexing\nfn lex() {}\n").unwrap();
    let file = tree.child.path.join("parse.rs");
    fs::write(&file, format!("use x;\n{PARSER}")).unwrap();

    let thread = fixture
        .coordination
        .thread(&**fixture, None, full(&thread))
        .await
        .unwrap();
    let [lexed, parsed] = &thread.posts[0].code[..] else {
        panic!("{:?}", thread.posts[0].code)
    };
    assert_eq!(lexed.latest.to_string(), "lexer.rs:1");
    assert_eq!(parsed.latest.to_string(), "parse.rs:6");
}

#[tokio::test]
async fn a_thread_whose_opening_post_is_refused_is_never_opened() {
    let tree = tree().await;
    let (fixture, child) = (&tree.fixture, caller(&tree.child));
    let long = tree.child.path.join("long.rs");
    fs::write(&long, format!("// {}\n", "x".repeat(97)).repeat(20)).unwrap();
    // Each snippet takes 2 KiB, and together they outgrow a thread.
    let code = (0..40).map(|_| at(long.clone(), "1-20")).collect();
    let opened = fixture
        .coordination
        .open_thread(
            &**fixture,
            Some(&child),
            ThreadOpening {
                code,
                ..ThreadOpening::named("Too much")
            },
            Some(ThreadKind::Request),
            None,
        )
        .await;
    let refused = opened.unwrap_err().to_string();
    assert!(refused.contains("would take more than"), "{refused}");
    let selector = shoal_protocol::space_names::SpaceSelector::id(&tree.child.id);
    let state = shoal_protocol::threads::ThreadState::All;
    let listed = fixture
        .coordination
        .threads(&**fixture, None, Some(&selector), state, false)
        .await
        .unwrap();
    assert!(listed.threads.is_empty(), "{:?}", listed.threads);
}

#[tokio::test]
async fn a_listing_follows_only_the_threads_it_returns() {
    let tree = tree().await;
    // The child's coordinator holds it, so it is not the user's turn.
    let number = about_parser(&tree, "5").await;
    let file = tree.child.path.join("parse.rs");
    fs::write(&file, format!("use x;\n{PARSER}")).unwrap();
    let state = shoal_protocol::threads::ThreadState::Open;
    let for_user = tree
        .fixture
        .coordination
        .threads(&*tree.fixture, None, None, state, true)
        .await
        .unwrap();
    assert!(for_user.threads.is_empty());
    let recorded = tree.fixture.coordination.open_threads().await.unwrap();
    assert_eq!(
        recorded[0].posts[0].code[0].latest.to_string(),
        "parse.rs:5"
    );
    let listed = tree
        .fixture
        .coordination
        .threads(&*tree.fixture, None, None, state, false)
        .await
        .unwrap();
    assert_eq!(
        listed.threads[0].posts[0].code[0].latest.to_string(),
        "parse.rs:6"
    );
    assert_eq!(followed(&tree, number).await.0, "parse.rs:6");
}
