use std::path::{Path, PathBuf};

use crate::git::{has_commit, ref_exists, resolve_commit, revision, run, verify_revision};
use crate::test_support::{git, reftable_repository, repository};

/// The ways a test repository keeps its refs: in files, which gix reads,
/// and in a reftable, which Git reads for it.
type Create = fn(&Path, &str) -> PathBuf;

/// Lookups of refs in a repository `create` makes.
async fn ref_reads(create: Create) {
    let root = tempfile::tempdir().unwrap();
    let repo = create(root.path(), "repo");
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).trim().to_owned();
    git(&repo, &["branch", "topic/nested"]);
    git(&repo, &["branch", "topical"]);
    git(&repo, &["tag", "-a", "-m", "v1", "v1"]);
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/gone",
            "refs/remotes/origin/missing",
        ],
    );

    assert!(ref_exists(&repo, "refs/heads/topical").await.unwrap());
    // Neither a ref's parent component nor a short name is a ref.
    assert!(!ref_exists(&repo, "refs/heads/topic").await.unwrap());
    assert!(!ref_exists(&repo, "topical").await.unwrap());
    assert!(!ref_exists(&repo, "refs/heads/topical~1").await.unwrap());

    for rev in [
        "HEAD",
        "refs/heads/topical",
        "refs/tags/v1",
        "refs/remotes/origin/HEAD",
        &head,
    ] {
        assert_eq!(
            resolve_commit(&repo, rev, run).await.unwrap(),
            head.as_str(),
            "{rev}"
        );
        assert!(has_commit(&repo, rev).await.unwrap(), "{rev}");
    }
    for rev in ["refs/heads/missing", tree.as_str(), &"0".repeat(head.len())] {
        assert!(resolve_commit(&repo, rev, run).await.is_err(), "{rev}");
        assert!(!has_commit(&repo, rev).await.unwrap(), "{rev}");
    }

    // A revision names its object unpeeled: an annotated tag, not its commit.
    let tag = git(&repo, &["rev-parse", "refs/tags/v1"]);
    assert_ne!(tag.trim(), head);
    assert_eq!(revision(&repo, "refs/tags/v1", run).await.unwrap(), tag);
    let verified = verify_revision(&repo, "HEAD", run).await.unwrap();
    assert_eq!(verified.trim(), head);
    assert!(
        verify_revision(&repo, "refs/heads/missing", run)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn refs_read_in_a_repository_with_its_refs_in_files() {
    ref_reads(repository).await;
}

#[tokio::test]
async fn refs_read_in_a_reftable_repository() {
    ref_reads(reftable_repository).await;
}
