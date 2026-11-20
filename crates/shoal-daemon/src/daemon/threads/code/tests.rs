use std::{fs, path::PathBuf};

use shoal_protocol::{
    code::{CodeState, NewCodeRef},
    threads::{ThreadKind, ThreadNumber},
};

use super::super::tests::{caller, full, open, post, tree};
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
        .post_to_thread(
            Some(&child),
            full(&thread),
            post("Rename it"),
            Vec::new(),
            code,
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

    // The space's ref keeps the version the reference points at.
    let repository = tree.child.repository_id.clone().unwrap();
    let repo = manager.repository(&repository).await.unwrap();
    let reference = git::code_versions::reference(tree.child.id.as_str());
    let kept = git::code_versions::kept(&repo.path, &reference)
        .await
        .unwrap();
    assert_eq!(kept, [code.original.blob.clone().unwrap()]);
}

#[tokio::test]
async fn code_outside_the_caller_s_scope_is_refused() {
    let tree = tree().await;
    let (manager, child) = (&tree.manager, caller(&tree.child));
    let thread = open(manager, Some(&child), None).await;
    let elsewhere = vec![at(parser(&tree.beside), "1-3")];
    let posted = manager
        .post_to_thread(
            Some(&child),
            full(&thread),
            post("Look"),
            Vec::new(),
            elsewhere,
        )
        .await;
    let refused = posted.unwrap_err().to_string();
    assert!(refused.contains("does not reach"), "{refused}");
    let past_the_end = vec![at(parser(&tree.child), "4-9")];
    let posted = manager
        .post_to_thread(
            Some(&child),
            full(&thread),
            post("Look"),
            Vec::new(),
            past_the_end,
        )
        .await;
    let refused = posted.unwrap_err().to_string();
    assert!(refused.contains("has 5 lines"), "{refused}");
}

#[tokio::test]
async fn a_thread_opened_about_code_posts_it_at_once_from_a_buffer() {
    let tree = tree().await;
    let (manager, parent) = (&tree.manager, caller(&tree.parent));
    let unsaved = NewCodeRef {
        path: tree.child.path.join("lexer.rs"),
        lines: Some("2".parse().unwrap()),
        contents: Some("fn lex() {\n    todo!()\n}\n".into()),
    };
    let to = shoal_protocol::space_names::SpaceSelector::id(&tree.child.id);
    let opened = manager
        .open_thread(
            Some(&parent),
            "Implement lex".into(),
            ThreadKind::Request,
            Some(&to),
            vec![unsaved],
        )
        .await
        .unwrap();
    let [opening] = &opened.posts[..] else {
        panic!("{:?}", opened.posts)
    };
    assert_eq!(opening.post.text, "Implement lex");
    assert_eq!(opening.code[0].snippet.as_deref(), Some("    todo!()"));
    let shown = manager.thread(None, ThreadNumber::Full(opened.number));
    assert_eq!(shown.await.unwrap().posts[0].code.len(), 1);
}
