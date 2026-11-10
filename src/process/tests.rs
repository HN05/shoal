use super::*;
use tokio::process::Command;

#[tokio::test]
async fn in_directory_leaves_out_excluded_processes() {
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new("sleep")
        .arg("60")
        .current_dir(root.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    let named = format!("(PID {pid})");
    let found = in_directory(root.path()).await.unwrap();
    assert!(found.contains(&format!("sleep {named}")), "{found:?}");
    let found = in_directory_except(root.path(), &[pid]).await.unwrap();
    assert!(!found.iter().any(|process| process.ends_with(&named)));
    child.kill().await.unwrap();
}

#[tokio::test]
async fn one_listing_scans_once_for_every_directory() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let users = DirectoryUsers::default();
    assert!(
        users
            .in_directory_except(first.path(), &[])
            .await
            .unwrap()
            .is_empty()
    );
    let mut child = Command::new("sleep")
        .arg("60")
        .current_dir(second.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let named = format!("(PID {})", child.id().unwrap());
    let listed = users.in_directory_except(second.path(), &[]).await.unwrap();
    assert!(
        !listed.iter().any(|process| process.ends_with(&named)),
        "a process started after the listing is not in it"
    );
    let rescanned = in_directory(second.path()).await.unwrap();
    assert!(rescanned.iter().any(|process| process.ends_with(&named)));
    child.kill().await.unwrap();
}

/// A Git command run in `root` that waits for its input, which the caller
/// holds.
fn waiting_git(root: &Path) -> tokio::process::Child {
    Command::new("git")
        .args(["hash-object", "--stdin"])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

#[tokio::test]
async fn own_git_commands_are_told_from_other_processes() {
    let root = tempfile::tempdir().unwrap();
    let mut git = waiting_git(root.path());
    let mut other = Command::new("sleep")
        .arg("60")
        .current_dir(root.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let users = DirectoryUsers::default();
    let found = users.of(root.path()).await.unwrap();
    let own: Vec<_> = (users.own_git(&found).await.unwrap().iter())
        .map(|process| process.pid)
        .collect();
    assert!(own.contains(&git.id().unwrap()), "{own:?}");
    assert!(!own.contains(&other.id().unwrap()), "{own:?}");
    git.kill().await.unwrap();
    other.kill().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn own_git_commands_count_once_the_wait_for_them_ends() {
    let root = tempfile::tempdir().unwrap();
    let mut git = waiting_git(root.path());
    let named = format!("(PID {})", git.id().unwrap());
    let started = tokio::time::Instant::now();
    let found = in_directory_besides(root.path(), async { Ok(Vec::new()) })
        .await
        .unwrap();
    assert!(started.elapsed() >= OWN_GIT_END);
    assert!(
        found.iter().any(|process| process.ends_with(&named)),
        "{found:?}"
    );
    git.kill().await.unwrap();
}

#[tokio::test]
async fn own_git_commands_that_end_do_not_count() {
    let root = tempfile::tempdir().unwrap();
    let mut git = waiting_git(root.path());
    let named = format!("(PID {})", git.id().unwrap());
    let input = git.stdin.take();
    let (found, _) = tokio::join!(
        in_directory_besides(root.path(), async { Ok(Vec::new()) }),
        async {
            drop(input);
            git.wait().await
        }
    );
    let found = found.unwrap();
    assert!(
        !found.iter().any(|process| process.ends_with(&named)),
        "{found:?}"
    );
}
