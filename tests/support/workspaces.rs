use crate::common;
use serde_json::Value;
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

pub(crate) struct Fixture {
    pub(crate) root: TempDir,
    pub(crate) repo: PathBuf,
    pub(crate) daemon: Child,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        Self::with_config(None)
    }

    pub(crate) fn with_config(config: Option<&str>) -> Self {
        Self::with_tools(config, false)
    }

    pub(crate) fn with_tools(config: Option<&str>, fake_sim: bool) -> Self {
        assert!(
            Command::new("wt").arg("--version").output().is_ok(),
            "workspace integration tests require Worktrunk (wt)"
        );
        let root = tempfile::tempdir_in("/tmp").unwrap();
        if let Some(config) = config {
            fs::create_dir_all(root.path().join(".config/shoal")).unwrap();
            fs::write(root.path().join(".config/shoal/config.toml"), config).unwrap();
        }
        if fake_sim {
            fs::create_dir(root.path().join("bin")).unwrap();
            let script = root.path().join("bin/xcrun");
            fs::write(&script, include_str!("../fixtures/simctl.py")).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let repo = root.path().join("repo with ' quotes & $literal");
        fs::create_dir(&repo).unwrap();
        let repo = fs::canonicalize(repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("tracked"), "committed\n").unwrap();
        fs::write(repo.join(".gitignore"), "ignored/\n").unwrap();
        git(&repo, &["add", "."]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Shoal Test",
                "-c",
                "user.email=shoal@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        // Model a starting commit already present on a remote, without network I/O.
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let daemon = cli(root.path())
            .args(["daemon", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut fixture = Self { root, repo, daemon };
        fixture.wait_ready();
        fixture.ok(&["repo", "add", fixture.repo.to_str().unwrap()]);
        fixture
    }

    pub(crate) fn add_github_origin(&self) {
        git(
            &self.repo,
            &["remote", "add", "origin", "git@github.com:team/project.git"],
        );
        // Keep forge detection realistic without contacting GitHub for its HEAD.
        git(
            &self.repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
    }

    pub(crate) fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.run(&["daemon", "status"]).status.success() {
            assert!(self.daemon.try_wait().unwrap().is_none(), "daemon exited");
            assert!(Instant::now() < deadline, "daemon startup timeout");
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub(crate) fn restart(&mut self) {
        self.daemon.kill().unwrap();
        self.daemon.wait().unwrap();
        self.daemon = self
            .command()
            .args(["daemon", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.wait_ready();
    }

    pub(crate) fn command(&self) -> Command {
        cli(self.root.path())
    }
    pub(crate) fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    pub(crate) fn ok(&self, args: &[&str]) -> Value {
        let output = self.command().arg("--json").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    pub(crate) fn add(&self, name: &str) -> Value {
        self.ok(&["add", self.repo.to_str().unwrap(), name])
    }

    /// `~/shoal`, the default parent of every repository directory.
    pub(crate) fn shoal_dir(&self) -> PathBuf {
        fs::canonicalize(self.root.path().join("shoal")).unwrap()
    }

    pub(crate) fn interactive(&self, args: &[&str], answer: &str) -> (Output, String) {
        use std::{io::Read, os::fd::FromRawFd, os::unix::process::CommandExt};
        let (mut master, mut slave) = (-1, -1);
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let mut master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        use std::os::fd::AsRawFd;
        assert_ne!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
            -1
        );
        let mut command = self.command();
        // A tracked command needs a controlling terminal to transfer foreground
        // ownership to its child, not just file descriptors that pass isatty.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .args(args)
            .stdin(slave.try_clone().unwrap())
            .stderr(slave)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        master.write_all(answer.as_bytes()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut transcript = Vec::new();
        loop {
            let _ = master.read_to_end(&mut transcript);
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "interactive command timed out: {}",
                    String::from_utf8_lossy(&transcript)
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = master.read_to_end(&mut transcript);
        (
            child.wait_with_output().unwrap(),
            String::from_utf8(transcript).unwrap(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

pub(crate) fn cli(root: &Path) -> Command {
    let mut command = common::isolated(env!("CARGO_BIN_EXE_shoal"));
    command
        .arg("--state-dir")
        .arg(root.join("state"))
        .env("HOME", root)
        .env(
            "PATH",
            format!(
                "{}:{}",
                root.join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env_remove("SHOAL_SHELL_DIRECTIVE")
        // Tests may themselves run inside a Shoal execution.
        .env_remove("SHOAL_SCOPE_TOKEN")
        .env_remove("SHOAL_EXECUTION_ID")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        // Happy tests must never see the developer's login or server.
        .env_remove("HAPPY_HOME_DIR")
        .env_remove("HAPPY_SERVER_URL")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

pub(crate) fn git(repo: &Path, args: &[&str]) -> String {
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
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

pub(crate) fn commit_resource_config(repo: &Path, config: &str) {
    fs::write(repo.join(".shoal.toml"), config).unwrap();
    git(repo, &["add", "."]);
    git(
        repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "resources",
        ],
    );
}

// Local bare origin plus a separate author checkout: no network or personal repos.
pub(crate) fn upstream_remote(fixture: &Fixture) -> PathBuf {
    let branch = git(&fixture.repo, &["branch", "--show-current"]);
    let branch = branch.trim_end_matches('\n');
    let remote = fixture.root.path().join("origin.git");
    git(
        &fixture.repo,
        &[
            "clone",
            "--bare",
            fixture.repo.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    git(
        &fixture.repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&fixture.repo, &["fetch", "origin"]);
    git(
        &fixture.repo,
        &[
            "branch",
            &format!("--set-upstream-to=origin/{branch}"),
            branch,
        ],
    );
    let author = fixture.root.path().join("author");
    git(
        &fixture.repo,
        &["clone", remote.to_str().unwrap(), author.to_str().unwrap()],
    );
    fs::write(author.join("upstream"), "from remote\n").unwrap();
    git(&author, &["add", "upstream"]);
    git(
        &author,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "upstream change",
        ],
    );
    git(&author, &["push", "origin", branch]);
    author
}

pub(crate) fn wait_registered_execution(fixture: &Fixture, workspace: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let inspection = fixture.ok(&["inspect", workspace]);
        if let Some(execution) = inspection["executions"].as_array().unwrap().first() {
            if !execution["child"].is_null() {
                return execution.clone();
            }
        }
        assert!(Instant::now() < deadline, "execution not registered");
        thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) fn wait_removed(fixture: &Fixture, name: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while fixture
        .ok(&["list"])
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["name"] == name)
    {
        assert!(
            Instant::now() < deadline,
            "PR cleanup did not remove workspace: {}",
            fixture.ok(&["inspect", name])
        );
        thread::sleep(Duration::from_millis(30));
    }
}

pub(crate) fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}
