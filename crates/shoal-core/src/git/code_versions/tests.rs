use std::fs;

use super::*;
use crate::test_support::{git, repository_with};

const PARSER: &str =
    "fn parse() {\n    let input = read();\n    tokens(input)\n}\n\nfn tokens() {}\n";

#[tokio::test]
async fn a_written_version_reads_back_and_hashes_as_its_file() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository_with(root.path(), "repo", &[("parse.rs", PARSER)]);
    let blob = write(&repo, "edited in a buffer\n").await.unwrap();
    assert_eq!(read(&repo, &blob).await.unwrap(), "edited in a buffer\n");
    fs::write(repo.join("buffer.rs"), "edited in a buffer\n").unwrap();
    let hashed = hash(&repo, &["parse.rs", "buffer.rs"]).await.unwrap();
    let committed = git(&repo, &["rev-parse", "HEAD:parse.rs"]);
    assert_eq!(hashed, [committed.trim(), blob.as_str()]);
}

#[tokio::test]
async fn a_space_s_ref_keeps_exactly_the_versions_its_records_keep() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository_with(root.path(), "repo", &[("parse.rs", PARSER)]);
    let reference = reference("w1");
    let (first, second) = (
        write(&repo, "first\n").await.unwrap(),
        write(&repo, "second\n").await.unwrap(),
    );
    keep(&repo, &reference, &[first.clone(), second.clone()])
        .await
        .unwrap();
    let kept = git(&repo, &["ls-tree", "--object-only", &reference]);
    let mut expected = vec![first.clone(), second];
    expected.sort();
    assert_eq!(kept.lines().collect::<Vec<_>>(), expected);

    keep(&repo, &reference, std::slice::from_ref(&first))
        .await
        .unwrap();
    let kept = git(&repo, &["ls-tree", "--object-only", &reference]);
    assert_eq!(kept.trim(), first);

    keep(&repo, &reference, &[]).await.unwrap();
    assert!(!crate::git::ref_exists(&repo, &reference).await.unwrap());
}

#[tokio::test]
async fn a_file_renamed_whole_is_found_where_it_went_unsaved() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository_with(root.path(), "repo", &[("parse.rs", PARSER)]);
    let head: CommitId = git(&repo, &["rev-parse", "HEAD"]).trim().into();
    // The version a reference keeps differs from the commit's.
    let edited = format!("{PARSER}\nfn more() {{}}\n");
    let blob = write(&repo, edited.as_str()).await.unwrap();
    fs::remove_file(repo.join("parse.rs")).unwrap();
    fs::create_dir(repo.join("syntax")).unwrap();
    fs::write(repo.join("syntax/parser.rs"), format!("{edited}// moved\n")).unwrap();

    let found = renamed(&repo, &head, "parse.rs", &blob).await.unwrap();
    assert_eq!(found.as_deref(), Some("syntax/parser.rs"));
    // A deleted file whose code went nowhere whole was renamed to nothing.
    let lexer = write(&repo, "fn lex() {}\n").await.unwrap();
    let gone = renamed(&repo, &head, "lexer.rs", &lexer).await.unwrap();
    assert_eq!(gone, None);
}
