//! Boolean Git queries whose negative answers have documented exit statuses.
use std::path::Path;

use anyhow::Result;
use tokio::process::Command;

use crate::subprocess;

/// Whether `ancestor` is reachable from `descendant`. Invalid commits and
/// command failures are errors, not negative ancestry answers.
pub async fn is_ancestor(
    repo: &Path,
    ancestor: &str,
    descendant: &str,
    command: impl FnOnce(&Path) -> Command,
) -> Result<bool> {
    let mut command = command(repo);
    command.args(["merge-base", "--is-ancestor", ancestor, descendant]);
    predicate(command, 1).await
}

/// Whether an exact ref exists, without resolving its target object. Callers
/// pass full ref names, not revision expressions. Unlike `--verify --quiet`,
/// `--exists` distinguishes a missing ref (2) from a broken ref (1).
pub async fn ref_exists(
    repo: &Path,
    reference: &str,
    command: impl FnOnce(&Path) -> Command,
) -> Result<bool> {
    let mut command = command(repo);
    command.args(["show-ref", "--exists", "--", reference]);
    predicate(command, 2).await
}

async fn predicate(command: Command, negative: i32) -> Result<bool> {
    let description = format!(
        "{} {:?}",
        command.as_std().get_program().to_string_lossy(),
        command.as_std().get_args().collect::<Vec<_>>()
    );
    let output = subprocess::Run::new(command).capture().await;
    match output.as_ref().ok().and_then(|output| output.status.code()) {
        Some(0) => Ok(true),
        Some(code) if code == negative => Ok(false),
        _ => subprocess::checked_output(&description, output).map(|_| true),
    }
}

#[cfg(test)]
mod tests {
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
                ("kill -TERM $$", None),
                ("missing executable", None),
            ] {
                let command = |_: &Path| {
                    if script == "missing executable" {
                        Command::new(root.path().join("missing-git"))
                    } else {
                        let mut command = Command::new("/bin/sh");
                        command.args(["-c", script]).env_clear();
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
}
