use super::*;

#[tokio::test]
async fn a_clone_with_a_fork_pushes_there_and_counts_the_push() {
    use crate::test_support::{commit, git, repository};
    let root = tempfile::tempdir().unwrap();
    let upstream = repository(root.path(), "upstream");
    let fork = root.path().join("fork.git");
    git(
        root.path(),
        &[
            "clone",
            "--bare",
            "--",
            upstream.to_str().unwrap(),
            fork.to_str().unwrap(),
        ],
    );
    let clone_path = root.path().join("clone");
    // The user's Git configuration and hooks stay out of the test.
    let mut cloning = clone_command(
        upstream.to_str().unwrap(),
        &clone_path,
        Some(fork.to_str().unwrap()),
    );
    cloning
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    subprocess::output(cloning).await.unwrap();
    assert_eq!(
        push_remote(&clone_path, &"topic".into()).await.unwrap(),
        FORK_REMOTE
    );
    git(&clone_path, &["switch", "-c", "topic"]);
    std::fs::write(clone_path.join("change"), "change\n").unwrap();
    commit(&clone_path, "change");
    assert_eq!(
        status::unretained_commits(&clone_path, "HEAD", &[])
            .await
            .unwrap(),
        1
    );
    let push = push_command(FORK_REMOTE, &"topic".into());
    let args: Vec<&str> = push[1..].iter().map(|arg| arg.to_str().unwrap()).collect();
    git(&clone_path, &args);
    assert_eq!(
        status::unretained_commits(&clone_path, "HEAD", &[])
            .await
            .unwrap(),
        0
    );
    assert!(
        !remote_has_branch(&clone_path, "origin", &"topic".into())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn repair_reconnects_a_moved_checkout_and_worktree() -> Result<()> {
    use crate::test_support::{git, repository};
    let root = tempfile::tempdir()?;
    let old = std::fs::canonicalize(root.path())?.join("old");
    std::fs::create_dir(&old)?;
    let checkout = repository(&old, "checkout");
    git(
        &checkout,
        &["worktree", "add", "-b", "feature", "../feature"],
    );
    let new = old.with_file_name("new");
    std::fs::rename(&old, &new)?;

    repair_worktrees(&new.join("checkout"), &[new.join("feature")]).await?;
    let trees = worktrees(&new.join("checkout"), run).await?;
    assert_eq!(
        trees
            .iter()
            .map(|tree| tree.path.clone())
            .collect::<Vec<_>>(),
        [new.join("checkout"), new.join("feature")]
    );
    assert!(trees.iter().all(|tree| !tree.prunable));
    assert_eq!(
        head_branch(&new.join("feature")).await?,
        Some("feature".into())
    );
    Ok(())
}

#[tokio::test]
async fn value_queries_distinguish_branches_tags_and_detached_head() {
    use crate::test_support::{git, repository};
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["branch", "-m", "feature/nested"]);
    git(&repo, &["tag", "-a", "v1", "-m", "annotated"]);
    let expected = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    assert_eq!(
        resolve_commit(&repo, "refs/tags/v1", run).await.unwrap(),
        expected
    );
    assert_eq!(
        resolve_commit(&repo, &local_ref(&"feature/nested".into()), run_isolated)
            .await
            .unwrap(),
        expected
    );
    assert!(resolve_commit(&repo, "HEAD^{tree}", run).await.is_err());
    assert!(
        resolve_commit(&repo, "missing", run_isolated)
            .await
            .is_err()
    );
    assert_eq!(
        head_branch(&repo).await.unwrap(),
        Some("feature/nested".into())
    );
    assert_eq!(
        checkout_of(&repo, &"feature/nested".into(), run)
            .await
            .unwrap()
            .unwrap()
            .path,
        repo.canonicalize().unwrap()
    );
    assert!(
        checkout_of(&repo, &"feature".into(), run)
            .await
            .unwrap()
            .is_none()
    );
    git(&repo, &["checkout", "--detach"]);
    assert!(head_branch(&repo).await.is_err());
    assert!(
        checkout_of(&repo, &"feature/nested".into(), run)
            .await
            .unwrap()
            .is_none()
    );
    git(&repo, &["symbolic-ref", "HEAD", "refs/heads/unborn"]);
    assert_eq!(head_branch(&repo).await.unwrap(), Some("unborn".into()));
    git(&repo, &["symbolic-ref", "HEAD", "refs/tags/v1"]);
    assert_eq!(head_branch(&repo).await.unwrap(), None);
}

#[tokio::test]
async fn a_named_commit_reads_the_ref_a_revision_names_with_its_commit() {
    use crate::test_support::{commit, git, repository};
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let first = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    std::fs::write(repo.join("second"), "").unwrap();
    commit(&repo, "second");
    git(&repo, &["tag", "-a", "v1", "-m", "annotated"]);
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let named = async |rev: &str| resolve_named_commit(&repo, rev, run_isolated).await;
    let resolved = |commit: &str, name: &str| (CommitId::from(commit), name.to_owned());
    assert_eq!(
        named("main").await.unwrap(),
        resolved(&head, "refs/heads/main")
    );
    assert_eq!(named("v1").await.unwrap(), resolved(&head, "refs/tags/v1"));
    assert_eq!(named("main~1").await.unwrap(), resolved(&first, ""));
    assert_eq!(named(&first).await.unwrap(), resolved(&first, ""));
    // A file of that name is no revision.
    std::fs::write(repo.join("file"), "").unwrap();
    for rev in [
        "missing",
        "file",
        "HEAD^{tree}",
        "--all",
        "main~1..main",
        "main...main~1",
        "^main",
    ] {
        assert!(named(rev).await.is_err(), "{rev} resolved");
    }
}

#[tokio::test]
async fn background_ssh_disables_prompts_and_preserves_transport_selection() {
    let root = tempfile::tempdir().unwrap();
    let ssh = root.path().join("fake-ssh");
    std::fs::write(
            &ssh,
            r#"#!/bin/sh
printf 'transport=%s askpass=%s terminal=%s\n' "$1" "$SSH_ASKPASS_REQUIRE" "$GIT_TERMINAL_PROMPT" >&2
exit 1
"#,
        )
        .unwrap();
    for isolated in [false, true] {
        for override_transport in [false, true] {
            let mut command = if isolated {
                isolated_command(root.path())
            } else {
                let mut command = command(root.path());
                command
                    .env("SSH_ASKPASS_REQUIRE", "force")
                    .env("GIT_TERMINAL_PROMPT", "1");
                command
            };
            command
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_SSH_VARIANT", "ssh")
                .env("SHOAL_TEST_SSH", &ssh)
                .env_remove("GIT_SSH_COMMAND")
                .args([
                    "-c",
                    "core.sshCommand=sh \"$SHOAL_TEST_SSH\" config",
                    "ls-remote",
                    "git@example.invalid:team/project.git",
                ]);
            if override_transport {
                command.env("GIT_SSH_COMMAND", "sh \"$SHOAL_TEST_SSH\" env");
            }
            let error = subprocess::output(command).await.unwrap_err();
            let transport = if override_transport { "env" } else { "config" };
            let prompts = if isolated {
                "askpass=never terminal=0"
            } else {
                "askpass=force terminal=1"
            };
            assert!(
                error
                    .to_string()
                    .contains(&format!("transport={transport} {prompts}")),
                "{error:#}"
            );
        }
    }
}

#[test]
fn shoal_git_works_where_it_is_told_not_where_the_caller_works() {
    let clone = clone_command("source", Path::new("/clone"), None);
    for command in [command(Path::new("/repo")), clone] {
        let removed: Vec<_> = (command.as_std().get_envs())
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_owned())
            .collect();
        for name in REPOSITORY_ENV {
            assert!(removed.iter().any(|removed| removed == name), "{name}");
        }
    }
}

#[test]
fn an_approved_push_replays_its_url_and_keeps_a_missing_ref_missing() {
    use shoal_protocol::approvals::PushSpecification;
    let push = PushSpecification {
        remote: "origin".into(),
        url: Some("git@mirror:o/r.git".into()),
        destination: "refs/heads/topic".into(),
        commit: Some("abc".into()),
        replaces: Some(String::new()),
        agent: None,
    };
    assert_eq!(
        approved_push_command(&push),
        [
            "git",
            "push",
            "--force-with-lease=refs/heads/topic:",
            "--",
            "git@mirror:o/r.git",
            "abc:refs/heads/topic",
        ]
        .map(std::ffi::OsString::from)
    );
    assert!(!push.subject().contains("force"));
    assert_eq!(push.request_name(), "refs/heads/topic git@mirror:o/r.git");
    let forced = PushSpecification {
        url: None,
        replaces: Some("old".into()),
        ..push
    };
    assert!(forced.subject().contains("force"));
    assert_eq!(
        approved_push_command(&forced)[4],
        std::ffi::OsString::from("origin")
    );
}

#[test]
fn the_core_bare_fix_quotes_the_checkout_path() {
    assert_eq!(
        clear_bare_flag_command(Path::new("/repos/my app")),
        "git -C '/repos/my app' config core.bare false"
    );
}
