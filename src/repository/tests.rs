use super::*;

fn repository(id: &str, path: &str, name: Option<&str>) -> Repository {
    Repository {
        id: id.into(),
        path: path.into(),
        source: format!("https://example.test/team/{id}"),
        last_used: 0,
        name: name.map(str::to_owned),
        workspaces_dir: None,
        contribution: false,
    }
}

#[tokio::test]
async fn typed_name_wins_over_a_colliding_caller_path() {
    for explicit_name in [Some("saldoir-server"), None] {
        let mut repo = repository("registered", "/tmp/registered", explicit_name);
        repo.source = "https://example.test/team/saldoir-server".into();
        let repositories = [repo];
        let selector = Selector::Path {
            value: "saldoir-server".into(),
            path: "/tmp/unrelated/saldoir-server".into(),
        };
        assert_eq!(
            select(&repositories, selector).await.unwrap().id,
            "registered"
        );
    }
}

#[tokio::test]
async fn colliding_name_and_path_are_reported_as_ambiguous() {
    let repositories = [
        repository("named", "/tmp/named", Some("saldoir-server")),
        repository("path", "/tmp/unrelated/saldoir-server", None),
    ];
    let selector = Selector::Path {
        value: "saldoir-server".into(),
        path: "/tmp/unrelated/saldoir-server".into(),
    };
    let error = select(&repositories, selector)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("named") && error.contains("path"), "{error}");
}

#[tokio::test]
async fn ids_and_sources_take_precedence_over_a_colliding_path() {
    let repositories = [
        repository("registered", "/tmp/one", None),
        repository("other", "/tmp/checkout", None),
    ];
    for value in [&repositories[0].id, &repositories[0].source] {
        let selector = Selector::Path {
            value: value.clone(),
            path: "/tmp/checkout".into(),
        };
        assert_eq!(
            select(&repositories, selector).await.unwrap().id,
            "registered"
        );
    }
}

#[tokio::test]
async fn source_lookup_keeps_urls_and_reads_only_origin_from_checkouts() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path();
    git::run(path, &["init", "--quiet", "--template="])
        .await
        .unwrap();
    let source = path.to_str().unwrap();
    assert_eq!(remote_url_from_source(source).await.unwrap(), None);
    git::run(
        path,
        &["remote", "add", "upstream", "https://other.test/a/b"],
    )
    .await
    .unwrap();
    assert_eq!(remote_url_from_path(path).await.unwrap(), None);

    let url = "ssh://git@example.com:2222/team/Repo.git";
    git::run(path, &["remote", "add", "origin", url])
        .await
        .unwrap();
    assert_eq!(
        remote_url_from_path(path).await.unwrap().as_deref(),
        Some(url)
    );
    assert_eq!(
        remote_url_from_source(source).await.unwrap().as_deref(),
        Some(url)
    );
    assert_eq!(
        identity(source).await.unwrap(),
        identity(url).await.unwrap()
    );
    assert_eq!(
        remote_url_from_source(url).await.unwrap().as_deref(),
        Some(url)
    );

    let missing = path.join("missing");
    assert!(remote_url_from_path(&missing).await.is_err());
    let missing = missing.to_str().unwrap();
    assert_eq!(
        remote_url_from_source(missing).await.unwrap().as_deref(),
        Some(missing)
    );
}

// macOS filesystems reject non-UTF-8 directory names.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn origin_lookup_accepts_non_utf8_checkout_paths() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(OsStr::from_bytes(b"repo-\xff"));
    std::fs::create_dir(&path).unwrap();
    git::run(&path, &["init", "--quiet", "--template="])
        .await
        .unwrap();
    let url = "git@example.com:team/repo.git";
    git::run(&path, &["remote", "add", "origin", url])
        .await
        .unwrap();
    let remote = remote_url_from_path(&path).await.unwrap().unwrap();
    let forge = crate::forge::ForgeRepo::parse(&remote).unwrap();
    assert_eq!(
        forge.issue("178").unwrap().1,
        "https://example.com/team/repo/issues/178"
    );
    assert_eq!(
        forge.pull("178").unwrap().1,
        "https://example.com/team/repo/pulls/178"
    );
}

#[test]
fn clone_directory_names_are_readable_single_components() {
    for (source, expected) in [
        (
            "https://example.com/team/saldoir-server.git",
            "saldoir-server",
        ),
        ("git@example.com:team/project.git", "project"),
        (
            "ssh://git@example.com:2222/team/My.Project.git/",
            "My.Project",
        ),
        ("file:///tmp/local repo.git", "local-repo"),
        ("file:///tmp/..", "repository"),
        ("file:///tmp/.git", "repository"),
    ] {
        assert_eq!(directory_name(source), expected);
    }
    assert_eq!(
        directory_name(&format!("https://example.com/{}", "a".repeat(300))).len(),
        64
    );
}

#[test]
fn hosts_drop_credentials_and_paths() {
    assert_eq!(
        host("https://user@example.com/team/repo"),
        Some("example.com")
    );
    assert_eq!(host("git@example.com:team/repo.git"), Some("example.com"));
    assert_eq!(host("/local/checkout"), None);
}

#[test]
fn equivalent_transports_match_without_conflating_hosts_paths_or_ports() {
    let key = url_key("https://example.com/team/Repo.git");
    assert_eq!(key, url_key("git@EXAMPLE.com:team/Repo.git"));
    assert_eq!(key, url_key("ssh://git@example.com/team/Repo/"));
    for other in [
        "https://other.com/team/Repo.git",
        "https://example.com/other/Repo.git",
        "https://example.com/team/repo.git",
        "ssh://git@example.com:2222/team/Repo.git",
    ] {
        assert_ne!(key, url_key(other));
    }
    assert_ne!(url_key("file:///repo.git"), url_key("file:///repo"));
}
