use std::fs;

use super::*;
use crate::test_support::{git, repository_with};

const PARSER: &str = "fn parse() {\n    tokens()\n}\n";

#[tokio::test]
async fn the_change_between_snapshots_is_each_file_s_hunks_or_its_kind() {
    let root = tempfile::tempdir().unwrap();
    let files = [
        ("parse.rs", PARSER),
        ("gone.rs", "fn gone() {}\n"),
        (".gitattributes", "*.lock linguist-generated\n"),
    ];
    let repo = repository_with(root.path(), "repo", &files);
    let before = snapshot(&repo).await.unwrap();
    fs::write(repo.join("parse.rs"), "fn parse() {\n    lex()\n}\n").unwrap();
    fs::remove_file(repo.join("gone.rs")).unwrap();
    fs::write(repo.join("new.rs"), "fn new() {}\n").unwrap();
    fs::write(repo.join("Cargo.lock"), "version = 4\n").unwrap();
    fs::write(repo.join("image.png"), b"\x89PNG\0\0").unwrap();
    // Unsaved files Git ignores stay out of a snapshot.
    fs::write(repo.join(".git/info/exclude"), "target/\n").unwrap();
    fs::create_dir(repo.join("target")).unwrap();
    fs::write(repo.join("target/out"), "built\n").unwrap();
    let after = snapshot(&repo).await.unwrap();

    let changed = changed(&repo, &before, &after).await.unwrap();
    let summary: Vec<(&str, ChangeKind, Vec<String>)> = (changed.iter())
        .map(|file| {
            let patches = file.hunks.iter().map(Hunk::patch).collect();
            (file.path.as_str(), file.kind, patches)
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("Cargo.lock", ChangeKind::Generated, vec![]),
            (
                "gone.rs",
                ChangeKind::Text,
                vec!["@@ -1 +0,0 @@\n-fn gone() {}\n".into()]
            ),
            ("image.png", ChangeKind::Binary, vec![]),
            (
                "new.rs",
                ChangeKind::Text,
                vec!["@@ -0,0 +1 @@\n+fn new() {}\n".into()]
            ),
            (
                "parse.rs",
                ChangeKind::Text,
                vec!["@@ -2 +2 @@\n-    tokens()\n+    lex()\n".into()]
            ),
        ]
    );
    let new = changed.iter().find(|file| file.path == "new.rs").unwrap();
    let hashed = git(&repo, &["hash-object", "new.rs"]);
    assert_eq!(new.blob.as_deref(), Some(hashed.trim()));
    let gone = changed.iter().find(|file| file.path == "gone.rs").unwrap();
    assert_eq!(gone.blob, None);
    // A snapshot leaves the worktree's own index as it was.
    assert_eq!(
        git(&repo, &["status", "--porcelain", "--", "new.rs"]),
        "?? new.rs\n"
    );
}

#[tokio::test]
async fn a_snapshot_takes_a_file_s_version_an_edit_tool_wrote() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository_with(root.path(), "repo", &[("parse.rs", PARSER)]);
    let before = snapshot(&repo).await.unwrap();
    let blob = super::super::code_versions::write(&repo, "fn parse() {}\n")
        .await
        .unwrap();
    let edited = with_version(&repo, &before, "parse.rs", Some(&blob))
        .await
        .unwrap();
    fs::write(repo.join("parse.rs"), "fn parse() {}\n").unwrap();
    let now = snapshot(&repo).await.unwrap();
    assert!(changed(&repo, &edited, &now).await.unwrap().is_empty());
    let removed = with_version(&repo, &edited, "parse.rs", None)
        .await
        .unwrap();
    let back = changed(&repo, &removed, &now).await.unwrap();
    assert_eq!(back[0].path, "parse.rs");
}
