//! Unit-test fixtures; keep their state and repositories under the returned root.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use crate::{daemon::workspace::Manager, paths::Paths};

pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git in {} {args:?}: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

pub fn commit(repo: &Path, file: &str) {
    git(repo, &["add", "--", file]);
    git(
        repo,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "-m",
            file,
        ],
    );
}

pub fn repository(root: &Path, name: &str) -> PathBuf {
    let repo = root.join(name);
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Shoal Test"]);
    git(&repo, &["config", "user.email", "shoal@example.invalid"]);
    fs::write(repo.join("tracked"), "initial\n").unwrap();
    commit(&repo, "tracked");
    repo
}

/// Keep the temporary root alive until all manager operations have finished.
/// Its global config removes eligible workspaces on their first sweep, as
/// reopened managers also read; grace period tests set their own.
pub async fn manager() -> (tempfile::TempDir, Arc<Manager>) {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let paths = Paths::for_test(root.path());
    write_global_config(&paths, "[cleanup]\ngrace_minutes = 0\n");
    let manager = Manager::open(paths).await.unwrap();
    (root, manager)
}

/// [`manager`] in a home whose name Spotlight indexes, unlike `tempfile`'s
/// `.tmp` directories, so the excluded root layout has something to change.
pub async fn indexed_manager() -> (tempfile::TempDir, Arc<Manager>) {
    let root = tempfile::Builder::new()
        .prefix("indexed")
        .tempdir_in("/tmp")
        .unwrap();
    let paths = Paths::for_test(root.path());
    write_global_config(&paths, "[cleanup]\ngrace_minutes = 0\n");
    let manager = Manager::open(paths).await.unwrap();
    (root, manager)
}

/// Replace the global config with `text` and reload it.
pub async fn configure(manager: &Manager, text: &str) {
    write_global_config(&manager.paths, text);
    manager.reload_config().await.unwrap();
}

/// Global config `text` as a published config, read like [`configure`]'s.
pub fn global_config(text: &str) -> crate::config::Config {
    toml::from_str(&format!("{BASE_CONFIG}{text}")).unwrap()
}

/// Global config always turns off `copy_ignored`: only the CLI runs the
/// tracked setup it needs, so workspaces created here are ready at once.
const BASE_CONFIG: &str = "copy_ignored = false\n";

fn write_global_config(paths: &Paths, text: &str) {
    let config = crate::config::Config::path(paths);
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, format!("{BASE_CONFIG}{text}")).unwrap();
}

/// Run a test helper as its own wrapper, without the runner's scope or terminal.
pub fn isolated_test(root: &Path, name: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--ignored", "--nocapture"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("ZDOTDIR", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("SHOAL_TEST_HELPER", "1")
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    command
}

/// Poll through setup and verify that an explicit gate still holds the future.
pub async fn assert_pending(future: impl std::future::Future) {
    tokio::pin!(future);
    std::future::poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "future completed before release"
        );
        std::task::Poll::Ready(())
    })
    .await;
}

/// A shell left running from `dir` after its parent exited, as a recording's
/// setup script is, with a `sleep` it started there. Dropping it kills both.
pub struct Detached {
    pub shell: u32,
    pub child: u32,
}

impl Detached {
    /// Start it, recording its PIDs in `pids`, which must lie outside `dir`.
    pub async fn start(dir: &Path, pids: &Path) -> Self {
        let script = format!(
            "sh -c 'sleep 300 & echo $$ $! > {pids}.tmp; mv {pids}.tmp {pids}; wait' \
             >/dev/null 2>&1 &",
            pids = pids.display()
        );
        let status = tokio::process::Command::new("sh")
            .args(["-c", &script])
            .current_dir(dir)
            .status()
            .await
            .unwrap();
        assert!(status.success());
        // A generous hang guard; the shell writes the file once both run.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !pids.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "detached shell never started"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let text = fs::read_to_string(pids).unwrap();
        let mut fields = text.split_whitespace().map(|pid| pid.parse().unwrap());
        Self {
            shell: fields.next().unwrap(),
            child: fields.next().unwrap(),
        }
    }
}

impl Drop for Detached {
    fn drop(&mut self) {
        for pid in [self.child, self.shell] {
            if crate::process::identity::capture(pid).is_ok_and(|live| live.is_some()) {
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
        }
    }
}
