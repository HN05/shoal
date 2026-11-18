use std::path::{Path, PathBuf};

use super::*;
use crate::git::config_value;
use crate::test_support::{git, reftable_repository, repository};

/// The ways a test repository keeps its refs: in files, which gix reads,
/// and in a reftable, which Git reads for it.
type Create = fn(&Path, &str) -> PathBuf;

/// Remote, URL and push-remote reads in a repository `create` makes.
async fn remote_reads(create: Create) {
    let root = tempfile::tempdir().unwrap();
    let repo = create(root.path(), "repo");
    assert!(remote_names(&repo).await.unwrap().is_empty());
    assert_eq!(configured_remote_url(&repo, "origin").await.unwrap(), None);
    assert!(remote_url(&repo, "origin").await.is_err());

    git(&repo, &["remote", "add", "zeta", "https://example.com/a/b"]);
    git(
        &repo,
        &["remote", "add", "origin", "git@host.test:team/app.git"],
    );
    git(
        &repo,
        &["config", "remote.bare.pushurl", "https://push.test/bare"],
    );
    let names = remote_names(&repo).await.unwrap();
    assert_eq!(names, ["bare", "origin", "zeta"]);

    // The longest insteadOf prefix wins; a remote without a URL is its name.
    git(
        &repo,
        &[
            "config",
            "url.https://mirror.test/.insteadOf",
            "https://example.com/",
        ],
    );
    git(
        &repo,
        &[
            "config",
            "url.https://deep.test/.insteadOf",
            "https://example.com/a/",
        ],
    );
    let at = repo.as_path();
    let url = move |name| async move { configured_remote_url(at, name).await.unwrap() };
    assert_eq!(url("zeta").await.as_deref(), Some("https://deep.test/b"));
    assert_eq!(
        url("origin").await.as_deref(),
        Some("git@host.test:team/app.git")
    );
    assert_eq!(url("bare").await.as_deref(), Some("bare"));
    let urls = remote_urls(&repo).await.unwrap();
    let configured = ["https://example.com/a/b", "git@host.test:team/app.git"];
    assert_eq!(urls.fetch, configured);
    assert_eq!(urls.push, ["https://push.test/bare"]);

    assert_eq!(push_remote(&repo, &"topic".into()).await.unwrap(), "origin");
    git(&repo, &["config", "remote.pushDefault", "fork"]);
    assert_eq!(push_remote(&repo, &"topic".into()).await.unwrap(), "fork");
    git(&repo, &["config", "branch.topic.pushRemote", "mine"]);
    assert_eq!(push_remote(&repo, &"topic".into()).await.unwrap(), "mine");
    assert_eq!(push_remote(&repo, &"other".into()).await.unwrap(), "fork");
    git(&repo, &["config", "branch.dotted.name.pushRemote", "dots"]);
    let dotted = config_value(&repo, "branch.dotted.name.pushRemote").await;
    assert_eq!(dotted.unwrap().as_deref(), Some("dots"));
    // Any other failure stays an error.
    let missing = root.path().join("missing");
    configured_remote_url(&missing, "origin").await.unwrap_err();
}

#[tokio::test]
async fn remotes_read_in_a_repository_with_its_refs_in_files() {
    remote_reads(repository).await;
}

#[tokio::test]
async fn remotes_read_in_a_reftable_repository() {
    remote_reads(reftable_repository).await;
}

/// Upstream reads of branches in a repository `create` makes.
async fn upstream_reads(create: Create) {
    let root = tempfile::tempdir().unwrap();
    let repo = create(root.path(), "repo");
    git(
        &repo,
        &["remote", "add", "origin", "https://example.com/app"],
    );
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&repo, &["branch", "--track", "tracked", "origin/main"]);
    git(&repo, &["branch", "--track", "local", "main"]);
    git(&repo, &["branch", "plain"]);
    let at = repo.as_path();
    let upstream = move |name: &str| {
        let reference = format!("refs/heads/{name}");
        async move { upstream(at, &reference).await.unwrap() }
    };
    let tracked = Upstream {
        remote: "origin".into(),
        merge: "refs/heads/main".into(),
        tracking: "refs/remotes/origin/main".into(),
    };
    assert_eq!(upstream("tracked").await, Some(tracked));
    let local = Upstream {
        remote: ".".into(),
        merge: "refs/heads/main".into(),
        tracking: "refs/heads/main".into(),
    };
    assert_eq!(upstream("local").await, Some(local));
    assert_eq!(upstream("plain").await, Some(Upstream::default()));
    assert_eq!(upstream("missing").await, None);
    // A branch no fetch refspec stores has no tracking ref.
    git(
        &repo,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/other:refs/remotes/origin/other",
        ],
    );
    // With no tracking ref, Git reports no upstream at all.
    assert_eq!(upstream("tracked").await, Some(Upstream::default()));
}

#[tokio::test]
async fn upstreams_read_in_a_repository_with_its_refs_in_files() {
    upstream_reads(repository).await;
}

#[tokio::test]
async fn upstreams_read_in_a_reftable_repository() {
    upstream_reads(reftable_repository).await;
}

#[tokio::test]
async fn a_remote_named_in_a_remotes_file_is_read_as_git_reads_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    std::fs::create_dir(repo.join(".git/remotes")).unwrap();
    std::fs::write(
        repo.join(".git/remotes/legacy"),
        "URL: https://legacy.test/app\n",
    )
    .unwrap();
    // Whether Git still reads such files depends on its version.
    let listed: Vec<String> = git(&repo, &["remote"]).lines().map(String::from).collect();
    assert_eq!(remote_names(&repo).await.unwrap(), listed);
}

/// The URL `origin`, configured as `bar:repo`, has under the rewrite
/// sections `sections`, each a base and its key and prefix, compared with
/// the URL Git gives it.
async fn rewritten_as_git_rewrites(sections: &[(&str, &str, &str)]) {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let config = repo.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    for (base, key, prefix) in sections {
        text.push_str(&format!(
            "[url \"https://{base}.example/\"]\n\t{key} = {prefix}\n"
        ));
    }
    text.push_str("[remote \"origin\"]\n\turl = bar:repo\n");
    std::fs::write(&config, text).unwrap();
    let expected = git(&repo, &["remote", "get-url", "origin"]);
    let url = configured_remote_url(&repo, "origin").await.unwrap();
    assert_eq!(url.as_deref(), Some(expected.trim()));
}

#[tokio::test]
async fn equally_long_rewrites_go_to_the_base_configured_first() {
    let sections = [
        ("a", "insteadOf", "foo:"),
        ("b", "insteadOf", "bar:"),
        ("a", "insteadOf", "bar:"),
    ];
    rewritten_as_git_rewrites(&sections).await;
}

#[tokio::test]
async fn a_push_rewrite_places_no_base_among_fetch_rewrites() {
    let sections = [
        ("a", "pushInsteadOf", "foo:"),
        ("b", "insteadOf", "bar:"),
        ("a", "insteadOf", "bar:"),
    ];
    rewritten_as_git_rewrites(&sections).await;
}

#[tokio::test]
async fn push_urls_are_the_pushurls_or_the_url_as_git_rewrites_it_for_pushes() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    assert!(remote_push_urls(&repo, "origin").await.is_err());
    git(
        &repo,
        &["remote", "add", "origin", "https://fetch.test/a/b"],
    );
    git(
        &repo,
        &[
            "config",
            "url.https://push.test/.pushInsteadOf",
            "https://fetch.test/",
        ],
    );
    let pushed = remote_push_urls(&repo, "origin").await.unwrap();
    assert_eq!(pushed, ["https://push.test/a/b"]);
    git(
        &repo,
        &["config", "remote.origin.pushurl", "https://other.test/c/d"],
    );
    git(
        &repo,
        &["config", "--add", "remote.origin.pushurl", "/mirror.git"],
    );
    let pushed = remote_push_urls(&repo, "origin").await.unwrap();
    assert_eq!(pushed, ["https://other.test/c/d", "/mirror.git"]);
}
