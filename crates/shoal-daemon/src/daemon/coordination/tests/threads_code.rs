use std::{fs, path::PathBuf};

use shoal_protocol::{
    code::{CodeState, NewCodeRef},
    threads::ThreadNumber,
};

use super::threads::{alerts, caller, full, open, post, tree};
use crate::{git, model::Workspace};

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

#[tokio::test]
async fn a_post_s_code_reaches_the_other_side_at_its_lines_with_its_snippet() {
    let tree = tree().await;
    let (manager, child) = (&tree.manager, caller(&tree.child));
    let thread = open(manager, Some(&child), None).await;
    let code = vec![at(parser(&tree.child), "1-3")];
    let posted = manager
        .coordination
        .post_to_thread(
            &**manager,
            Some(&child),
            full(&thread),
            post("Rename it"),
            Vec::new(),
            code,
            Vec::new(),
        )
        .await
        .unwrap();

    let code = &posted.posts[0].code[0];
    assert_eq!(code.space.id, tree.child.id);
    assert_eq!(code.latest.to_string(), "parse.rs:1-3");
    assert_eq!(
        code.snippet.as_deref(),
        Some("fn parse() {\n    tokens()\n}")
    );
    assert_eq!(code.state, CodeState::Unchanged);
    let received = alerts(manager, &tree.parent).await;
    let code_line = format!("Code: parse.rs:1-3 in {}", code.space.label());
    assert!(received[0].contains(&code_line), "{received:?}");
    assert!(received[0].contains("\n        tokens()"), "{received:?}");

    // The space's ref keeps the version the reference points at.
    let repository = tree.child.repository_id.clone().unwrap();
    let repo = manager.repositories.repository(&repository).await.unwrap();
    let reference = git::code_versions::reference(tree.child.id.as_str());
    let kept = git::code_versions::kept(&repo.path, &reference)
        .await
        .unwrap();
    assert_eq!(kept, [code.original.blob.clone().unwrap()]);
}

#[tokio::test]
async fn expiry_lets_go_of_the_versions_of_threads_that_went() {
    let tree = tree().await;
    let (manager, child) = (&tree.manager, caller(&tree.child));
    let thread = open(manager, Some(&child), None).await;
    let code = vec![at(parser(&tree.child), "1-3")];
    (manager.coordination.post_to_thread(
        &**manager,
        Some(&child),
        full(&thread),
        post("Here"),
        Vec::new(),
        code,
        Vec::new(),
    ))
    .await
    .unwrap();
    let repository = tree.child.repository_id.clone().unwrap();
    let repo = manager.repositories.repository(&repository).await.unwrap();
    let reference = git::code_versions::reference(tree.child.id.as_str());

    let id = thread.id;
    (manager.store)
        .write(move |tx| Ok(tx.execute("DELETE FROM threads WHERE id=?1", [id])?))
        .await
        .unwrap();
    manager.spaces.expire_checkpoints(&**manager).await.unwrap();
    let kept = git::code_versions::kept(&repo.path, &reference)
        .await
        .unwrap();
    assert!(kept.is_empty(), "{kept:?}");
}

/// A thread from the child to its coordinator whose post points at lines
/// `lines` of `parse.rs` in the child's worktree.
async fn about_parser(tree: &super::threads::Tree, lines: &str) -> ThreadNumber {
    let (manager, child) = (&tree.manager, caller(&tree.child));
    let thread = open(manager, Some(&child), None).await;
    let code = vec![at(parser(&tree.child), lines)];
    (manager.coordination.post_to_thread(
        &**manager,
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
async fn followed(tree: &super::threads::Tree, number: ThreadNumber) -> (String, CodeState) {
    let thread = tree
        .manager
        .coordination
        .thread(&*tree.manager, None, number)
        .await
        .unwrap();
    let code = &thread.posts[0].code[0];
    (code.latest.to_string(), code.state)
}

#[tokio::test]
async fn a_reference_follows_its_file_renamed_whole() {
    let tree = tree().await;
    let number = about_parser(&tree, "1-3").await;
    fs::create_dir(tree.child.path.join("syntax")).unwrap();
    let moved = format!("// The parser\n{PARSER}");
    fs::write(tree.child.path.join("syntax/parser.rs"), moved).unwrap();
    fs::remove_file(tree.child.path.join("parse.rs")).unwrap();
    let renamed = ("syntax/parser.rs:2-4".to_owned(), CodeState::Unchanged);
    assert_eq!(followed(&tree, number).await, renamed);
}

#[tokio::test]
async fn a_chat_points_at_code_in_the_worktree_it_runs_in() {
    let tree = tree().await;
    let manager = &tree.manager;
    let new = crate::daemon::chats::NewChat {
        priority: None,
        name: "reader".into(),
        agent: "claude".into(),
        mode: "chat".into(),
        narrowing: Default::default(),
        origin: None,
        parent: None,
    };
    let beside = shoal_protocol::space_names::SpaceSelector::id(&tree.beside.id);
    let chat = manager
        .spaces
        .start_new_chat(&**manager, None, Some(&beside), new)
        .await
        .unwrap();
    let reader = crate::daemon::test_support::caller_in(
        &shoal_protocol::space_names::SpaceSelector::id(&chat.id),
    );
    let thread = open(manager, Some(&reader), None).await;
    let code = vec![at(parser(&tree.beside), "1-3")];
    let posted = manager
        .coordination
        .post_to_thread(
            &**manager,
            Some(&reader),
            full(&thread),
            post("This?"),
            Vec::new(),
            code,
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(posted.posts[0].code[0].space.id, tree.beside.id);
}
