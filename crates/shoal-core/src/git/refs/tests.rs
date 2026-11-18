use std::path::{Path, PathBuf};

use super::{Listed, list};
use crate::git::{
    has_commit, ref_exists, resolve_commit, resolve_named_commit, revision, run, verify_revision,
};
use crate::test_support::{git, reftable_repository, repository};

/// The ways a test repository keeps its refs: in files, which gix reads,
/// and in a reftable, which Git reads for it.
type Create = fn(&Path, &str) -> PathBuf;

/// Lookups and listings of refs in a repository `create` makes.
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

    // A symbolic ref leads through every symbolic ref after it.
    let symbolic = |name: &str, target: &str| {
        let (name, target) = (
            format!("refs/remotes/origin/{name}"),
            format!("refs/remotes/origin/{target}"),
        );
        git(&repo, &["symbolic-ref", &name, &target]);
    };
    symbolic("stable", "main");
    symbolic("latest", "stable");
    let target = super::symbolic_target(&repo, "refs/remotes/origin/latest")
        .await
        .unwrap();
    assert_eq!(target.as_deref(), Some("refs/remotes/origin/main"));
    let direct = super::symbolic_target(&repo, "refs/remotes/origin/main")
        .await
        .unwrap();
    assert_eq!(direct, None);
    for name in ["latest", "stable"] {
        let name = format!("refs/remotes/origin/{name}");
        git(&repo, &["update-ref", "-d", "--no-deref", &name]);
    }

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

    // One peel reads in process as Git reads it.
    let peeled_tree = verify_revision(&repo, "refs/tags/v1^{tree}", run)
        .await
        .unwrap();
    assert_eq!(peeled_tree.trim(), tree);
    let peeled = revision(&repo, &format!("{head}^{{commit}}"), run)
        .await
        .unwrap();
    assert_eq!(peeled.trim(), head);
    assert!(
        verify_revision(&repo, &format!("{tree}^{{commit}}"), run)
            .await
            .is_err()
    );

    let named = |rev: &str| {
        let rev = rev.to_owned();
        let repo = repo.clone();
        async move { resolve_named_commit(&repo, &rev, run).await.unwrap() }
    };
    let commit = |name: &str| (head.as_str().into(), name.to_owned());
    assert_eq!(named("HEAD").await, commit("refs/heads/main"));
    assert_eq!(named("refs/tags/v1").await, commit("refs/tags/v1"));
    assert_eq!(named(&head).await, commit(""));
    git(&repo, &["switch", "-q", "--detach"]);
    assert_eq!(named("HEAD").await, commit("HEAD"));
    git(&repo, &["switch", "-q", "main"]);

    let names = |listed: Vec<Listed>| listed.into_iter().map(|r| r.name).collect::<Vec<_>>();
    let topic = list(&repo, &["refs/heads/topic"]).await.unwrap();
    assert_eq!(names(topic), ["refs/heads/topic/nested"]);
    let remotes = list(&repo, &["refs/remotes/", "refs/heads/main"])
        .await
        .unwrap();
    let origin = |name: &str, symref: Option<&str>, object: &str| Listed {
        name: format!("refs/remotes/origin/{name}"),
        object: object.to_owned(),
        symref: symref.map(|target| format!("refs/remotes/origin/{target}")),
    };
    let main = Listed {
        name: "refs/heads/main".into(),
        object: head.clone(),
        symref: None,
    };
    let expected = [
        main,
        origin("HEAD", Some("main"), &head),
        origin("main", None, &head),
    ];
    assert_eq!(remotes, expected);
    assert!(list(&repo, &[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn refs_read_in_a_repository_with_its_refs_in_files() {
    ref_reads(repository).await;
}

#[tokio::test]
async fn refs_read_in_a_reftable_repository() {
    ref_reads(reftable_repository).await;
}

#[tokio::test]
async fn a_tree_a_partial_clone_lacks_is_fetched_as_git_fetches_it() {
    let root = tempfile::tempdir().unwrap();
    let source = repository(root.path(), "source");
    git(&source, &["config", "uploadpack.allowFilter", "true"]);
    let tree = git(&source, &["rev-parse", "HEAD^{tree}"]);
    let url = format!("file://{}", source.display());
    let clone = [
        "clone",
        "-q",
        "--no-checkout",
        "--filter=tree:0",
        &url,
        "clone",
    ];
    git(root.path(), &clone);
    let clone = root.path().join("clone");
    // Git marks the clone partial with a promisor remote alone.
    let read = verify_revision(&clone, "refs/heads/main^{tree}", run)
        .await
        .unwrap();
    assert_eq!(read, tree);
}

#[tokio::test]
async fn a_sha1_length_prefix_of_a_sha256_id_resolves_as_git_resolves_it() {
    let root = tempfile::tempdir().unwrap();
    let init = ["init", "-q", "-b", "main", "--object-format=sha256", "repo"];
    git(root.path(), &init);
    let repo = root.path().join("repo");
    let identity = [
        "-c",
        "user.name=Shoal",
        "-c",
        "user.email=shoal@example.com",
    ];
    git(
        &repo,
        &[
            &identity[..],
            &["commit", "-q", "--allow-empty", "-m", "first"],
        ]
        .concat(),
    );
    let commit = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    assert_eq!(commit.len(), 64);
    // Forty hex digits abbreviate a SHA-256 ID rather than name one.
    let prefix = &commit[..40];
    assert_eq!(
        resolve_commit(&repo, prefix, run).await.unwrap(),
        commit.as_str()
    );
    assert!(has_commit(&repo, prefix).await.unwrap());
    assert_eq!(
        resolve_commit(&repo, &commit, run).await.unwrap(),
        commit.as_str()
    );
}
