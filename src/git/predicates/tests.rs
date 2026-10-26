use super::*;
use crate::{git, test_support};

#[tokio::test]
async fn predicates_distinguish_answers_errors_and_spawn_failure() {
    let root = tempfile::tempdir().unwrap();
    for ancestry in [true, false] {
        for (script, expected) in [
            ("exit 0", Some(true)),
            ("exit 1", ancestry.then_some(false)),
            ("exit 2", (!ancestry).then_some(false)),
            ("echo broken >&2; exit 128", None),
            ("echo unknown option exists >&2; exit 129", None),
            ("kill -TERM $$", None),
            ("missing executable", None),
        ] {
            let command = |_: &Path| {
                if script == "missing executable" {
                    Command::new(root.path().join("missing-git"))
                } else {
                    let mut command = Command::new("/bin/sh");
                    command
                        .args(["-c", script])
                        .env_clear()
                        .env("SHOAL_TEST_PROCESS", "1");
                    command
                }
            };
            let result = if ancestry {
                is_ancestor(root.path(), "parent", "child", command).await
            } else {
                ref_exists(root.path(), "refs/heads/topic", command).await
            };
            match expected {
                Some(expected) => assert_eq!(result.unwrap(), expected),
                None => {
                    let error = format!("{:#}", result.unwrap_err());
                    if script.contains("broken") {
                        assert!(error.contains("broken") && error.contains("128"));
                    } else if script == "missing executable" {
                        assert!(error.contains("missing-git"));
                    } else if script.contains("unknown option") {
                        assert!(error.contains("unknown option exists"));
                        if !ancestry {
                            assert!(error.contains("Git 2.43 or newer is required"));
                        }
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn real_git_checks_history_and_exact_refs() {
    let root = tempfile::tempdir().unwrap();
    let repo = test_support::repository(root.path(), "repo");
    let parent = test_support::git(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("tracked"), "child").unwrap();
    test_support::commit(&repo, "tracked");
    for command in [git::command, git::isolated_command] {
        assert!(
            is_ancestor(&repo, parent.trim(), "HEAD", command)
                .await
                .unwrap()
        );
        assert!(
            !is_ancestor(&repo, "HEAD", parent.trim(), command)
                .await
                .unwrap()
        );
        assert!(
            is_ancestor(&repo, "missing", "HEAD", command)
                .await
                .is_err()
        );
        assert!(ref_exists(&repo, "refs/heads/main", command).await.unwrap());
        assert!(
            !ref_exists(&repo, "refs/heads/missing", command)
                .await
                .unwrap()
        );
        assert!(!ref_exists(&repo, "main", command).await.unwrap());
        assert!(
            ref_exists(root.path(), "refs/heads/main", command)
                .await
                .is_err()
        );
        assert!(
            is_ancestor(root.path(), "HEAD", "HEAD", command)
                .await
                .is_err()
        );
        std::fs::write(repo.join(".git/refs/heads/broken"), "broken\n").unwrap();
        assert!(
            ref_exists(&repo, "refs/heads/broken", command)
                .await
                .is_err()
        );
    }
}
