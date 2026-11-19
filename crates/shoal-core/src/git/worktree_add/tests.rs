use std::path::Path;

use super::*;

/// What Git prints when a prune deleted the admin directory it had made.
const PRUNED: &str = "Preparing worktree (checking out 'first-2')\n\
     fatal: could not open '.git/worktrees/first-2/locked' for writing: No such file or directory";

/// A command that records its run in `runs`, then fails with `stderr` on
/// the runs `failing` names, a shell test of the run's number, and prints
/// `added` otherwise.
fn add(runs: &Path, failing: &str, stderr: &str) -> impl Fn() -> Command {
    let script = format!(
        "echo run >> '{runs}'; n=$(wc -l < '{runs}'); \
         if [ {failing} ]; then printf '%s\\n' \"$1\" >&2; exit 128; fi; echo added",
        runs = runs.display(),
    );
    let stderr = stderr.to_owned();
    move || {
        let mut command = Command::new("sh");
        command.args(["-c", &script, "sh", &stderr]);
        command
    }
}

fn runs(runs: &Path) -> usize {
    std::fs::read_to_string(runs).unwrap().lines().count()
}

#[tokio::test]
async fn an_add_a_prune_undid_runs_again() {
    let root = tempfile::tempdir().unwrap();
    let count = root.path().join("runs");
    let added = run(add(&count, "$n -eq 1", PRUNED)).await.unwrap();
    assert_eq!(added, "added\n");
    assert_eq!(runs(&count), 2);
}

#[tokio::test]
async fn an_add_that_failed_otherwise_runs_once() {
    let root = tempfile::tempdir().unwrap();
    let count = root.path().join("runs");
    let stderr = "Preparing worktree (new branch 'first-2')\nfatal: 'first-2' already exists";
    let error = run(add(&count, "$n -ge 1", stderr)).await.unwrap_err();
    assert!(format!("{error:#}").contains("already exists"), "{error:#}");
    assert_eq!(runs(&count), 1);
}

#[tokio::test]
async fn an_add_every_prune_undoes_fails_once_its_attempts_run_out() {
    let root = tempfile::tempdir().unwrap();
    let count = root.path().join("runs");
    let error = run(add(&count, "$n -ge 1", PRUNED)).await.unwrap_err();
    assert!(pruned(&error), "{error:#}");
    assert_eq!(runs(&count), ATTEMPTS);
}
