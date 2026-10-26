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
        push_remote(&clone_path, "topic").await.unwrap(),
        FORK_REMOTE
    );
    git(&clone_path, &["switch", "-c", "topic"]);
    std::fs::write(clone_path.join("change"), "change\n").unwrap();
    commit(&clone_path, "change");
    assert_eq!(
        status::unretained_commits(&clone_path, "HEAD", None)
            .await
            .unwrap(),
        1
    );
    let push = push_command(FORK_REMOTE, "topic");
    let args: Vec<&str> = push[1..].iter().map(|arg| arg.to_str().unwrap()).collect();
    git(&clone_path, &args);
    assert_eq!(
        status::unretained_commits(&clone_path, "HEAD", None)
            .await
            .unwrap(),
        0
    );
    assert!(
        !remote_has_branch(&clone_path, "origin", "topic")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn value_queries_keep_arguments_and_runner_errors() {
    let repo = Path::new("/repo");
    for quiet in [false, true] {
        let branch = head_branch(repo, quiet, async |path, args| {
            assert_eq!(path, repo);
            let expected = if quiet {
                vec!["symbolic-ref", "--quiet", "HEAD"]
            } else {
                vec!["symbolic-ref", "HEAD"]
            };
            assert_eq!(args, expected);
            Ok("refs/heads/feature/nested\n".into())
        })
        .await
        .unwrap();
        assert_eq!(branch.as_deref(), Some("feature/nested"));
    }
    let commit = resolve_commit(repo, "refs/tags/v1", async |path, args| {
        assert_eq!(path, repo);
        assert_eq!(args, ["rev-parse", "--verify", "refs/tags/v1^{commit}"]);
        Ok("abc\n".into())
    })
    .await
    .unwrap();
    assert_eq!(commit, "abc");
    let tree = checkout_of(repo, "feature/nested", async |path, args| {
            assert_eq!(path, repo);
            assert_eq!(args, ["worktree", "list", "--porcelain", "-z"]);
            Ok("worktree /detached\0detached\0\0worktree /feature\0branch refs/heads/feature/nested\0locked\0prunable\0\0".into())
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tree.path, Path::new("/feature"));
    assert!(tree.locked && tree.prunable);

    async fn failed(_: &Path, _: &[&str]) -> Result<String> {
        anyhow::bail!("runner failed")
    }
    assert_eq!(
        head_branch(repo, true, failed)
            .await
            .unwrap_err()
            .to_string(),
        "runner failed"
    );
    assert_eq!(
        resolve_commit(repo, "HEAD", failed)
            .await
            .unwrap_err()
            .to_string(),
        "runner failed"
    );
    assert_eq!(
        checkout_of(repo, "main", failed)
            .await
            .unwrap_err()
            .to_string(),
        "runner failed"
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
        head_branch(&new.join("feature"), false, run)
            .await?
            .as_deref(),
        Some("feature")
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
        resolve_commit(&repo, &local_ref("feature/nested"), run_isolated)
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
        head_branch(&repo, true, run).await.unwrap().as_deref(),
        Some("feature/nested")
    );
    assert_eq!(
        head_branch(&repo, false, run_isolated)
            .await
            .unwrap()
            .as_deref(),
        Some("feature/nested")
    );
    assert_eq!(
        checkout_of(&repo, "feature/nested", run)
            .await
            .unwrap()
            .unwrap()
            .path,
        repo.canonicalize().unwrap()
    );
    assert!(checkout_of(&repo, "feature", run).await.unwrap().is_none());
    git(&repo, &["checkout", "--detach"]);
    assert!(head_branch(&repo, true, run).await.is_err());
    assert!(
        checkout_of(&repo, "feature/nested", run)
            .await
            .unwrap()
            .is_none()
    );
    git(&repo, &["symbolic-ref", "HEAD", "refs/heads/unborn"]);
    assert_eq!(
        head_branch(&repo, true, run).await.unwrap().as_deref(),
        Some("unborn")
    );
    git(&repo, &["symbolic-ref", "HEAD", "refs/tags/v1"]);
    assert_eq!(head_branch(&repo, true, run).await.unwrap(), None);
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
fn parses_porcelain_worktree_records() {
    let listing = "worktree /repo\0HEAD abc\0branch refs/heads/main\0\0\
            worktree /repo/.wt/feature\0HEAD def\0branch refs/heads/feature\0locked reason\0prunable gitdir file points to non-existent location\0\0\
            worktree /repo/.wt/detached\0HEAD 123\0detached\0\0";
    let trees = parse_worktrees(listing);
    assert_eq!(trees.len(), 3);
    assert!(trees[0].is_branch("main") && !trees[0].locked && !trees[0].prunable);
    assert_eq!(trees[1].path, PathBuf::from("/repo/.wt/feature"));
    assert!(trees[1].locked && trees[1].prunable);
    assert_eq!(trees[2].branch, None);
    assert!(parse_worktrees("").is_empty());
}

#[tokio::test]
async fn branch_names_follow_git_syntax() {
    for name in ["feature/login", "fix-2", "HEAD", "release/v1.0.0"] {
        check_branch_name(None, name).await.unwrap();
    }
    for name in [
        "",
        "bad name",
        "-dash",
        "a..b",
        "trailing/",
        "@{-1}",
        "refs/heads/x\n",
    ] {
        let error = check_branch_name(None, name).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("branch name"),
            "{name:?}: {error:#}"
        );
    }
}
