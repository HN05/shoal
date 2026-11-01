//! Unit-test fixtures; keep their state and repositories under the returned root.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use crate::{
    daemon::workspace::Manager,
    paths::Paths,
    process::identity::{self, Identity},
};

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
    crate::config::Config::from_toml(&format!("{BASE_CONFIG}{text}")).unwrap()
}

/// Global config always turns off `copy_ignored`: only the CLI runs the
/// tracked setup it needs, so workspaces created here are ready at once. It
/// delivers each agent message at once, so a test reads it without waiting
/// for a run of them to settle.
const BASE_CONFIG: &str = "setup.copy_ignored = false\nmessages.settle_seconds = 0\n";

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
        // The helper runs CLI handlers in-process; give its test thread the
        // stack the CLI's runtime threads get, which Linux debug builds need.
        .env("RUST_MIN_STACK", crate::WORKER_STACK.to_string())
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    command
}

/// Install `script` at `path` as a new executable file, renamed over any
/// earlier one. A child process writes it: a test that forks while this
/// binary holds the file open for writing leaves the descriptor in its child
/// until the child execs, and running the script meanwhile fails with "Text
/// file busy".
pub fn install_test_script(path: &Path, script: &str) {
    use std::io::Write as _;
    let staging = tempfile::tempdir_in(path.parent().unwrap()).unwrap();
    let staged = staging.path().join("script");
    let mut writer = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(&staged)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = writer.stdin.take().unwrap();
    input.write_all(script.as_bytes()).unwrap();
    drop(input);
    assert!(
        writer.wait().unwrap().success(),
        "install {}",
        path.display()
    );
    fs::rename(&staged, path).unwrap();
}

/// A shell command that runs until this test binary exits, for at most ten
/// minutes, standing in for a long-running tool: a test killed before it
/// stops the command leaves nothing running.
pub fn until_tests_end() -> String {
    format!(
        "i=0; while [ $i -lt 600 ] && kill -0 {} 2>/dev/null; do sleep 1; i=$((i+1)); done",
        std::process::id()
    )
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
    pub shell: Identity,
    pub child: Identity,
}

impl Detached {
    /// Start it, recording its PIDs in `pids`, which must lie outside `dir`.
    pub async fn start(dir: &Path, pids: &Path) -> Self {
        let script = format!(
            "sh -c 'sleep 300 & echo $$ $! > {pids}.tmp; mv {pids}.tmp {pids}; wait' \
             >/dev/null 2>&1 &",
            pids = pids.display()
        );
        let status = isolated_command("sh", dir)
            .args(["-c", &script])
            .status()
            .await
            .unwrap();
        assert!(status.success());
        Self::recorded(pids).await
    }

    /// The pair whose PIDs a shell started elsewhere records in `pids`,
    /// once it has.
    pub async fn recorded(pids: &Path) -> Self {
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
        let mut identities = text
            .split_whitespace()
            .map(|pid| identity::capture(pid.parse().unwrap()).unwrap().unwrap());
        Self {
            shell: identities.next().unwrap(),
            child: identities.next().unwrap(),
        }
    }
}

impl Drop for Detached {
    fn drop(&mut self) {
        for process in [&self.child, &self.shell] {
            let _ = identity::signal(process, libc::SIGKILL);
        }
    }
}

/// A process started from a shell that exits at once, so launchd or init
/// adopts it. Dropping it kills it.
pub struct Adopted {
    pub identity: Identity,
}

impl Adopted {
    /// Run `command` in `dir` with `env`, recording its PID in `pid`, which
    /// must lie outside the directories a test looks for processes in.
    pub async fn start(dir: &Path, command: &str, env: &[(&str, &str)], pid: &Path) -> Self {
        let script = format!(
            "{command} >/dev/null 2>&1 & echo $! > {pid}.tmp; mv {pid}.tmp {pid}",
            pid = pid.display()
        );
        let status = isolated_command("sh", dir)
            .envs(env.iter().copied())
            .args(["-c", &script])
            .status()
            .await
            .unwrap();
        assert!(status.success());
        let pid: u32 = fs::read_to_string(pid).unwrap().trim().parse().unwrap();
        let adopted = Self {
            identity: identity::capture(pid).unwrap().unwrap(),
        };
        adopted.await_exec(&script).await;
        adopted
    }

    /// Wait until the process runs its command rather than the shell that
    /// forked it, whose command line is `script`.
    async fn await_exec(&self, script: &str) {
        // A generous hang guard; the fork execs the command at once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let described = crate::process::described::describe(&[self.identity.pid])
                .await
                .unwrap();
            let command = &described[&self.identity.pid].command;
            if !command.ends_with(script) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{command} never ran its command"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

impl Drop for Adopted {
    fn drop(&mut self) {
        let _ = identity::signal(&self.identity, libc::SIGKILL);
    }
}

/// Copy this test binary to `path`. A child copies it, so parallel spawns
/// cannot inherit its writable descriptor and cause ETXTBSY when a test
/// executes the copy.
pub async fn copy_test_binary(path: &Path) {
    let dir = path.parent().unwrap();
    let output = isolated_command("cp", dir)
        .arg(std::env::current_exe().unwrap())
        .arg(path)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "copy the test binary: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A command running `program`, a copy of this test binary, until stopped.
/// Its environment stays readable once launchd adopts it, which a platform
/// binary such as `sleep` hides on macOS.
pub fn runs_until_stopped(program: &Path) -> String {
    format!("'{}' {}", program.display(), RUN_UNTIL_STOPPED.join(" "))
}

/// This test binary running until stopped in `dir`, started without a shell,
/// so a survey never finds the shell, whose environment macOS hides, in its
/// place.
pub fn run_until_stopped_in(dir: &Path) -> tokio::process::Command {
    let program = std::env::current_exe().unwrap();
    let mut command = isolated_command(program.to_str().unwrap(), dir);
    command.args(RUN_UNTIL_STOPPED);
    command
}

/// The arguments that make this test binary, or a copy, run until stopped.
const RUN_UNTIL_STOPPED: [&str; 3] = ["--exact", "test_support::run_until_stopped", "--ignored"];

#[test]
#[ignore = "helper process tests start"]
fn run_until_stopped() {
    std::thread::sleep(std::time::Duration::from_secs(300));
}

/// This test binary's `marked_child` helper, which waits 30 seconds, for a
/// test to give its environment before [`spawn_marked_child`] starts it.
pub fn marked_child() -> tokio::process::Command {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "process::identity::tests::marked_child",
        "--ignored",
    ]);
    command
}

/// Start `command` from [`marked_child`] and return once the helper runs its
/// own image. Linux resumes a `posix_spawn` caller before the child leaves
/// the caller's memory, so until then the child's environment reads as the
/// caller's. The helper prints libtest's header only after that.
pub fn spawn_marked_child(command: &mut tokio::process::Command) -> tokio::process::Child {
    use std::io::Read as _;
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap().into_owned_fd().unwrap();
    fs::File::from(stdout).read_exact(&mut [0]).unwrap();
    child
}

/// A process a test started as the leader of its own process group.
/// Dropping it kills the group, so neither the process nor what it started
/// outlives a test that fails or times out.
pub struct ProcessGroup {
    leader: std::process::Child,
    /// Once the leader is reaped, its ID may name another group.
    reaped: bool,
}

impl ProcessGroup {
    pub fn spawn(command: &mut Command) -> Self {
        use std::os::unix::process::CommandExt as _;
        Self {
            leader: command.process_group(0).spawn().unwrap(),
            reaped: false,
        }
    }

    pub fn id(&self) -> u32 {
        self.leader.id()
    }

    /// Wait for the leader to exit on its own or by another's signal.
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.leader.wait()?;
        self.reaped = true;
        Ok(status)
    }

    /// Kill the group and wait for its leader.
    pub fn kill(self) {
        drop(self);
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // SAFETY: the unreaped leader keeps its group's ID from naming another.
        unsafe {
            libc::kill(-(self.leader.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.leader.wait();
    }
}

/// `program` run from `dir` without the runner's Shoal variables or
/// configuration locations.
pub fn isolated_command(program: &str, dir: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    command
}

impl Manager {
    /// Move the repository's checkout to `path` and record it there, as an
    /// earlier version may have registered it.
    pub async fn move_checkout(&self, id: &str, path: &Path) -> anyhow::Result<()> {
        let repo = self.repository(id).await?;
        fs::rename(&repo.path, path)?;
        let (id, path) = (repo.id, path.to_path_buf());
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE repositories SET path=?2 WHERE id=?1",
                    rusqlite::params![id, path.to_str()],
                )?;
                Ok(())
            })
            .await
    }

    /// Write the repository's local config file, or remove it for `None`.
    pub async fn write_local_config(
        &self,
        selector: &str,
        toml: Option<String>,
    ) -> anyhow::Result<std::path::PathBuf> {
        let path = self.local_config_path(selector).await?;
        match toml {
            Some(text) => {
                crate::config::repo::parse(&text)?;
                fs::create_dir_all(path.parent().unwrap())?;
                fs::write(&path, text)?;
            }
            None => match fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            },
        }
        Ok(path)
    }
}
