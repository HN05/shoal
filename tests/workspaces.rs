#[path = "support/pty.rs"]
mod pty;

#[path = "support/daemon.rs"]
mod daemon_fixture;
use daemon_fixture::DaemonGuard;

#[path = "support/git.rs"]
mod git_fixture;
use git_fixture::{git, init_repo};

mod support;

use support::cli;

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

struct Fixture {
    daemon: DaemonGuard,
    root: TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::with_config(None)
    }

    fn with_config(config: Option<&str>) -> Self {
        Self::with_tools(config, false)
    }

    fn with_tools(config: Option<&str>, fake_sim: bool) -> Self {
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
            fs::write(&script, include_str!("fixtures/simctl.py")).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let repo = init_repo(
            root.path(),
            "repo with ' quotes & $literal",
            &[("tracked", "committed\n"), (".gitignore", "ignored/\n")],
        );
        // Model a starting commit already present on a remote, without network I/O.
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let daemon = DaemonGuard::start(root.path(), &mut cli(root.path()));
        let fixture = Self { root, repo, daemon };
        fixture.ok(&["repo", "add", fixture.repo.to_str().unwrap()]);
        fixture
    }

    fn add_github_origin(&self) {
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

    fn restart(&mut self) {
        self.daemon
            .restart(self.root.path(), &mut cli(self.root.path()));
    }

    fn command(&self) -> Command {
        cli(self.root.path())
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let output = self.command().arg("--json").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn add(&self, name: &str) -> Value {
        self.ok(&["add", self.repo.to_str().unwrap(), name])
    }

    /// `~/shoal`, the default parent of every repository directory.
    fn shoal_dir(&self) -> PathBuf {
        fs::canonicalize(self.root.path().join("shoal")).unwrap()
    }

    fn interactive(&self, args: &[&str], answer: &str) -> (Output, String) {
        use std::{io::Read, os::unix::process::CommandExt};
        let (mut master, slave) = pty::open();
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
            .stderr(slave.try_clone().unwrap())
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

#[test]
fn named_workspace_uses_committed_history_and_deletes_redundant_branch() {
    let fixture = Fixture::new();
    let first = fixture.ok(&["repo", "list"]);
    fixture.ok(&["repo", "add", fixture.repo.to_str().unwrap()]);
    assert_eq!(fixture.ok(&["repo", "list"]), first);
    fs::write(fixture.repo.join("tracked"), "uncommitted source\n").unwrap();
    let workspace = fixture.add("fix-login");
    let path = Path::new(workspace["path"].as_str().unwrap());
    assert_eq!(path.file_name().unwrap(), "fix-login");
    assert_eq!(
        fs::read_to_string(path.join("tracked")).unwrap(),
        "committed\n"
    );
    fs::create_dir(path.join("ignored")).unwrap();
    fs::write(path.join("ignored/cache"), "disposable").unwrap();
    fixture.ok(&["rm", "fix-login"]);
    assert!(!path.exists());
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                &format!("refs/heads/{}", workspace["branch"].as_str().unwrap())
            ]
        ),
        ""
    );
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    assert_eq!(
        fs::read_to_string(fixture.repo.join("tracked")).unwrap(),
        "uncommitted source\n"
    );
}

#[test]
fn url_registration_clones_once_and_supports_workspaces() {
    let fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let repo = fixture.ok(&["repo", "add", &url]);
    assert_ne!(
        repo["path"].as_str().unwrap(),
        fixture.repo.to_str().unwrap()
    );
    // Clones live at `~/shoal/<repo>/.checkout` beside that repository's workspaces.
    let clone = Path::new(repo["path"].as_str().unwrap());
    let directory = clone.parent().unwrap();
    assert_eq!(clone.file_name().unwrap(), ".checkout");
    assert_eq!(repo["workspaces_dir"], directory.to_str().unwrap());
    assert_eq!(directory.parent().unwrap(), fixture.shoal_dir());
    assert!(!fixture.root.path().join("state/repositories").exists());
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
    let workspace = fixture.ok(&["add", &url, "cloned"]);
    assert_eq!(
        Path::new(workspace["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        directory
    );
    fixture.ok(&["rm", "cloned"]);
    assert!(clone.is_dir());
    // No workspace name can collide with the clone's directory.
    let main = fixture.ok(&["add", &url, "main"]);
    assert_eq!(main["path"], directory.join("main").to_str().unwrap());
    assert_eq!(main["branch"], "main-2");
}

#[test]
fn url_registration_drops_trailing_slashes_from_the_clone_origin() {
    let fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let repo = fixture.ok(&["repo", "add", &format!("{url}//")]);
    assert_eq!(repo["source"], url);
    let clone = Path::new(repo["path"].as_str().unwrap());
    assert_eq!(git(clone, &["remote", "get-url", "origin"]).trim(), url);
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
}

#[test]
fn displayed_repository_name_resolves_old_uuid_clones_and_rejects_ambiguity() {
    let fixture = Fixture::new();
    let mut clones = Vec::new();
    for parent in ["one", "two"] {
        let source = fixture.root.path().join(parent).join("saldoir-server.git");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        git(
            &fixture.repo,
            &["clone", "--bare", ".", source.to_str().unwrap()],
        );
        let url = format!("file://{}", source.display());
        let destination = fixture.root.path().join(format!("old-uuid-{parent}"));
        clones.push(fixture.ok(&["repo", "add", &url, "--path", destination.to_str().unwrap()]));
        if parent == "one" {
            let listing = fixture.run(&["repo", "list"]);
            assert!(String::from_utf8_lossy(&listing.stdout).contains("saldoir-server  file://"));
            fixture.ok(&["add", "saldoir-server", "feature"]);
        }
    }
    let ambiguous = fixture.run(&["repo", "rm", "saldoir-server", "--yes"]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("repository name is ambiguous"));
    for repo in &clones {
        assert!(Path::new(repo["path"].as_str().unwrap()).exists());
    }
    // An explicit name wins over a colliding inferred name.
    fixture.ok(&[
        "repo",
        "rename",
        clones[1]["id"].as_str().unwrap(),
        "saldoir-server",
    ]);
    fixture.ok(&["repo", "rm", "saldoir-server", "--yes"]);
    assert!(!Path::new(clones[1]["path"].as_str().unwrap()).exists());
    assert!(Path::new(clones[0]["path"].as_str().unwrap()).exists());
    // The remaining inferred name now resolves, deleting its workspace too.
    fixture.ok(&["repo", "rm", "saldoir-server", "--yes"]);
    assert!(!Path::new(clones[0]["path"].as_str().unwrap()).exists());
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    assert!(fixture.repo.exists());
}

#[test]
fn live_completion_uses_targets_state_override_workspace_context_and_scope() {
    let fixture = Fixture::with_config(Some(RESOURCE_CONFIG));
    fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    let first = fixture.add("first");
    fixture.add("second");
    fixture.ok(&["port", "acquire", "web", "first"]);
    fixture.ok(&["resource", "acquire", "devices", "first", "--name", "tests"]);
    let complete = |args: &[&str], cwd: &Path| {
        let state = fixture.root.path().join("state");
        let mut words = vec!["shoal", "--state-dir", state.to_str().unwrap()];
        words.extend_from_slice(args);
        let output = fixture
            .command()
            .arg("--")
            .args(&words)
            .env("SHOAL_COMPLETE", "bash")
            .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
            .env("SHOAL_STATE_DIR", fixture.root.path().join("wrong-state"))
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    for args in [
        vec!["repo", "rm", "pr"],
        vec!["repo", "config", "pr"],
        vec!["repo", "rename", "pr"],
        vec!["add", "pr"],
        vec!["issue", "103", "--repo", "pr"],
    ] {
        assert!(
            complete(&args, fixture.root.path()).contains(&"project".into()),
            "{args:?}"
        );
    }
    for command in ["rm", "cd", "exec", "diff", "status", "inspect"] {
        assert!(
            complete(&[command, "fi"], fixture.root.path()).contains(&"first".into()),
            "{command}"
        );
    }
    let choices = complete(&["repo", "rm", ""], fixture.root.path());
    let target = choices.iter().position(|v| v == "project").unwrap();
    assert!(
        choices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.starts_with('-'))
            .all(|(i, _)| i > target)
    );
    let cwd = Path::new(first["path"].as_str().unwrap());
    fs::write(cwd.join(".shoal.toml"), "[commands]\nreview = ['tuicr']\n").unwrap();
    // Complete the command name with an unknown workspace already on the line.
    let names = fixture
        .command()
        .env("SHOAL_STATE_DIR", fixture.root.path().join("state"))
        .args(["--", "shoal", "run", "rev", "unknown"])
        .env("SHOAL_COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", "2")
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(names.status.success(), "{names:?}");
    assert!(
        String::from_utf8_lossy(&names.stdout)
            .lines()
            .any(|line| line == "review")
    );
    assert!(complete(&["rev"], cwd).contains(&"review".into()));
    assert!(complete(&["review", "fi"], cwd).contains(&"first".into()));
    assert!(complete(&["review", "fi"], fixture.root.path()).contains(&"first".into()));
    assert!(complete(&["port", "release", "w"], cwd).contains(&"web".into()));
    assert!(complete(&["resource", "acquire", "d"], cwd).contains(&"devices".into()));
    assert!(
        complete(
            &["resource", "acquire", "devices", "first", "--resource", "b"],
            fixture.root.path()
        )
        .contains(&"beta".into())
    );
    assert!(
        complete(
            &["resource", "release", "devices", "first", "--name", "t"],
            fixture.root.path()
        )
        .contains(&"tests".into())
    );
    assert!(
        complete(
            &[
                "resource",
                "acquire",
                "devices",
                first["id"].as_str().unwrap(),
                "--resource",
                "b"
            ],
            fixture.root.path(),
        )
        .contains(&"beta".into())
    );
    assert!(
        !complete(
            &[
                "resource",
                "acquire",
                "devices",
                "unknown",
                "--resource",
                "b"
            ],
            cwd,
        )
        .contains(&"beta".into())
    );
    fixture.ok(&["rm", "second"]);
    assert!(!complete(&["rm", ""], cwd).contains(&"second".into()));
    fixture.add("second");
    let scoped = fixture.run(&[
        "exec",
        "first",
        "--",
        "env",
        "SHOAL_COMPLETE=bash",
        "_CLAP_COMPLETE_INDEX=2",
        env!("CARGO_BIN_EXE_shoal"),
        "--",
        "shoal",
        "rm",
        "",
    ]);
    assert!(
        scoped.status.success(),
        "{}",
        String::from_utf8_lossy(&scoped.stderr)
    );
    let text = String::from_utf8(scoped.stdout).unwrap();
    assert!(text.lines().any(|line| line == "first"), "{text}");
    assert!(!text.lines().any(|line| line == "second"), "{text}");
    assert!(!fixture.root.path().join("wrong-state").exists());
}

#[test]
fn workspace_context_adapters_preserve_directory_scope_and_picker_policy() {
    let fixture = Fixture::new();
    let own = fixture.add("context-own");
    let other = fixture.add("context-other");
    let own_path = Path::new(own["path"].as_str().unwrap());
    let other_path = Path::new(other["path"].as_str().unwrap());
    for (path, name) in [(own_path, "own-only"), (other_path, "other-only")] {
        fs::write(
            path.join(".shoal.toml"),
            format!("[commands]\n{name} = ['true']\n"),
        )
        .unwrap();
    }
    let nested = own_path.join("nested/deep");
    fs::create_dir_all(&nested).unwrap();
    let alias = fixture.root.path().join("alias");
    std::os::unix::fs::symlink(&nested, &alias).unwrap();

    let command = |cwd: &Path, scoped: bool, completion: bool| {
        if scoped {
            let mut command = fixture.command();
            command
                .args([
                    "exec",
                    "context-own",
                    "--",
                    "sh",
                    "-c",
                    "cd \"$1\"; shift; exec \"$@\"",
                    "context-test",
                ])
                .arg(cwd)
                .arg("env");
            // Completion variables belong to the inner process, after scope delivery.
            if completion {
                command.args(["SHOAL_COMPLETE=bash", "_CLAP_COMPLETE_INDEX=2"]);
            }
            command.arg(env!("CARGO_BIN_EXE_shoal"));
            command
        } else {
            let mut command = fixture.command();
            command.current_dir(cwd);
            if completion {
                command
                    .env("SHOAL_COMPLETE", "bash")
                    .env("_CLAP_COMPLETE_INDEX", "2")
                    .env("SHOAL_STATE_DIR", fixture.root.path().join("state"));
            }
            command
        }
    };
    for (cwd, scoped) in [
        (nested.as_path(), false),
        (alias.as_path(), false),
        (fixture.root.path(), true),
        (other_path, true),
    ] {
        for args in [vec!["--json", "run"], vec!["--json", "config", "show"]] {
            let output = command(cwd, scoped, false).args(&args).output().unwrap();
            assert!(output.status.success(), "{args:?}: {output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.contains("own-only"), "{text}");
            assert!(!text.contains("other-only"), "{text}");
        }
        let output = command(cwd, scoped, false)
            .args(["--json", "status"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let status: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(status["workspace"]["id"], own["id"]);

        let output = command(cwd, scoped, true)
            .args(["--", "shoal", "run", "own"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == "own-only")
        );
    }
    for args in [
        vec!["status", "context-other"],
        vec!["config", "show", "context-other"],
    ] {
        let output = command(other_path, true, false)
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("scope"),
            "{output:?}"
        );
    }
    for args in [vec!["inspect"], vec!["cd"]] {
        let output = command(&nested, false, false).args(&args).output().unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("non-interactive"));
    }
    let output = command(fixture.root.path(), false, false)
        .args(["--json", "run"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("own-only"));
    let output = command(fixture.root.path(), false, false)
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("no current workspace or registered checkout")
    );
}

#[test]
fn status_summarizes_current_workspace_work_and_supports_json() {
    let config = format!("{RESOURCE_CONFIG}\n[pr_cleanup]\nenabled=false\n");
    let fixture = Fixture::with_config(Some(&config));
    let workspace = fixture.add("summary");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("tracked"), "changed\nagain\n").unwrap();
    fs::write(path.join("added"), "new\n").unwrap();
    git(path, &["add", "added"]);
    fixture.ok(&["port", "acquire", "web", "summary"]);
    fixture.ok(&[
        "resource", "acquire", "devices", "summary", "--name", "tests",
    ]);
    fixture.ok(&["resource", "acquire", "signing", "summary"]);
    fixture.add("waiter");
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "signing", "waiter"])
            .status
            .code(),
        Some(2)
    );
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
        rusqlite::params![
            workspace["id"].as_str().unwrap(),
            serde_json::json!({
                "url": "https://forge.example/team/repo/pulls/7",
                "head": null,
                "error": null
            })
            .to_string()
        ],
    )
    .unwrap();

    let started = fixture.root.path().join("status-started");
    let finish = fixture.root.path().join("status-finish");
    let mut execution = fixture
        .command()
        .args([
            "exec",
            "summary",
            "--",
            "sh",
            "-c",
            "touch \"$1\"; while test ! -f \"$2\"; do sleep 0.02; done",
            "status-test",
            started.to_str().unwrap(),
            finish.to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started.exists() {
        assert!(Instant::now() < deadline, "execution did not start");
        thread::sleep(Duration::from_millis(20));
    }

    let output = fixture
        .command()
        .args(["--json", "status"])
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(status.get("inspection").is_none());
    assert_eq!(status["workspace"]["name"], "summary");
    assert_eq!(status["workspace"]["branch"], "summary");
    assert_eq!(status["workspace"]["state"], "ready");
    assert_eq!(status["setup_finished"], true);
    assert_eq!(
        status["diff"],
        serde_json::json!({"files_changed": 2, "insertions": 3, "deletions": 1})
    );
    assert_eq!(status["executions"].as_array().unwrap().len(), 1);
    assert_eq!(status["ports"].as_array().unwrap().len(), 1);
    assert_eq!(status["resources"].as_array().unwrap().len(), 2);
    assert_eq!(status["simulators"], serde_json::json!([]));
    assert_eq!(
        status["pr_cleanup"]["url"],
        "https://forge.example/team/repo/pulls/7"
    );
    assert_eq!(status["unread_notifications"], 1);

    let text = fixture.run(&["status", "summary"]);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    for expected in [
        "summary  ready",
        "Branch:        summary",
        "Setup:         finished",
        "Changes:       2 files, +3 -1",
        "Executions:    1",
        "Ports:         1",
        "Simulators:    0",
        "Resources:     2",
        "PR watch:      https://forge.example/team/repo/pulls/7",
        "Notifications: 1 unread",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in {text:?}");
    }
    let missing = fixture.run(&["status"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr)
            .contains("missing argument; pass an explicit target/name")
    );

    fs::write(finish, "done").unwrap();
    assert!(execution.wait().unwrap().success());
}

#[test]
fn status_keeps_shared_state_when_the_worktree_is_missing() {
    let fixture = Fixture::with_config(Some("[auto_cleanup]\nenabled=false\n"));
    let workspace = fixture.add("missing-status");
    fixture.ok(&["port", "acquire", "web", "missing-status"]);
    fs::remove_dir_all(workspace["path"].as_str().unwrap()).unwrap();

    let status = fixture.ok(&["status", "missing-status"]);
    assert_eq!(status["workspace"]["name"], "missing-status");
    assert_eq!(status["ports"].as_array().unwrap().len(), 1);
    assert!(status["diff"].is_null());
    assert!(!status["diff_error"].as_str().unwrap().is_empty());

    let text = fixture.run(&["status", "missing-status"]);
    assert!(text.status.success());
    assert!(
        String::from_utf8(text.stdout)
            .unwrap()
            .contains("Changes:       unavailable")
    );
}

#[test]
fn local_repository_without_remotes_registers_in_place_and_creates_workspaces() {
    let fixture = Fixture::new();
    git(
        &fixture.repo,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    assert!(git(&fixture.repo, &["remote"]).is_empty());
    let repo = fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "local",
    ]);
    assert_eq!(repo["path"], fixture.repo.to_str().unwrap());
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
    let workspace = fixture.ok(&["add", "local", "offline"]);
    assert_eq!(
        git(
            Path::new(workspace["path"].as_str().unwrap()),
            &["rev-parse", "HEAD"]
        ),
        git(&fixture.repo, &["rev-parse", "main"])
    );
    // In-place checkouts stay put; only their workspaces gather under `~/shoal/<repo>`.
    assert_eq!(
        Path::new(workspace["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        fixture.shoal_dir().join("repo-with---quotes----literal")
    );
    assert!(!fixture.root.path().join("state/repositories").exists());
    let unused = fixture.root.path().join("unused");
    let output = fixture.run(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--path",
        unused.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("local repositories are registered in place")
    );
    assert!(!unused.exists());
}

#[test]
fn placed_checkouts_adopt_their_directory_but_never_another_checkout() {
    let fixture = Fixture::new();
    let init = |path: &Path| {
        fs::create_dir_all(path).unwrap();
        git(path, &["init", "-b", "main"]);
        fs::write(path.join("file"), "x").unwrap();
        git(path, &["add", "."]);
        git(
            path,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-m",
                "i",
            ],
        );
        fixture.ok(&["repo", "add", path.to_str().unwrap()])
    };
    let shoal = fixture.root.path().join("shoal");
    // `~/shoal/placed/main` keeps `~/shoal/placed` for its workspaces.
    let placed = init(&shoal.join("placed/main"));
    assert_eq!(
        placed["workspaces_dir"],
        fixture.shoal_dir().join("placed").to_str().unwrap()
    );
    // A checkout directly under the root is not a repository directory...
    let outer = init(&shoal.join("outer"));
    assert_ne!(
        outer["workspaces_dir"],
        fixture.shoal_dir().to_str().unwrap()
    );
    // ...and a checkout nested in it must not put its workspaces inside `outer`.
    let inner = init(&shoal.join("outer/inner"));
    let inner_dir = Path::new(inner["workspaces_dir"].as_str().unwrap());
    assert_ne!(inner_dir, fixture.shoal_dir().join("outer"));
    assert!(inner_dir.starts_with(fixture.shoal_dir()));
    assert!(!inner_dir.starts_with(fixture.shoal_dir().join("outer")));
}

#[test]
fn root_directory_inside_state_or_a_checkout_is_refused() {
    let mut fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let config = fixture.root.path().join(".config/shoal/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let outer = fixture.root.path().join("outer");
    fs::create_dir(&outer).unwrap();
    git(&outer, &["init", "-b", "main"]);
    let outer = outer.display().to_string();
    // A root inside the state directory or a registered checkout refuses every
    // registration; one inside the checkout being registered refuses that one.
    for (root, message, sources) in [
        ("~/state/worktrees", "state directory", vec![&url, &outer]),
        (
            "~/missing/../state/worktrees",
            "state directory",
            vec![&url, &outer],
        ),
        (
            "~/repo with ' quotes & $literal/worktrees",
            "repository checkout",
            vec![&url, &outer],
        ),
        ("~/outer/worktrees", "repository checkout", vec![&outer]),
        (
            "~/missing/../outer/worktrees",
            "repository checkout",
            vec![&outer],
        ),
    ] {
        fs::write(&config, format!("root_dir = {root:?}\n")).unwrap();
        fixture.restart();
        for source in sources {
            let output = fixture.run(&["repo", "add", source]);
            assert!(!output.status.success(), "{root}: {source} was registered");
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        }
        assert!(!fixture.root.path().join(&root[2..]).exists());
        assert!(!fixture.root.path().join("missing").exists());
        assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
    }
    // The already placed repository still creates workspaces in its own directory.
    let workspace = fixture.add("still-works");
    assert!(Path::new(workspace["path"].as_str().unwrap()).starts_with(fixture.shoal_dir()));
}

#[test]
fn root_directory_normalizes_missing_components_before_creation() {
    let fixture = Fixture::with_config(Some("root_dir = \"~/missing/../clones\"\n"));
    let workspace = fixture.add("normalized-root");
    let root = fs::canonicalize(fixture.root.path()).unwrap();
    assert!(Path::new(workspace["path"].as_str().unwrap()).starts_with(root.join("clones")));
    assert!(!root.join("missing").exists());
}

#[test]
fn clone_directories_use_repo_names_and_suffix_occupied_or_recorded_paths() {
    let fixture = Fixture::with_config(Some("root_dir = \"~/clones\"\n"));
    let directory = fixture.root.path().join("clones");
    fs::write(directory.join("project"), "keep this file").unwrap();
    std::os::unix::fs::symlink("missing-target", directory.join("project-2")).unwrap();
    let mut repos = Vec::new();
    for index in 0..3 {
        let parent = fixture.root.path().join(format!("source-{index}"));
        fs::create_dir(&parent).unwrap();
        let source = parent.join("project.git");
        git(
            &fixture.repo,
            &["clone", "--bare", ".", source.to_str().unwrap()],
        );
        let url = format!("file://{}", source.display());
        let repo = fixture.ok(&["repo", "add", &url]);
        let clone = Path::new(repo["path"].as_str().unwrap());
        assert_eq!(clone.file_name().unwrap(), ".checkout");
        assert_eq!(
            clone
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            format!("project-{}", index + 3)
        );
        assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
        repos.push(repo);
        if index == 0 {
            // A lost directory must retain its path reservation in Shoal's registry.
            fs::remove_dir_all(repos[0]["workspaces_dir"].as_str().unwrap()).unwrap();
        }
    }
    assert_eq!(
        fs::read_to_string(directory.join("project")).unwrap(),
        "keep this file"
    );
    assert_eq!(
        fs::read_link(directory.join("project-2")).unwrap(),
        PathBuf::from("missing-target")
    );
    assert!(!directory.join("project-3").exists());
    let url = format!("file://{}", fixture.repo.display());
    let named = fixture.ok(&["repo", "add", &url, "--name", "chosen"]);
    assert_eq!(
        Path::new(named["path"].as_str().unwrap())
            .parent()
            .unwrap()
            .file_name()
            .unwrap(),
        "chosen"
    );
    let renamed = fixture.ok(&["repo", "rename", "chosen", "new-label"]);
    assert_eq!(renamed["path"], named["path"]);
    assert_eq!(renamed["workspaces_dir"], named["workspaces_dir"]);
}

#[test]
fn clone_name_allocation_is_atomic_across_daemons_sharing_a_directory() {
    let shared = tempfile::tempdir_in("/tmp").unwrap();
    let config = format!("root_dir = {:?}", shared.path().to_str().unwrap());
    let first = Fixture::with_config(Some(&config));
    let second = Fixture::with_config(Some(&config));
    let clone = |fixture: &Fixture| {
        let source = fixture.root.path().join("project.git");
        git(
            &fixture.repo,
            &["clone", "--bare", ".", source.to_str().unwrap()],
        );
        fixture.ok(&["repo", "add", &format!("file://{}", source.display())])
    };
    let (one, two) = thread::scope(|scope| {
        let one = scope.spawn(|| clone(&first));
        let two = scope.spawn(|| clone(&second));
        (one.join().unwrap(), two.join().unwrap())
    });
    let mut names = [one, two].map(|repo| {
        Path::new(repo["workspaces_dir"].as_str().unwrap())
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    });
    names.sort();
    assert_eq!(names, ["project", "project-2"]);
    assert!(shared.path().join("project/.checkout/.git").exists());
    assert!(shared.path().join("project-2/.checkout/.git").exists());
}

#[test]
fn configured_root_directory_affects_new_repositories_and_preserves_existing_paths() {
    let mut fixture =
        Fixture::with_config(Some("root_dir = \"~/clones with ' quotes & $literal\"\n"));
    let url = format!("file://{}", fixture.repo.display());
    let repo = fixture.ok(&["repo", "add", &url]);
    let path = Path::new(repo["path"].as_str().unwrap());
    assert_eq!(
        path.parent().unwrap().parent().unwrap(),
        fs::canonicalize(fixture.root.path().join("clones with ' quotes & $literal")).unwrap()
    );
    assert!(!fixture.root.path().join("state/repositories").exists());
    let new_root = fixture.root.path().join("new clones");
    fs::write(
        fixture.root.path().join(".config/shoal/config.toml"),
        format!("root_dir = {:?}\n", new_root.to_str().unwrap()),
    )
    .unwrap();
    fixture.restart();
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
    let retained = fixture.ok(&["add", &url, "retained"]);
    assert_eq!(
        Path::new(retained["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        path.parent().unwrap()
    );
    assert!(!new_root.exists());
    let source = fixture.root.path().join("second.git");
    git(
        &fixture.repo,
        &["clone", "--bare", ".", source.to_str().unwrap()],
    );
    let second = fixture.ok(&["repo", "add", &format!("file://{}", source.display())]);
    assert_eq!(
        Path::new(second["path"].as_str().unwrap())
            .parent()
            .unwrap()
            .parent()
            .unwrap(),
        fs::canonicalize(new_root).unwrap()
    );
}

#[test]
fn repository_clone_path_overrides_default_and_resolves_in_callers_directory() {
    let fixture = Fixture::with_config(Some("root_dir = \"~/default-clones\"\n"));
    let url = format!("file://{}", fixture.repo.display());
    let relative = "projects/a repo with ' quotes & $literal";
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args(["--json", "repo", "add", &url, "--path", relative])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repo: Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected = fs::canonicalize(fixture.root.path().join(relative)).unwrap();
    assert_eq!(repo["path"], expected.to_str().unwrap());
    // The repository directory is still reserved for its workspaces.
    let directory = Path::new(repo["workspaces_dir"].as_str().unwrap());
    assert_eq!(
        directory.parent().unwrap(),
        fs::canonicalize(fixture.root.path().join("default-clones")).unwrap()
    );
    assert!(fs::read_dir(directory).unwrap().next().is_none());
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
    assert_eq!(
        fixture.ok(&["repo", "add", &url, "--path", &format!("~/{relative}")]),
        repo
    );
    let mismatch = fixture.root.path().join("other");
    let output = fixture.run(&[
        "repo",
        "add",
        &url,
        "--path",
        mismatch.to_str().unwrap(),
        "--name",
        "wrong",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot relocate"));
    assert!(!mismatch.exists());
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
    fixture.ok(&["add", &url, "custom-clone"]);
}

#[test]
fn clone_path_preserves_existing_destinations_and_cleans_only_failed_new_clones() {
    let fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let occupied = fixture.root.path().join("occupied");
    fs::create_dir(&occupied).unwrap();
    let file = occupied.join("keep");
    fs::write(&file, "user data").unwrap();
    let empty = fixture.root.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let link = fixture.root.path().join("link");
    std::os::unix::fs::symlink(&occupied, &link).unwrap();
    for destination in [&occupied, &empty, &file, &link] {
        let output = fixture.run(&["repo", "add", &url, "--path", destination.to_str().unwrap()]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("destination must not already exist")
        );
        assert!(destination.exists());
        assert_eq!(fs::read_to_string(&file).unwrap(), "user data");
    }
    let failed = fixture.root.path().join("failed clone");
    let missing = format!(
        "file://{}",
        fixture.root.path().join("missing.git").display()
    );
    assert!(
        !fixture
            .run(&["repo", "add", &missing, "--path", failed.to_str().unwrap()])
            .status
            .success()
    );
    assert!(!failed.exists());
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
}

#[test]
fn registration_reuses_repositories_by_origin_across_paths_and_url_forms() {
    let fixture = Fixture::new();
    let original = fixture.ok(&["repo", "list"])[0].clone();
    let url = "https://example.invalid/team/project.git";
    let ssh_url = "git@example.invalid:team/project.git";
    git(&fixture.repo, &["remote", "add", "origin", url]);
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let other = fixture.root.path().join("other-checkout");
    git(
        &fixture.repo,
        &["clone", "--local", ".", other.to_str().unwrap()],
    );
    git(&other, &["remote", "set-url", "origin", ssh_url]);
    assert_eq!(
        fixture.ok(&["repo", "add", other.to_str().unwrap()]),
        original
    );
    assert_eq!(fixture.ok(&["repo", "add", ssh_url]), original);
    assert_eq!(fixture.ok(&["repo", "add", url]), original);
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
    // This remote tests identity matching only; explicitly use local history.
    fixture.ok(&["add", ssh_url, "alias", "--base", "HEAD"]);
    fixture.ok(&["rm", "alias"]);
}

#[test]
fn removal_accepts_switched_branch_but_refuses_replaced_repository() {
    let fixture = Fixture::new();
    let workspace = fixture.add("switched");
    let path = Path::new(workspace["path"].as_str().unwrap());
    git(path, &["switch", "-c", "my-branch"]);
    fixture.ok(&["rm", "switched"]);
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/my-branch"
            ]
        ),
        ""
    );

    let workspace = fixture.add("replaced");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let moved = fixture.root.path().join("original-worktree");
    fs::rename(path, moved).unwrap();
    fs::create_dir(path).unwrap();
    git(path, &["init", "-b", "main"]);
    fs::write(path.join("precious"), "keep me").unwrap();
    let output = fixture.run(&["rm", "replaced"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("different repository"));
    assert_eq!(
        fs::read_to_string(path.join("precious")).unwrap(),
        "keep me"
    );
}

#[test]
fn dirty_workspace_is_retained_and_failed_creation_can_be_removed() {
    let fixture = Fixture::new();
    let workspace = fixture.add("dirty");
    let file = Path::new(workspace["path"].as_str().unwrap()).join("untracked");
    fs::write(&file, "keep me").unwrap();
    assert!(!fixture.run(&["rm", "dirty"]).status.success());
    assert_eq!(fs::read_to_string(file).unwrap(), "keep me");
    assert_eq!(
        fixture.ok(&["inspect", "dirty"])["workspace"]["state"],
        "ready"
    );
    fixture.ok(&["rm", "dirty", "--yes", "--keep-branch"]);
    assert!(!Path::new(workspace["path"].as_str().unwrap()).exists());
    assert!(
        !fixture
            .run(&[
                "add",
                fixture.repo.to_str().unwrap(),
                "broken",
                "--base",
                "does-not-exist"
            ])
            .status
            .success()
    );
    assert_eq!(
        fixture.ok(&["inspect", "broken"])["workspace"]["state"],
        "failed"
    );
    fixture.ok(&["rm", "broken"]);
}

#[test]
fn removal_requires_branch_choice_for_differences_but_not_external_processes() {
    let fixture = Fixture::new();
    let workspace = fixture.add("unpushed");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("tracked"), "different contents").unwrap();
    git(path, &["add", "tracked"]);
    git(
        path,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "unpushed",
        ],
    );
    let output = fixture.run(&["rm", "unpushed"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("branch choice"));
    fixture.ok(&["rm", "unpushed", "--yes", "--keep-branch"]);
    git(
        &fixture.repo,
        &[
            "rev-parse",
            "--verify",
            workspace["branch"].as_str().unwrap(),
        ],
    );

    let workspace = fixture.add("external");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let mut process = Command::new("sleep")
        .arg("10")
        .current_dir(path)
        .spawn()
        .unwrap();
    let output = fixture.run(&["rm", "external"]);
    let _ = process.kill();
    let _ = process.wait();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!path.exists());
}

#[test]
fn branch_removal_compares_contents_to_main_or_upstream_and_honors_explicit_choice() {
    let fixture = Fixture::new();
    let commit = |path: &Path| {
        git(path, &["add", "."]);
        git(
            path,
            &[
                "-c",
                "user.name=Shoal Test",
                "-c",
                "user.email=shoal@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "work",
            ],
        );
    };
    let same = fixture.add("same-tree");
    commit(Path::new(same["path"].as_str().unwrap()));
    assert_eq!(fixture.ok(&["rm", "same-tree"])["branch_deleted"], true);

    let pushed = fixture.add("pushed");
    let path = Path::new(pushed["path"].as_str().unwrap());
    fs::write(path.join("tracked"), "different from main").unwrap();
    commit(path);
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/project.git",
        ],
    );
    git(path, &["update-ref", "refs/remotes/origin/pushed", "HEAD"]);
    git(path, &["branch", "--set-upstream-to=origin/pushed"]);
    assert_eq!(fixture.ok(&["rm", "pushed"])["branch_deleted"], true);

    // The fake remote above models pushed history, not a fetchable upstream.
    let divergent = fixture.ok(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "divergent",
        "--base",
        "HEAD",
    ]);
    let path = Path::new(divergent["path"].as_str().unwrap());
    fs::write(path.join("tracked"), "unmerged work").unwrap();
    commit(path);
    assert!(!fixture.run(&["rm", "divergent"]).status.success());
    assert!(path.exists());
    assert_eq!(
        fixture.ok(&["rm", "divergent", "--yes", "--delete-branch"])["branch_deleted"],
        true
    );
}

#[test]
fn repositories_can_be_named_when_added_and_renamed_without_duplication() {
    let fixture = Fixture::new();
    let original = fixture.ok(&["repo", "list"])[0]["id"].clone();
    let named = fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "project",
    ]);
    assert_eq!(named["id"], original);
    assert_eq!(named["name"], "project");
    fixture.ok(&["add", "project", "named"]);
    let renamed = fixture.ok(&["repo", "rename", "project", "renamed"]);
    assert_eq!(renamed["id"], original);
    assert_eq!(renamed["name"], "renamed");
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
    fixture.ok(&["rm", "named"]);
}

#[test]
fn ports_are_named_idempotent_exported_and_released_with_the_workspace() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    fixture.add("second");
    let web = fixture.ok(&[
        "port",
        "acquire",
        "web",
        "first",
        "--reason",
        "Frontend dev server",
    ]);
    assert_eq!(web["env_var"], "SHOAL_PORT_WEB");
    assert_eq!(web["reason"], "Frontend dev server");
    assert_eq!(fixture.ok(&["port", "acquire", "web", "first"]), web);
    let port = web["port"].to_string();
    assert!(
        !fixture
            .run(&["port", "acquire", "web", "second", "--port", &port])
            .status
            .success()
    );
    let api = fixture.ok(&["port", "acquire", "api", "first", "--env", "API_PORT"]);
    let output = fixture.run(&[
        "exec",
        "first",
        "--",
        "sh",
        "-c",
        "printf '%s:%s' \"$SHOAL_PORT_WEB\" \"$API_PORT\"",
    ]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}:{}", web["port"], api["port"])
    );
    assert_eq!(
        fixture.ok(&["inspect", "first"])["ports"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let nested = fixture.run(&[
        "exec",
        "first",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "exec",
        "second",
        "--",
        "sh",
        "-c",
        "test -z \"${SHOAL_PORT_WEB:-}\" && test -z \"${API_PORT:-}\"",
    ]);
    assert!(
        !nested.status.success(),
        "cross-workspace execution should be denied: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    let path = Path::new(first["path"].as_str().unwrap());
    fs::write(path.join("dirty"), "keep").unwrap();
    assert!(!fixture.run(&["rm", "first"]).status.success());
    assert_eq!(
        fixture.ok(&["port", "first"])["reserved"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    fixture.ok(&["rm", "first", "--yes", "--keep-branch"]);
    assert_eq!(
        fixture.ok(&["port", "--all"])[0]["reserved"],
        serde_json::json!([])
    );
    fixture.ok(&["port", "acquire", "web", "second", "--port", &port]);
    fixture.ok(&["port", "release", "web", "second"]);
    assert_eq!(
        fixture.ok(&["port", "second"])["reserved"],
        serde_json::json!([])
    );
    fixture.ok(&["rm", "second"]);
}

#[test]
fn ports_avoid_listeners_and_concurrent_allocations_are_unique_and_persistent() {
    let mut fixture = Fixture::new();
    fixture.add("ports");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let occupied = listener.local_addr().unwrap().port().to_string();
    assert!(
        !fixture
            .run(&["port", "acquire", "occupied", "ports", "--port", &occupied])
            .status
            .success()
    );
    let mut children = Vec::new();
    for i in 0..8 {
        children.push(
            fixture
                .command()
                .args(["--json", "port", "acquire", &format!("server{i}"), "ports"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let mut numbers = std::collections::HashSet::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reservation: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(numbers.insert(reservation["port"].as_u64().unwrap()));
    }
    let before = fixture.ok(&["port", "ports"]);
    assert!(fixture.run(&["daemon", "stop"]).status.success());
    fixture.daemon.child.wait().unwrap();
    fixture.restart();
    assert_eq!(fixture.ok(&["port", "ports"]), before);
    fixture.ok(&["rm", "ports"]);
}

#[test]
fn diff_excludes_new_main_commits_before_and_after_rebase_and_uses_git_configuration() {
    let fixture = Fixture::with_config(Some(
        "[commands]\nreview = ['printf', '%s\\n', '{diff_base}..HEAD']\nplain = ['printf', '%s', '{args}']\n",
    ));
    let workspace = fixture.add("changes");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let commit = |path: &Path| {
        git(path, &["add", "."]);
        git(
            path,
            &[
                "-c",
                "user.name=Shoal Test",
                "-c",
                "user.email=shoal@example.invalid",
                "commit",
                "-m",
                "change",
            ],
        );
    };
    fs::write(path.join("tracked"), "workspace change\n").unwrap();
    commit(path);
    fs::write(fixture.repo.join("main-only"), "upstream-only content\n").unwrap();
    commit(&fixture.repo);
    for rebase in [false, true] {
        if rebase {
            git(
                path,
                &[
                    "-c",
                    "user.name=Shoal Test",
                    "-c",
                    "user.email=shoal@example.invalid",
                    "rebase",
                    "main",
                ],
            );
            fs::write(path.join("staged"), "staged workspace content\n").unwrap();
            git(path, &["add", "staged"]);
            fs::write(path.join("tracked"), "unstaged workspace content\n").unwrap();
        }
        let base = git(path, &["merge-base", "main", "HEAD"]);
        let review = fixture.run(&["review", "changes", "--manual", "--", "{diff_base}"]);
        assert!(
            review.status.success(),
            "{}",
            String::from_utf8_lossy(&review.stderr)
        );
        assert_eq!(
            String::from_utf8(review.stdout).unwrap(),
            format!("{}..HEAD\n{{diff_base}}\n", base.trim())
        );
        let output = fixture
            .command()
            .current_dir(path)
            .args(["diff"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let diff = String::from_utf8(output.stdout).unwrap();
        assert!(diff.contains("workspace"));
        assert!(!diff.contains("main-only") && !diff.contains("upstream-only"));
        if rebase {
            assert!(
                diff.contains("staged workspace content")
                    && diff.contains("unstaged workspace content")
            );
        }
    }
    let external = fixture.root.path().join("external-diff");
    fs::write(&external, "#!/bin/sh\nprintf 'configured-diff\\n'\n").unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        path,
        &["config", "diff.external", external.to_str().unwrap()],
    );
    let output = fixture.run(&["diff", "changes"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("configured-diff"));
    // A missing base blocks review, but is irrelevant to commands without the placeholder.
    git(&fixture.repo, &["branch", "-m", "renamed-main"]);
    assert!(
        !fixture
            .run(&["review", "changes", "--manual"])
            .status
            .success()
    );
    assert_eq!(
        fixture
            .run(&["plain", "changes", "--", "{diff_base}"])
            .stdout,
        b"{diff_base}"
    );
    fixture.ok(&["rm", "changes", "--yes", "--delete-branch"]);
}

#[test]
fn review_runs_the_configured_command_or_prompts_an_agent() {
    let fixture = Fixture::with_config(Some(
        "default_agent = 'reviewer'\n[commands]\nreview = ['printf', 'manual %s', '{branch}']\nreviewer = ['printf', '%s|', '{prompt}', '{args}']\n",
    ));
    fixture.add("changes");
    let output = fixture.run(&["review", "changes"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--manual or --agent"),
        "{output:?}"
    );
    assert_eq!(
        fixture.run(&["review", "changes", "--manual"]).stdout,
        b"manual changes"
    );
    for args in [
        &["review", "changes", "--agent", "reviewer", "--", "extra"][..],
        &["run", "reviewer", "changes", "--", "extra"][..],
    ] {
        let output = fixture.run(args);
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.ends_with("|extra|"), "{stdout}");
        if args[0] == "review" {
            assert!(stdout.contains("Review the changes on branch changes"));
            assert!(stdout.contains("`shoal diff`"));
        }
    }
    // Without a configured review command, the default agent reviews.
    let fixture = Fixture::with_config(Some(
        "default_agent = 'reviewer'\n[commands]\nreviewer = ['printf', '%s', '{prompt}']\n",
    ));
    fixture.add("agent-only");
    let output = fixture.run(&["review", "agent-only"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("branch agent-only"));
}

#[test]
fn pr_review_opens_the_head_against_its_base_and_reuses_the_owner() {
    let fixture = Fixture::with_config(Some(
        "default_agent = 'reviewer'\n[commands]\nreviewer = ['printf', '%s', '{prompt}']\n",
    ));
    let author = upstream_remote(&fixture);
    // Serve the local bare origin over a forge-shaped ssh URL.
    let remote = fixture.root.path().join("origin.git");
    let ssh = fixture.root.path().join("ssh");
    fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nfor last; do :; done\nexec sh -c \"$(printf '%s' \"$last\" | sed 's|/team/project.git|{}|')\"\n",
            remote.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &fixture.repo,
        &["config", "core.sshCommand", ssh.to_str().unwrap()],
    );
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://git@forge.example/team/project.git",
        ],
    );
    let commit = |message: &str| {
        fs::write(author.join(message), "change\n").unwrap();
        git(&author, &["add", message]);
        git(
            &author,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                message,
            ],
        );
    };
    git(&author, &["switch", "-c", "stack/base"]);
    commit("base");
    git(&author, &["switch", "-c", "stack/top"]);
    commit("top");
    git(&author, &["push", "origin", "stack/base", "stack/top"]);

    let bin = fixture.root.path().join("pr-bin");
    fs::create_dir(&bin).unwrap();
    let fj_args = fixture.root.path().join("fj-args");
    fs::write(
        bin.join("fj"),
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nprintf 'Top change #7\\nBy user — Open — +1 -0\\nFrom `stack/top` into `stack/base`\\n'\n",
            fj_args.display()
        ),
    )
    .unwrap();
    fs::set_permissions(bin.join("fj"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let review = |input: &str| {
        fixture
            .command()
            .args(["pr", "review", input, "--agent", "reviewer"])
            .current_dir(&fixture.repo)
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    let output = review("7");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(
            "Review pull request #7: Top change\nhttps://forge.example/team/project/pulls/7"
        ),
        "{stdout}"
    );
    assert!(stdout.contains("branch stack/top"), "{stdout}");
    assert!(
        fs::read_to_string(&fj_args)
            .unwrap()
            .contains("pr\0view\x007\0--host\0forge.example\0")
    );
    let workspace = &fixture.ok(&["inspect", "stack-top"])["workspace"];
    assert_eq!(workspace["base_ref"], "refs/remotes/origin/stack/base");
    let path_in = Path::new(workspace["path"].as_str().unwrap());
    assert_eq!(
        git(
            path_in,
            &["merge-base", "--fork-point", "origin/stack/base", "HEAD"]
        ),
        git(&author, &["rev-parse", "stack/base"])
    );

    let output = review("https://forge.example/team/project/pulls/7");
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Reviewing PR #7 in workspace stack-top"),
        "{output:?}"
    );
    let output = review("https://forge.example/other/project/pulls/7");
    assert!(!output.status.success());
}

#[test]
fn repository_config_sets_the_automatic_port_range() {
    let fixture = Fixture::new();
    let workspace = fixture.add("ranged");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let [first, second] = [(); 2].map(|()| {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    });
    fs::write(
        path.join(".shoal.toml"),
        format!("[ports]\nstart={first}\nend={first}\n"),
    )
    .unwrap();
    assert_eq!(
        fixture.ok(&["port", "acquire", "web", "ranged"])["port"],
        first
    );
    assert!(
        !fixture
            .run(&["port", "acquire", "api", "ranged"])
            .status
            .success()
    );
    // The saved config is the top layer, bound by bound.
    let saved = fixture.root.path().join("saved.toml");
    let save = |text: &str| {
        fs::write(&saved, text).unwrap();
        fixture.ok(&[
            "repo",
            "config",
            fixture.repo.to_str().unwrap(),
            "--file",
            saved.to_str().unwrap(),
        ]);
    };
    save(&format!("[ports]\nstart={second}\nend={second}\n"));
    assert_eq!(
        fixture.ok(&["port", "acquire", "api", "ranged"])["port"],
        second
    );
    // The layered range must stay nonempty: `end = 1` under the file's `start`.
    save("[ports]\nend=1\n");
    let output = fixture.run(&["port", "acquire", "db", "ranged"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("nonempty range"));
}

#[test]
fn config_show_reports_effective_values_and_their_layers() {
    let fixture = Fixture::with_config(Some(
        "default_agent = 'claude'\n[commands]\nglobal = ['global']\nshared = ['global']\n\
         [auto_cleanup]\nenabled = false\n[ports]\nstart = 2000\nend = 6000\n",
    ));

    // In a registered checkout without a workspace, the checkout file is the
    // worktree layer for the current-directory target.
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "post_setup_cmd = 'checkout/attach'\n",
    )
    .unwrap();
    let checkout = fixture
        .command()
        .current_dir(&fixture.repo)
        .args(["--json", "config", "show"])
        .output()
        .unwrap();
    assert!(checkout.status.success());
    let checkout: Value = serde_json::from_slice(&checkout.stdout).unwrap();
    let checkout_entry = checkout
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "post_setup_cmd")
        .unwrap();
    assert_eq!(checkout_entry["value"], "checkout/attach");
    assert_eq!(checkout_entry["layer"], "worktree_file");
    fs::remove_file(fixture.repo.join(".shoal.toml")).unwrap();

    let workspace = fixture.add("layered-config");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "setup_cmd = 'scripts/setup'\n[commands]\nworktree = ['worktree']\nshared = ['worktree']\n\
         [ports]\nstart = 3000\n[ports.web]\nenv = 'WORKTREE_PORT'\n",
    )
    .unwrap();
    let saved = fixture.root.path().join("saved-config.toml");
    fs::write(
        &saved,
        "default_agent = 'codex'\n[commands]\nshared = ['saved']\n[ports]\nend = 4000\n",
    )
    .unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);

    let report = fixture.ok(&["config", "show", "layered-config"]);
    let entry = |key: &str| {
        report
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["key"] == key)
            .unwrap()
    };
    for (key, value, layer) in [
        (
            "default_agent",
            serde_json::json!("codex"),
            "saved_repository_config",
        ),
        (
            "commands.shared",
            serde_json::json!(["saved"]),
            "saved_repository_config",
        ),
        (
            "commands.worktree",
            serde_json::json!(["worktree"]),
            "worktree_file",
        ),
        (
            "setup_cmd",
            serde_json::json!("scripts/setup"),
            "worktree_file",
        ),
        ("ports.start", serde_json::json!(3000), "worktree_file"),
        (
            "ports.end",
            serde_json::json!(4000),
            "saved_repository_config",
        ),
        (
            "commands.global",
            serde_json::json!(["global"]),
            "global_config",
        ),
        (
            "auto_cleanup.enabled",
            serde_json::json!(false),
            "global_config",
        ),
        (
            "codex.default_mode",
            serde_json::json!("cli"),
            "built_in_default",
        ),
        (
            "auto_cleanup.idle_minutes",
            serde_json::json!(10),
            "built_in_default",
        ),
    ] {
        assert_eq!(entry(key)["value"], value, "{key}");
        assert_eq!(entry(key)["layer"], layer, "{key}");
    }
    assert_eq!(entry("ports.web")["value"]["env"], "WORKTREE_PORT");
    assert_eq!(entry("ports.web")["layer"], "worktree_file");

    let human = fixture.run(&["config", "show", "layered-config"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("ports.start = 3000 (worktree file)"));
    assert!(human.contains("default_agent = \"codex\" (saved repository config)"));
}

#[test]
fn configured_port_range_exhaustion_and_release() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let number = listener.local_addr().unwrap().port();
    let fixture = Fixture::with_config(Some(&format!("[ports]\nstart={number}\nend={number}\n")));
    fixture.add("limited");
    assert!(
        !fixture
            .run(&["port", "acquire", "web", "limited"])
            .status
            .success()
    );
    drop(listener);
    let lease = fixture.ok(&["port", "acquire", "web", "limited"]);
    assert_eq!(lease["port"], number);
    assert!(
        !fixture
            .run(&["port", "acquire", "api", "limited"])
            .status
            .success()
    );
    fixture.ok(&["port", "release", "web", "limited"]);
    assert_eq!(
        fixture.ok(&["port", "acquire", "api", "limited"])["port"],
        number
    );
    fixture.ok(&["rm", "limited"]);
}

#[test]
fn concurrent_adds_cannot_claim_the_same_name() {
    let fixture = Fixture::new();
    for (first_name, second_name, workspace_name) in [
        ("shared", "shared", "shared"),
        ("shared/topic", "shared-topic", "shared-topic"),
    ] {
        let first = fixture
            .command()
            .args(["add", fixture.repo.to_str().unwrap(), first_name])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let second = fixture.run(&["add", fixture.repo.to_str().unwrap(), second_name]);
        let first = first.wait_with_output().unwrap();
        assert_ne!(first.status.success(), second.status.success());
        assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
        fixture.ok(&["rm", workspace_name]);
    }
}

#[test]
fn branch_names_are_preserved_with_portable_workspace_names() {
    let fixture = Fixture::new();
    let long = format!("long/{}", "x".repeat(100));
    let sha1 = "a".repeat(40);
    let sha256 = "b".repeat(64);
    for (branch, name) in [
        (
            "henrik/8374-set-league-season-player-profile",
            "henrik-8374-set-league-season-player-profile",
        ),
        ("release/v1.2", "release-v1-2"),
        ("feature/æøå-日本語", "feature--------"),
        ("_private", "private"),
        ("+special", "special"),
        ("@", "workspace"),
        (sha1.as_str(), sha1.as_str()),
        (sha256.as_str(), sha256.as_str()),
        ("refs/heads/topic", "refs-heads-topic"),
        ("topic/-leaf", "topic--leaf"),
        ("\u{2003}topic\u{2003}", "topic-"),
        (
            "topic/quote'\";$`(literal)&{ok}",
            "topic-quote------literal---ok-",
        ),
        (long.as_str(), &format!("long-{}", "x".repeat(59))),
    ] {
        git(&fixture.repo, &["check-ref-format", "--branch", branch]);
        let workspace = fixture.add(branch);
        let actual_branch = if branch == "@" || branch == sha1 || branch == sha256 {
            format!("{branch}-2")
        } else {
            branch.to_owned()
        };
        assert_eq!(workspace["branch"], actual_branch);
        assert_eq!(workspace["name"], name, "{branch}");
        let path = Path::new(workspace["path"].as_str().unwrap());
        assert_eq!(path.file_name().unwrap(), name);
        assert_eq!(
            git(path, &["symbolic-ref", "HEAD"]),
            format!("refs/heads/{actual_branch}\n")
        );
        assert_eq!(
            fixture.ok(&["inspect", name])["workspace"]["id"],
            workspace["id"]
        );
        let output = fixture.run(&["exec", name, "--", "git", "symbolic-ref", "HEAD"]);
        assert!(output.status.success());
        assert_eq!(
            output.stdout,
            format!("refs/heads/{actual_branch}\n").as_bytes()
        );
        assert_eq!(fixture.ok(&["merge", "main", name])["success"], true);
        assert_eq!(fixture.ok(&["rm", name])["branch_deleted"], true);
    }
}

#[test]
fn invalid_branch_names_and_normalized_name_collisions_preserve_existing_work() {
    let fixture = Fixture::new();
    git(&fixture.repo, &["switch", "-c", "previous"]);
    git(&fixture.repo, &["switch", "main"]);
    for branch in [
        "",
        "../escape",
        "/absolute",
        "trailing/",
        "double//slash",
        "bad..name",
        ".hidden",
        "x/.hidden",
        "bad.lock",
        "x/bad.lock/y",
        "bad.",
        "bad name",
        "bad\nname",
        "bad\\name",
        "bad~name",
        "bad^name",
        "bad:name",
        "bad?name",
        "bad*name",
        "bad[name",
        "-option",
        "@{1}",
        "@{-1}",
    ] {
        assert!(
            !fixture
                .run(&["add", fixture.repo.to_str().unwrap(), branch])
                .status
                .success(),
            "{branch}"
        );
        assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    }
    git(&fixture.repo, &["branch", "-d", "previous"]);
    let existing = fixture.add("feature/topic");
    for branch in ["feature-topic", "feature.topic"] {
        let output = fixture.run(&["add", fixture.repo.to_str().unwrap(), branch]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("feature-topic"));
    }
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([existing]));
    assert_eq!(
        git(
            &fixture.repo,
            &["for-each-ref", "--format=%(refname)", "refs/heads/"]
        ),
        "refs/heads/feature/topic\nrefs/heads/main\n"
    );
}

#[test]
fn interactive_add_preserves_literal_branch_spelling() {
    let fixture = Fixture::new();
    let (output, transcript) = fixture.interactive(
        &["add", fixture.repo.to_str().unwrap(), "--base", "main"],
        "\u{2003}henrik/topic\u{2003}\n",
    );
    assert!(output.status.success(), "{transcript}");
    assert!(transcript.contains("Branch name:"));
    let workspace = &fixture.ok(&["inspect", "henrik-topic-"])["workspace"];
    assert_eq!(workspace["branch"], "\u{2003}henrik/topic\u{2003}");
    fixture.ok(&["rm", "henrik-topic-"]);
}

#[test]
fn execution_preserves_pipes_exit_code_environment_and_current_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.add("execute");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::create_dir(path.join("nested")).unwrap();
    let mut child = fixture
        .command()
        .current_dir(path.join("nested"))
        .args([
            "exec",
            "--",
            "sh",
            "-c",
            "cat; printf '%s' \"$SHOAL_WORKSPACE\"; printf error >&2; exit 7",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"pipe:").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"pipe:execute");
    assert_eq!(output.stderr, b"error");
    assert_eq!(
        fixture.ok(&["inspect", "execute"])["executions"],
        serde_json::json!([])
    );
    fixture.ok(&["rm", "execute"]);
}

#[test]
fn add_starts_agents_only_after_creation_and_preserves_workspace_on_exit() {
    let fixture = Fixture::new();
    let bin = fixture.root.path().join("add-agent-bin");
    fs::create_dir(&bin).unwrap();
    let inspection = fixture.root.path().join("agent-inspection.json");
    let directive = fixture.root.path().join("directive");
    for agent in ["codex", "claude"] {
        let stub = bin.join(agent);
        fs::write(
            &stub,
            r#"#!/bin/sh
"$SHOAL_TEST_BIN" --state-dir "$SHOAL_TEST_STATE" --json inspect "$SHOAL_TEST_NAME" > "$SHOAL_TEST_INSPECTION" || exit 99
test -f tracked || exit 98
test -z "$SHOAL_SHELL_DIRECTIVE" || exit 97
printf '%s\n' "$PWD" "$SHOAL_WORKSPACE" "$@"
exit 7
"#,
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    for (name, agent, mode) in [
        ("add-codex", "codex", "cli"),
        ("add-claude", "claude", "cli"),
        ("add-app", "codex", "app"),
    ] {
        fs::write(
            config_dir.join("config.toml"),
            format!("[codex]\ndefault_mode = '{mode}'"),
        )
        .unwrap();
        let output = fixture
            .command()
            .args([
                "add",
                fixture.repo.to_str().unwrap(),
                name,
                "--agent",
                agent,
                "--",
                "literal spaces; $(false)",
            ])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("SHOAL_TEST_BIN", env!("CARGO_BIN_EXE_shoal"))
            .env("SHOAL_TEST_STATE", fixture.root.path().join("state"))
            .env("SHOAL_TEST_NAME", name)
            .env("SHOAL_TEST_INSPECTION", &inspection)
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7), "{output:?}");
        let during: Value = serde_json::from_slice(&fs::read(&inspection).unwrap()).unwrap();
        assert_eq!(during["workspace"]["name"], name);
        assert_eq!(
            during["executions"].as_array().unwrap().len(),
            usize::from(mode == "cli")
        );
        let after = fixture.ok(&["inspect", name]);
        assert_eq!(after["executions"], serde_json::json!([]));
        let path = after["workspace"]["path"].as_str().unwrap();
        assert!(Path::new(path).join("tracked").exists());
        assert_eq!(fs::read_to_string(&directive).unwrap(), format!("{path}\n"));
        let args = if mode == "app" {
            format!("\napp\n{path}\nliteral spaces; $(false)\n")
        } else if agent == "claude" {
            format!("{name}\nliteral spaces; $(false)\n--remote-control\n{name}\n")
        } else {
            format!(
                "{name}\nliteral spaces; $(false)\n--sandbox\ndanger-full-access\n--ask-for-approval=never\n"
            )
        };
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .ends_with(&format!(
                    "{}\n{args}",
                    fs::canonicalize(path).unwrap().display()
                ))
        );
    }

    fs::remove_file(&inspection).unwrap();
    let output = fixture
        .command()
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "bad-base",
            "--base",
            "missing-ref",
            "--agent",
            "codex",
        ])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("SHOAL_TEST_BIN", env!("CARGO_BIN_EXE_shoal"))
        .env("SHOAL_TEST_INSPECTION", &inspection)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!inspection.exists());

    // A missing executable leaves a completed worktree available for retry.
    fs::remove_file(bin.join("codex")).unwrap();
    fs::write(config_dir.join("config.toml"), "").unwrap();
    let output = fixture
        .command()
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "missing-agent",
            "--agent",
            "codex",
        ])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let workspace = fixture.ok(&["inspect", "missing-agent"]);
    assert!(
        Path::new(workspace["workspace"]["path"].as_str().unwrap())
            .join("tracked")
            .exists()
    );
    assert_eq!(workspace["executions"], serde_json::json!([]));
    assert!(
        !fixture
            .run(&["add", "--", "prompt without agent"])
            .status
            .success()
    );
}

#[test]
fn codex_default_mode_is_read_at_launch_and_explicit_modes_override_it() {
    let fixture = Fixture::new();
    let workspace = fixture.add("default-mode");
    let path = workspace["path"].as_str().unwrap();
    let bin = fixture.root.path().join("codex-bin");
    fs::create_dir(&bin).unwrap();
    let stub = bin.join("codex");
    fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s\\n' \"$SHOAL_WORKSPACE\" \"$@\"\nexit 7\n",
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();

    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    for config in ["", "[codex]\ndefault_mode = 'app'"] {
        // Change the default while the daemon remains running.
        fs::write(config_dir.join("config.toml"), config).unwrap();
        for mode in [None, Some("--cli"), Some("--app")] {
            let mut command = fixture.command();
            command.current_dir(path).arg("codex");
            if let Some(mode) = mode {
                command.args([mode, "default-mode"]);
            }
            let output = command
                .args(["--", "literal spaces; $(false)"])
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(7),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let app = mode == Some("--app") || (mode.is_none() && !config.is_empty());
            let expected = if app {
                format!("\napp\n{path}\nliteral spaces; $(false)\n")
            } else {
                "default-mode\nliteral spaces; $(false)\n--sandbox\ndanger-full-access\n--ask-for-approval=never\n".into()
            };
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
            assert_eq!(
                fixture.ok(&["inspect", "default-mode"])["executions"],
                serde_json::json!([])
            );
        }
    }
    // The worktree's own config wins over the global default, also at launch.
    fs::write(config_dir.join("config.toml"), "").unwrap();
    fs::write(
        Path::new(path).join(".shoal.toml"),
        "[codex]\ndefault_mode = 'app'\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .current_dir(path)
        .args(["codex", "--", "repo-default"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("\napp\n{path}\nrepo-default\n")
    );
    // An explicit desktop handoff does not load terminal prompt templates.
    fs::write(config_dir.join("agent-template.md"), [0xff]).unwrap();
    for mode in ["--app", "--cli"] {
        let output = fixture
            .command()
            .args(["codex", mode, "default-mode", "--", "desktop prompt"])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        if mode == "--app" {
            assert_eq!(output.status.code(), Some(7), "{output:?}");
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("\napp\n{path}\ndesktop prompt\n")
            );
        } else {
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("agent-template.md"),
                "{output:?}"
            );
        }
    }
}

#[test]
fn agent_shortcuts_forward_arguments_without_starting_real_agents() {
    let fixture = Fixture::new();
    let workspace = fixture.add("shortcut");
    let bin = fixture.root.path().join("stub-bin");
    fs::create_dir(&bin).unwrap();
    for agent in ["claude", "codex"] {
        let path = bin.join(agent);
        fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$SHOAL_WORKSPACE\" \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = fixture.command();
        command.arg(agent);
        if agent == "codex" {
            command.arg("--cli");
        }
        let output = command
            .args(["shortcut", "--", "--version", "hello with spaces"])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "shortcut\n--version\nhello with spaces\n{}",
                if agent == "claude" {
                    "--remote-control\nshortcut\n"
                } else {
                    "--sandbox\ndanger-full-access\n--ask-for-approval=never\n"
                }
            )
        );
    }
    // Current-directory resolution supplies an ID internally; Claude still gets
    // the human workspace name, as it must after an fzf selection as well.
    for target in [Some(workspace["id"].as_str().unwrap()), None] {
        let mut command = fixture.command();
        command
            .current_dir(workspace["path"].as_str().unwrap())
            .arg("claude");
        if let Some(target) = target {
            command.arg(target);
        }
        let output = command
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"shortcut\n--remote-control\nshortcut\n");
    }
}

#[test]
fn agent_templates_resolve_per_launch_and_reach_native_instruction_options() {
    let fixture = Fixture::new();
    let workspace = fixture.add("instructions");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("agent-template.md"),
        "global {workspace} {branch}",
    )
    .unwrap();
    let bin = fixture.root.path().join("template-bin");
    fs::create_dir(&bin).unwrap();
    for agent in ["codex", "claude"] {
        let stub = bin.join(agent);
        fs::write(&stub, "#!/bin/sh\nprintf '%s\\0' \"$@\"\n").unwrap();
        fs::set_permissions(stub, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for (index, expected) in [
        "global instructions instructions".to_owned(),
        format!("repo {}", path.display()),
        "saved \"quotes\"\n$(false) {unknown}".to_owned(),
        String::new(),
    ]
    .into_iter()
    .enumerate()
    {
        if index == 1 {
            fs::write(path.join("agent-template.md"), "repo {path}").unwrap();
        }
        if index >= 2 {
            let local = fixture.root.path().join("local.toml");
            fs::write(
                &local,
                format!("agent_template = {}", toml::Value::String(expected.clone())),
            )
            .unwrap();
            fixture.ok(&[
                "repo",
                "config",
                fixture.repo.to_str().unwrap(),
                "--file",
                local.to_str().unwrap(),
            ]);
        }
        for agent in ["codex", "claude"] {
            let mut command = fixture.command();
            command.arg(agent);
            if agent == "codex" {
                command.arg("--cli");
            }
            let output = command
                .args(["instructions", "--", "user prompt"])
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            let args: Vec<_> = stdout.split('\0').collect();
            if expected.is_empty() {
                assert_eq!(args[0], "user prompt");
            } else {
                if agent == "claude" {
                    assert_eq!(args[..2], ["--append-system-prompt", &expected]);
                } else {
                    assert_eq!(args[0], "-c");
                    let setting: toml::Value = toml::from_str(args[1]).unwrap();
                    assert_eq!(
                        setting["developer_instructions"].as_str(),
                        Some(expected.as_str())
                    );
                }
                assert_eq!(args[2], "user prompt");
            }
        }
    }
}

#[test]
fn stop_and_manual_removal_terminate_connected_executions() {
    let fixture = Fixture::new();
    for operation in ["stop", "rm"] {
        fixture.add("running");
        let child = fixture
            .command()
            .args(["exec", "running", "--", "sleep", "5"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while fixture.ok(&["inspect", "running"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        fixture.ok(&[operation, "running"]);
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(Instant::now() < deadline);
        if operation == "stop" {
            fixture.ok(&["rm", "running"]);
        }
    }
}

#[test]
fn registry_survives_daemon_restart() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("persistent");
    fixture.run(&["daemon", "stop"]);
    fixture.daemon.child.wait().unwrap();
    fixture.restart();
    assert_eq!(
        fixture.ok(&["inspect", "persistent"])["workspace"]["id"],
        workspace["id"]
    );
    fixture.ok(&["rm", "persistent"]);
}

#[test]
fn noninteractive_missing_targets_do_not_open_pickers() {
    let fixture = Fixture::new();
    for args in [&["add"][..], &["exec", "--", "true"], &["rm"]] {
        let output = fixture.run(args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("non-interactive"));
    }
}

#[test]
fn shell_function_navigates_after_add_and_away_after_rm() {
    let fixture = Fixture::new();
    let integration = fixture.root.path().join("integration.sh");
    fs::write(&integration, fixture.run(&["shell", "init"]).stdout).unwrap();
    let script = r#"
set -e
. "$INTEGRATION"
cd "$REPO"
shoal add "$REPO" navigate
test "${PWD##*/}" = navigate
shoal_repo_dir="$(dirname "$PWD")"
shoal cd -
test "$PWD" = "$REPO"
shoal cd -
test "${PWD##*/}" = navigate
cd "$REPO"
shoal cd navigate
test "${PWD##*/}" = navigate
if shoal exec navigate -- sh -c 'exit 7'; then
  exit 1
else
  test "$?" -eq 7
fi
printf 'keep me' > untracked
if shoal rm; then
  exit 1
fi
test "${PWD##*/}" = navigate
test -f untracked
rm untracked
mkdir nested
cd nested
shoal rm
test "$PWD" = "$shoal_repo_dir"
if shoal cd -; then
  exit 1
fi
test "$PWD" = "$shoal_repo_dir"
shoal add "$REPO" acknowledged
test "${PWD##*/}" = acknowledged
printf 'retained' > untracked
shoal pr merged
test "${PWD##*/}" = acknowledged
shoal pr clear
rm untracked
shoal pr merged
test "$PWD" = "$shoal_repo_dir"
until ! shoal inspect acknowledged >/dev/null 2>&1; do sleep 0.1; done
printf 'navigation-ok\n'
"#;
    for shell in ["bash", "zsh"] {
        let output = support::isolated(fixture.root.path(), shell)
            .arg("-c")
            .arg(script)
            .env("INTEGRATION", &integration)
            .env("REPO", &fixture.repo)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    Path::new(env!("CARGO_BIN_EXE_shoal"))
                        .parent()
                        .unwrap()
                        .display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).ends_with("navigation-ok\n"));
    }
}

#[test]
fn interactive_navigation_reports_missing_shell_integration() {
    let fixture = Fixture::new();
    let workspace = fixture.add("existing");

    let (cd, cd_stderr) = fixture.interactive(&["cd", "existing"], "");
    assert!(cd.status.success());
    assert_eq!(
        String::from_utf8(cd.stdout).unwrap(),
        format!("{}\n", workspace["path"].as_str().unwrap())
    );
    assert!(cd_stderr.contains("shell integration is not loaded"));
    assert!(cd_stderr.contains("source <(shoal shell init)"));

    let (add, add_stderr) =
        fixture.interactive(&["add", fixture.repo.to_str().unwrap(), "created"], "");
    assert!(add.status.success());
    assert!(
        String::from_utf8(add.stdout)
            .unwrap()
            .contains("Created created")
    );
    assert!(add_stderr.contains("shell integration is not loaded"));
    assert!(add_stderr.contains("source <(shoal shell init)"));

    let piped = fixture.run(&["cd", "existing"]);
    assert!(piped.status.success());
    assert!(piped.stderr.is_empty());
    assert_eq!(
        String::from_utf8(piped.stdout).unwrap(),
        format!("{}\n", workspace["path"].as_str().unwrap())
    );

    let json = fixture.ok(&["cd", "existing"]);
    assert_eq!(json["path"], workspace["path"]);
}

#[test]
fn resource_overviews_handle_no_workspaces() {
    let fixture = Fixture::new();
    for noun in ["port", "resource"] {
        for args in [vec![noun, "--all"], vec![noun, "list", "--all"]] {
            assert_eq!(fixture.ok(&args), serde_json::json!([]));
            let output = fixture.run(&args);
            assert!(output.status.success());
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty());
        }
    }
}

#[test]
fn resource_overviews_preserve_results_after_workspace_errors() {
    let fixture = Fixture::new();
    let broken = fixture.add("broken");
    fixture.add("healthy");
    fs::write(
        Path::new(broken["path"].as_str().unwrap()).join(".shoal.toml"),
        "invalid = [",
    )
    .unwrap();
    // The daemon lists by name, so a failure must not short-circuit the query.
    let workspaces = fixture.ok(&["list"]);
    assert_eq!(workspaces[0]["name"], "broken");
    assert_eq!(workspaces[1]["name"], "healthy");
    for (noun, empty_text) in [
        ("port", "No configured or reserved ports"),
        ("resource", "No configured resources or leases"),
    ] {
        let mut expected = fixture.ok(&[noun, "healthy"]);
        expected["workspace"] = workspaces[1].clone();
        for args in [vec![noun, "--all"], vec![noun, "list", "--all"]] {
            let output = fixture
                .command()
                .arg("--json")
                .args(&args)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stderr.is_empty());
            let overviews: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(overviews.as_array().unwrap().len(), 2);
            assert_eq!(overviews[0]["workspace"], workspaces[0]);
            let error = overviews[0]["error"].as_str().unwrap();
            assert!(!error.is_empty());
            assert_eq!(overviews[0].as_object().unwrap().len(), 2);
            assert_eq!(overviews[1], expected);

            let human = fixture.run(&args);
            assert_eq!(human.status.code(), Some(1));
            assert!(human.stderr.is_empty());
            assert_eq!(
                String::from_utf8(human.stdout).unwrap(),
                format!("broken: {error}\nhealthy\n{empty_text}\n")
            );
        }
    }
    // Once every workspace is healthy, the same overview succeeds.
    fs::remove_file(Path::new(broken["path"].as_str().unwrap()).join(".shoal.toml")).unwrap();
    for noun in ["port", "resource"] {
        assert_eq!(fixture.ok(&[noun, "--all"]).as_array().unwrap().len(), 2);
    }
}

#[test]
fn configured_ports_are_lazy_and_conflicts_require_acceptance() {
    let fixture = Fixture::new();
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let preferred = occupied.local_addr().unwrap().port();
    fs::write(
        fixture.repo.join(".shoal.toml"),
        format!("[ports.web]\nport = {preferred}\nenv = \"PORT\"\nreason = \"Web server\"\n")
            .replace("\\\"", "\""),
    )
    .unwrap();
    git(&fixture.repo, &["add", ".shoal.toml"]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "config",
        ],
    );
    let workspace = fixture.add("configured");
    fixture.add("healthy");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let overview = fixture
        .command()
        .current_dir(path)
        .args(["--json", "port"])
        .output()
        .unwrap();
    assert!(overview.status.success());
    let overview: Value = serde_json::from_slice(&overview.stdout).unwrap();
    assert_eq!(overview["configured"]["web"]["port"], preferred);
    assert_eq!(overview["reserved"], serde_json::json!([]));
    assert_eq!(fixture.ok(&["port", "list", "configured"]), overview);
    let human = fixture.run(&["port", "configured"]);
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains(&format!(
        "web: not reserved (preferred: {preferred}; conflicts: suggest)"
    )));
    for old in [
        vec!["ports", "configured"],
        vec!["resources", "configured"],
        vec!["port", "reserve", "web", "configured"],
    ] {
        assert!(
            !fixture.run(&old).status.success(),
            "old command still works: {old:?}"
        );
    }
    let proposal = fixture.run(&["--json", "port", "acquire", "web", "configured"]);
    assert_eq!(proposal.status.code(), Some(2));
    let proposal: Value = serde_json::from_slice(&proposal.stdout).unwrap();
    assert_eq!(proposal["reserved"], false);
    assert_eq!(
        fixture.ok(&["port", "configured"])["reserved"],
        serde_json::json!([])
    );
    let accepted = fixture.ok(&[
        "port",
        "acquire",
        "web",
        "configured",
        "--port",
        &proposal["suggested_port"].to_string(),
    ]);
    assert_eq!(accepted["env_var"], "PORT");
    assert_eq!(
        fixture.ok(&["port", "acquire", "web", "configured"]),
        accepted
    );
    fixture.ok(&["port", "release", "web", "configured"]);
    let automatic = fixture.ok(&[
        "port",
        "acquire",
        "web",
        "configured",
        "--on-conflict",
        "auto",
    ]);
    assert_ne!(automatic["port"], preferred);
    assert_eq!(
        fixture.ok(&["port", "acquire", "web", "configured"]),
        automatic
    );
    fs::create_dir(path.join(".shoal")).unwrap();
    fs::rename(path.join(".shoal.toml"), path.join(".shoal/config.toml")).unwrap();
    fixture.ok(&["port", "configured"]);
    fs::write(path.join(".shoal.toml"), "").unwrap();
    assert!(!fixture.run(&["port", "configured"]).status.success());
    for noun in ["port", "resource"] {
        let output = fixture.run(&["--json", noun, "--all"]);
        assert_eq!(output.status.code(), Some(1));
        let overviews: Value = serde_json::from_slice(&output.stdout).unwrap();
        let overviews = overviews.as_array().unwrap();
        assert_eq!(overviews.len(), 2);
        assert!(overviews.iter().any(|overview| {
            overview["workspace"]["name"] == "configured" && overview["error"].is_string()
        }));
        assert!(overviews.iter().any(|overview| {
            overview["workspace"]["name"] == "healthy" && overview["error"].is_null()
        }));
    }
}

#[test]
fn execution_scope_limits_management_and_expires() {
    let fixture = Fixture::new();
    let setup = fixture.repo.join("setup.sh");
    fs::write(&setup, "#!/bin/sh\nprintf setup >> setup-runs\n").unwrap();
    fs::set_permissions(&setup, fs::Permissions::from_mode(0o755)).unwrap();
    let post_setup = fixture.repo.join("post-setup.sh");
    fs::write(
        &post_setup,
        "#!/bin/sh\ntest -z \"$SHOAL_SCOPE_TOKEN\" || exit 81\ntest -z \"$SHOAL_EXECUTION_ID\" || exit 82\ntest -z \"$SHOAL_RESERVED_PORT_ENV\" || exit 83\ntest -z \"$SHOAL_PORT_WEB\" || exit 84\nprintf hook >> hook-runs\nsleep 30 < /dev/null > /dev/null 2>&1 &\n",
    )
    .unwrap();
    fs::set_permissions(&post_setup, fs::Permissions::from_mode(0o755)).unwrap();
    commit_resource_config(
        &fixture.repo,
        "setup_cmd = 'setup.sh'\npost_setup_cmd = 'post-setup.sh'\n",
    );
    let worker = fixture.add("worker");
    let worker_path = Path::new(worker["path"].as_str().unwrap());
    fixture.add("other");
    let binary = env!("CARGO_BIN_EXE_shoal");
    let scoped = |args: &[&str]| {
        fixture
            .command()
            .args(["exec", "worker", "--", binary])
            .args(args)
            .output()
            .unwrap()
    };
    let output = scoped(&["--json", "list"]);
    assert!(output.status.success());
    let list: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["name"], "worker");
    let output = scoped(&["--json", "config", "show"]);
    assert!(output.status.success(), "{output:?}");
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        config
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["key"] == "setup_cmd" && entry["value"] == "setup.sh" })
    );
    assert!(scoped(&["port", "acquire", "web"]).status.success());
    let sibling_started = worker_path.join("sibling-started");
    let mut sibling = fixture
        .command()
        .args([
            "exec",
            "worker",
            "--",
            "sh",
            "-c",
            "touch sibling-started; sleep 30",
        ])
        .spawn()
        .unwrap();
    wait_until("sibling execution", || sibling_started.exists());
    let output = scoped(&["setup", "worker"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("active or unknown execution"),
        "{output:?}"
    );
    fixture.ok(&["stop", "worker"]);
    assert!(!sibling.wait().unwrap().success());
    let output = scoped(&["setup", "worker"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(worker_path.join("setup-runs")).unwrap(),
        "setupsetup"
    );
    assert_eq!(
        fs::read_to_string(worker_path.join("hook-runs")).unwrap(),
        "hookhook"
    );
    assert_eq!(
        fixture.ok(&["inspect", "worker"])["executions"],
        serde_json::json!([]),
        "hook survivors must not belong to the invoking execution"
    );
    let output = scoped(&["install", "--dry-run"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot administer Shoal"));
    assert!(
        scoped(&["exec", "worker", "--", binary, "port"])
            .status
            .success()
    );
    for args in [
        vec!["rm", "worker", "--yes", "--delete-branch"],
        vec!["stop", "worker"],
        vec!["inspect", "other"],
        vec!["exec", "other", "--", "true"],
        vec!["setup", "other"],
        vec!["pr", "merged", "other"],
        vec!["pr", "clear", "other"],
        vec!["config", "show", "other"],
        vec![
            "config",
            "set",
            "default_agent",
            "codex",
            "--repo",
            fixture.repo.to_str().unwrap(),
        ],
        vec![
            "config",
            "unset",
            "default_agent",
            "--repo",
            fixture.repo.to_str().unwrap(),
        ],
        vec!["port", "acquire", "web", "other"],
        vec!["repo", "rename", fixture.repo.to_str().unwrap(), "changed"],
        vec!["repo", "config", fixture.repo.to_str().unwrap()],
        vec!["repo", "config", fixture.repo.to_str().unwrap(), "--clear"],
        vec!["daemon", "stop"],
    ] {
        let output = scoped(&args);
        assert!(
            !output.status.success(),
            "scoped {args:?} unexpectedly succeeded"
        );
    }
    let token = fixture.run(&[
        "exec",
        "worker",
        "--",
        "sh",
        "-c",
        "printf '%s' \"$SHOAL_SCOPE_TOKEN\"",
    ]);
    let token = String::from_utf8(token.stdout).unwrap();
    assert!(!token.is_empty());
    let output = fixture
        .command()
        .env("SHOAL_SCOPE_TOKEN", token)
        .args(["list"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expired or unknown"));
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 2);
}

#[test]
fn setup_commands_cannot_recursively_run_setup() {
    let fixture = Fixture::new();
    let setup = fixture.repo.join("setup.sh");
    fs::write(&setup, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&setup, fs::Permissions::from_mode(0o755)).unwrap();
    commit_resource_config(&fixture.repo, "setup_cmd = 'setup.sh'\n");
    let worker = fixture.add("worker");
    let setup = Path::new(worker["path"].as_str().unwrap()).join("setup.sh");
    fs::write(
        &setup,
        format!("#!/bin/sh\n'{}' setup\n", env!("CARGO_BIN_EXE_shoal")),
    )
    .unwrap();

    let output = fixture.run(&["setup", "worker"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("a setup command cannot recursively run setup"),
        "{output:?}"
    );
}

#[cfg(target_os = "macos")]
const SIM_CONFIG: &str = r#"
[simulators]
max_booted = 1
max_devices = 2
idle_seconds = 120
default = "phone"
[simulators.profiles.phone]
device = "Phone"
runtime = "iOS Test"
[simulators.profiles.tablet]
device = "Tablet"
runtime = "iOS Test"
"#;

#[test]
#[cfg(target_os = "macos")]
fn simulator_exclusivity_wait_reuse_scope_and_removal() {
    let fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    fixture.add("first");
    fixture.add("second");
    let overview = fixture.ok(&["sim", "first"]);
    assert_eq!(overview["policy"]["max_booted"], 1);
    assert_eq!(overview["policy"]["max_devices"], 2);
    assert!(overview["policy"]["profiles"]["phone"].is_object());
    assert_eq!(overview["simulators"], serde_json::json!([]));
    assert_eq!(fixture.ok(&["sim", "list", "first"]), overview);
    let first = fixture.ok(&["sim", "acquire", "first"]);
    assert_eq!(first["state"], "leased");
    assert_eq!(fixture.ok(&["status", "first"])["simulators"][0], first);
    assert_eq!(fixture.ok(&["sim", "acquire", "first"]), first);
    let busy = fixture.run(&["--json", "sim", "acquire", "second"]);
    assert_eq!(busy.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&busy.stdout).unwrap()["acquired"],
        false
    );
    let mut waiting = fixture
        .command()
        .args(["--json", "sim", "acquire", "second", "--wait", "10"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(150));
    assert!(waiting.try_wait().unwrap().is_none());
    fixture.ok(&["sim", "release", "default", "first"]);
    assert_eq!(
        fixture.ok(&["status", "first"])["simulators"],
        serde_json::json!([])
    );
    let second = waiting.wait_with_output().unwrap();
    assert!(second.status.success());
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["udid"], first["udid"]);
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    assert!(
        !events.contains("erase") && !events.contains("shutdown"),
        "normal handoff must preserve simulator state without rebooting"
    );
    let scoped = fixture.run(&[
        "exec",
        "first",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "sim",
        "release",
        "default",
        "second",
    ]);
    assert!(!scoped.status.success());
    let listed = fixture.run(&[
        "exec",
        "first",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "--json",
        "sim",
        "--all",
    ]);
    assert_eq!(
        serde_json::from_slice::<Value>(&listed.stdout).unwrap()["simulators"],
        serde_json::json!([])
    );
    fixture.ok(&["rm", "first"]);
    assert_eq!(
        fixture.ok(&["sim", "--all"])["simulators"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.ok(&["rm", "second"]);
    assert_eq!(
        fixture.ok(&["sim", "--all"])["simulators"],
        serde_json::json!([])
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("sim-devices.json")).unwrap(),
        "[]"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn simulator_failures_retain_claims_and_restart_never_reassigns_them() {
    let mut fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    fixture.add("first");
    fixture.add("second");
    fs::write(fixture.root.path().join("sim-fail"), "bootstatus").unwrap();
    assert!(!fixture.run(&["sim", "acquire", "first"]).status.success());
    assert_eq!(
        fixture.ok(&["sim", "first"])["simulators"][0]["state"],
        "failed"
    );
    fs::remove_file(fixture.root.path().join("sim-fail")).unwrap();
    fixture.ok(&["sim", "release", "default", "first"]);
    let lease = fixture.ok(&["sim", "acquire", "first"]);
    fixture.daemon.child.kill().unwrap();
    fixture.daemon.child.wait().unwrap();
    fixture.restart();
    assert_eq!(fixture.ok(&["sim", "acquire", "first"]), lease);
    assert_eq!(
        fixture.run(&["sim", "acquire", "second"]).status.code(),
        Some(2)
    );
    fs::write(fixture.root.path().join("sim-fail"), "delete").unwrap();
    assert!(!fixture.run(&["rm", "first"]).status.success());
    assert_eq!(
        fixture.ok(&["sim", "first"])["simulators"][0]["udid"],
        lease["udid"]
    );
    fs::remove_file(fixture.root.path().join("sim-fail")).unwrap();
    fixture.ok(&["rm", "first"]);
    assert_eq!(
        fixture.ok(&["sim", "--all"])["simulators"],
        serde_json::json!([])
    );
}

#[test]
#[cfg(target_os = "macos")]
fn simulator_requires_confirmed_native_boot_and_shutdown_states() {
    let fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    fixture.add("worker");
    let overrides = fixture.root.path().join("sim-state-overrides.json");
    for native in ["Creating", "Booting", "Shutting Down", "Future State"] {
        fs::write(
            &overrides,
            serde_json::json!({"bootstatus": native}).to_string(),
        )
        .unwrap();
        let output = fixture.run(&["sim", "acquire", "worker"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("did not finish booting"));
        let failed = fixture.ok(&["sim", "worker"])["simulators"][0].clone();
        assert_eq!(failed["state"], "failed");

        fs::write(
            &overrides,
            serde_json::json!({"shutdown": native}).to_string(),
        )
        .unwrap();
        let output = fixture.run(&["sim", "release", "default", "worker"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("has not shut down"));
        assert_eq!(
            fixture.ok(&["sim", "worker"])["simulators"][0]["udid"],
            failed["udid"]
        );
        let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
        assert!(!events.contains(&format!("[\"delete\", {}]", failed["udid"])));
        fs::remove_file(&overrides).unwrap();
        fixture.ok(&["sim", "release", "default", "worker"]);
    }

    let lease = fixture.ok(&["sim", "acquire", "worker"]);
    let devices_path = fixture.root.path().join("sim-devices.json");
    let mut devices: Value =
        serde_json::from_str(&fs::read_to_string(&devices_path).unwrap()).unwrap();
    for native in ["Shutdown", "Booting", "Shutting Down", "Future State"] {
        devices[0]["state"] = serde_json::json!(native);
        fs::write(&devices_path, devices.to_string()).unwrap();
        let output = fixture.run(&["sim", "acquire", "worker"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("no longer booted/available"));
        assert_eq!(fixture.ok(&["sim", "worker"])["simulators"][0], lease);
    }
    devices[0]["state"] = serde_json::json!("Booted");
    fs::write(&devices_path, devices.to_string()).unwrap();
    assert_eq!(fixture.ok(&["sim", "acquire", "worker"]), lease);
}

#[test]
#[cfg(target_os = "macos")]
fn simulator_policy_capacity_reclamation_and_external_devices() {
    let fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    let workspace = fixture.add("worker");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[simulators]\npreferred = [\"tablet\"]\n",
    )
    .unwrap();
    let tablet = fixture.ok(&["sim", "acquire", "worker"]);
    assert_eq!(tablet["device"], "type.Tablet");
    fixture.ok(&["sim", "release", "default", "worker"]);
    let phone = fixture.ok(&["sim", "acquire", "worker", "--profile", "phone"]);
    assert_ne!(phone["udid"], tablet["udid"]);
    let devices: Value = serde_json::from_str(
        &fs::read_to_string(fixture.root.path().join("sim-devices.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        devices
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["state"] == "Booted")
            .count(),
        1
    );
    fixture.ok(&["sim", "release", "default", "worker"]);
    let unavailable = fixture.run(&[
        "sim",
        "acquire",
        "worker",
        "--device",
        "Phone",
        "--runtime",
        "Missing",
    ]);
    assert!(!unavailable.status.success());
    assert!(String::from_utf8_lossy(&unavailable.stderr).contains("does not download"));
    let mut devices = devices.as_array().unwrap().clone();
    for device in &mut devices {
        device["state"] = serde_json::json!("Shutdown");
    }
    devices.push(serde_json::json!({"name":"Personal simulator","udid":"external","state":"Booted","isAvailable":true}));
    for native in [
        "Booted",
        "Creating",
        "Booting",
        "Shutting Down",
        "Future State",
    ] {
        devices.last_mut().unwrap()["state"] = serde_json::json!(native);
        fs::write(
            fixture.root.path().join("sim-devices.json"),
            serde_json::to_string(&devices).unwrap(),
        )
        .unwrap();
        assert_eq!(
            fixture.run(&["sim", "acquire", "worker"]).status.code(),
            Some(2),
            "external device in state {native} must occupy capacity"
        );
    }
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    assert!(!events.contains("external"));
    fixture.ok(&["rm", "worker", "--yes", "--delete-branch"]);
}

#[test]
#[cfg(target_os = "macos")]
fn simulator_any_policy_pool_limit_and_interrupted_creation_cleanup() {
    let config = SIM_CONFIG.replace("max_devices = 2", "max_devices = 1\nallow_any = true");
    let fixture = Fixture::with_tools(Some(&config), true);
    fixture.add("worker");
    let phone = fixture.ok(&["sim", "acquire", "worker"]);
    fixture.ok(&["sim", "release", "default", "worker"]);
    assert!(
        !fixture
            .run(&[
                "sim",
                "acquire",
                "worker",
                "--device",
                "Watch",
                "--runtime",
                "iOS Test"
            ])
            .status
            .success()
    );
    let watch = fixture.ok(&[
        "sim",
        "acquire",
        "worker",
        "--device",
        "Watch",
        "--runtime",
        "iOS Test",
        "--reason",
        "Watch layout regression",
    ]);
    assert_ne!(watch["udid"], phone["udid"]);
    assert_eq!(
        fixture.ok(&["sim", "--all"])["simulators"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.ok(&["sim", "release", "default", "worker"]);
    fs::write(fixture.root.path().join("sim-lost-create-response"), "1").unwrap();
    assert!(!fixture.run(&["sim", "acquire", "worker"]).status.success());
    let record = fixture.ok(&["sim", "worker"]);
    assert_eq!(record["simulators"][0]["state"], "failed");
    assert!(record["simulators"][0]["udid"].is_null());
    fixture.ok(&["sim", "release", "default", "worker"]);
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("sim-devices.json")).unwrap(),
        "[]"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn simulators_allocate_concurrently_and_idle_expiry_keeps_active_leases() {
    let config = SIM_CONFIG
        .replace("max_booted = 1", "max_booted = 2")
        .replace("idle_seconds = 120", "idle_seconds = 0");
    let fixture = Fixture::with_tools(Some(&config), true);
    fixture.add("first");
    fixture.add("second");
    let first = fixture
        .command()
        .args(["--json", "sim", "acquire", "first"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let second = fixture
        .command()
        .args(["--json", "sim", "acquire", "second"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    assert!(first.status.success() && second.status.success());
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_ne!(first["udid"], second["udid"]);
    fixture.ok(&["sim", "release", "default", "first"]);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let sims = fixture.ok(&["sim", "--all"]);
        if sims["simulators"].as_array().unwrap().len() == 1 {
            assert_eq!(sims["simulators"][0]["udid"], second["udid"]);
            break;
        }
        assert!(Instant::now() < deadline, "idle simulator was not deleted");
        thread::sleep(Duration::from_millis(100));
    }
    fixture.ok(&["rm", "first"]);
    fixture.ok(&["rm", "second"]);
}

#[cfg(target_os = "macos")]
fn set_sim_apps(fixture: &Fixture, udid: &Value, count: usize) {
    let path = fixture.root.path().join("sim-devices.json");
    let mut devices: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    devices
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|d| d["udid"] == *udid)
        .unwrap()["user_apps"] = serde_json::json!(count);
    fs::write(path, serde_json::to_string(&devices).unwrap()).unwrap();
}

#[test]
#[cfg(target_os = "macos")]
fn clean_simulator_requires_reason_minimizes_erasure_and_keeps_audit_after_removal() {
    let config = SIM_CONFIG.replace("max_booted = 1", "max_booted = 2");
    let mut fixture = Fixture::with_tools(Some(&config), true);
    fixture.add("first");
    fixture.add("second");
    fixture.add("third");
    let first = fixture.ok(&["sim", "acquire", "first"]);
    let second = fixture.ok(&["sim", "acquire", "second"]);
    set_sim_apps(&fixture, &first["udid"], 5);
    set_sim_apps(&fixture, &second["udid"], 1);
    fixture.ok(&["sim", "release", "default", "first"]);
    fixture.ok(&["sim", "release", "default", "second"]);
    assert!(
        !fixture
            .run(&["sim", "acquire", "third", "--clean"])
            .status
            .success()
    );
    let clean = fixture.run(&[
        "exec",
        "third",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "--json",
        "sim",
        "acquire",
        "--clean",
        "--reason",
        "Test first-launch permission prompts",
    ]);
    assert!(
        clean.status.success(),
        "{}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let clean: Value = serde_json::from_slice(&clean.stdout).unwrap();
    assert_eq!(clean["udid"], second["udid"]);
    let history = fixture.ok(&["sim", "history", "--all"]);
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["apps_removed"], 1);
    assert_eq!(history[0]["erase_completed"], true);
    assert_eq!(history[0]["action"], "erase");
    assert_eq!(history[0]["status"], "acquired");
    assert_eq!(history[0]["workspace_name"], "third");
    assert_eq!(
        history[0]["request"]["reason"],
        "Test first-launch permission prompts"
    );
    assert!(history[0]["execution_id"].is_string());
    // A second clean request must not wipe a simulator still in use.
    assert!(
        !fixture
            .run(&[
                "sim",
                "acquire",
                "third",
                "--clean",
                "--reason",
                "Accidental duplicate"
            ])
            .status
            .success()
    );
    let history = fixture.ok(&["sim", "history", "--all"]);
    assert_eq!(history[0]["status"], "failed");
    assert!(history[0]["action"].is_null());
    let page = fixture.ok(&[
        "sim",
        "history",
        "--all",
        "--before",
        &history[0]["id"].to_string(),
        "--limit",
        "1",
    ]);
    assert_eq!(page[0]["status"], "acquired");
    let visible = fixture.run(&[
        "exec",
        "first",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "--json",
        "sim",
        "history",
        "--all",
    ]);
    assert_eq!(
        serde_json::from_slice::<Value>(&visible.stdout).unwrap(),
        serde_json::json!([])
    );
    fixture.ok(&["rm", "third"]);
    assert_eq!(fixture.ok(&["sim", "history", "--all"]), history);
    fixture.daemon.child.kill().unwrap();
    fixture.daemon.child.wait().unwrap();
    fixture.restart();
    assert_eq!(fixture.ok(&["sim", "history", "--all"]), history);
    let devices: Value = serde_json::from_str(
        &fs::read_to_string(fixture.root.path().join("sim-devices.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(devices[0]["user_apps"], 5);
}

#[test]
#[cfg(target_os = "macos")]
fn clean_simulator_prefers_fresh_capacity_and_logs_invalid_reasons() {
    let fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    fixture.add("first");
    fixture.add("second");
    let first = fixture.ok(&["sim", "acquire", "first"]);
    set_sim_apps(&fixture, &first["udid"], 3);
    fixture.ok(&["sim", "release", "default", "first"]);
    assert!(
        !fixture
            .run(&["sim", "acquire", "second", "--clean", "--reason", "   "])
            .status
            .success()
    );
    let second = fixture.ok(&[
        "sim",
        "acquire",
        "second",
        "--clean",
        "--reason",
        "Verify clean system settings",
    ]);
    assert_ne!(first["udid"], second["udid"]);
    let history = fixture.ok(&["sim", "history", "--all"]);
    assert_eq!(history[0]["action"], "create");
    assert_eq!(history[1]["status"], "failed");
    assert!(
        !fs::read_to_string(fixture.root.path().join("sim-events"))
            .unwrap()
            .contains("erase")
    );
}

#[test]
#[cfg(target_os = "macos")]
fn clean_simulator_wait_retries_share_one_audit_entry() {
    let config = SIM_CONFIG.replace("max_devices = 2", "max_devices = 1");
    let fixture = Fixture::with_tools(Some(&config), true);
    fixture.add("first");
    fixture.add("second");
    fixture.ok(&["sim", "acquire", "first"]);
    let waiting = fixture
        .command()
        .args([
            "--json",
            "sim",
            "acquire",
            "second",
            "--clean",
            "--reason",
            "Verify no login session",
            "--wait",
            "10",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let entries = fixture.ok(&["sim", "history", "--all"]);
        if entries.as_array().unwrap().len() == 1 && entries[0]["status"] == "busy" {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    fixture.ok(&["sim", "release", "default", "first"]);
    let output = waiting.wait_with_output().unwrap();
    assert!(output.status.success());
    let entries = fixture.ok(&["sim", "history", "--all"]);
    assert_eq!(entries.as_array().unwrap().len(), 1);
    assert!(entries[0]["attempts"].as_u64().unwrap() >= 2);
    assert_eq!(entries[0]["status"], "acquired");
    assert_eq!(entries[0]["erase_completed"], true);
}

#[test]
#[cfg(target_os = "macos")]
fn clean_simulator_can_replace_an_empty_incompatible_device_to_preserve_apps() {
    let config = SIM_CONFIG.replace("max_booted = 1", "max_booted = 2");
    let fixture = Fixture::with_tools(Some(&config), true);
    fixture.add("phone");
    fixture.add("tablet");
    fixture.add("requester");
    let phone = fixture.ok(&["sim", "acquire", "phone"]);
    let tablet = fixture.ok(&["sim", "acquire", "tablet", "--profile", "tablet"]);
    set_sim_apps(&fixture, &phone["udid"], 5);
    fixture.ok(&["sim", "release", "default", "phone"]);
    fixture.ok(&["sim", "release", "default", "tablet"]);
    let clean = fixture.ok(&[
        "sim",
        "acquire",
        "requester",
        "--clean",
        "--reason",
        "Check default OS settings",
    ]);
    assert_ne!(clean["udid"], phone["udid"]);
    assert_ne!(clean["udid"], tablet["udid"]);
    let history = fixture.ok(&["sim", "history", "--all"]);
    assert_eq!(history[0]["action"], "create_after_eviction");
    assert_eq!(history[0]["evicted"][0]["udid"], tablet["udid"]);
    assert_eq!(history[0]["evicted"][0]["installed_apps"], 0);
    assert!(
        !fs::read_to_string(fixture.root.path().join("sim-events"))
            .unwrap()
            .contains("erase")
    );
}

#[test]
#[cfg(target_os = "macos")]
fn clean_simulator_daemon_requires_reason_and_records_erase_failures() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixStream,
    };
    let config = SIM_CONFIG.replace("max_devices = 2", "max_devices = 1");
    let fixture = Fixture::with_tools(Some(&config), true);
    let workspace = fixture.add("worker");
    let call = |value: Value| {
        let mut stream =
            UnixStream::connect(fixture.root.path().join("state/daemon.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(stream, "{value}").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let protocol =
        call(serde_json::json!({"protocol":0,"id":1,"method":"status"}))["protocol"].clone();
    let response = call(
        serde_json::json!({"protocol":protocol,"id":2,"method":{"sim_acquire":{"workspace":workspace["id"],"request":{"request_id":uuid::Uuid::new_v4().to_string(),"clean":true,"name":"default","profile":null,"device":null,"runtime":null,"reason":null}}}}),
    );
    assert_eq!(response["type"], "error");
    assert!(
        response["data"]["message"]
            .as_str()
            .unwrap()
            .contains("--clean requires --reason")
    );
    assert_eq!(
        fixture.ok(&["sim", "history", "--all"])[0]["status"],
        "failed"
    );
    assert!(!fixture.root.path().join("sim-devices.json").exists());
    fixture.ok(&["sim", "acquire", "worker"]);
    fixture.ok(&["sim", "release", "default", "worker"]);
    fs::write(fixture.root.path().join("sim-fail"), "erase").unwrap();
    assert!(
        !fixture
            .run(&[
                "sim",
                "acquire",
                "worker",
                "--clean",
                "--reason",
                "Reset permission state"
            ])
            .status
            .success()
    );
    let entry = &fixture.ok(&["sim", "history", "--all"])[0];
    assert_eq!(entry["status"], "failed");
    assert_eq!(entry["action"], "erase");
    assert_eq!(entry["erase_completed"], false);
    assert!(entry["error"].as_str().unwrap().contains("injected"));
}

#[test]
#[cfg(target_os = "macos")]
fn simulator_mutations_observe_persisted_audits_and_failure_retains_ownership() {
    let config = SIM_CONFIG.replace("max_devices = 2", "max_devices = 1");
    for failure in ["create", "shutdown", "erase", "bootstatus", "delete"] {
        let mut fixture = Fixture::with_tools(Some(&config), true);
        let workspace = fixture.add("worker");
        let previous = if failure == "create" {
            None
        } else {
            let sim = fixture.ok(&["sim", "acquire", "worker"]);
            fixture.ok(&["sim", "release", "default", "worker"]);
            Some(sim)
        };
        fs::write(fixture.root.path().join("sim-observe-db"), "").unwrap();
        fs::write(fixture.root.path().join("sim-fail"), failure).unwrap();
        let profile = if failure == "delete" {
            "tablet"
        } else {
            "phone"
        };
        let output = fixture.run(&[
            "sim",
            "acquire",
            "worker",
            "--profile",
            profile,
            "--clean",
            "--reason",
            "Test persistent mutation ordering",
        ]);
        assert!(!output.status.success(), "{failure}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("injected"),
            "{output:?}"
        );
        let events =
            fs::read_to_string(fixture.root.path().join("sim-persistence-events")).unwrap();
        let events: Vec<Value> = events
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let event = events
            .iter()
            .find(|event| event["command"] == failure)
            .unwrap();
        let saved = &event["simulators"][0];
        let audit = &event["simulator_clean_requests"][0];
        assert_eq!(audit["status"], "requested");
        assert_eq!(audit["workspace_id"], workspace["id"]);
        if failure == "delete" {
            assert_eq!(saved["id"], previous.as_ref().unwrap()["id"]);
            assert_eq!(saved["state"], "idle");
            assert_eq!(audit["action"], "create_after_eviction");
            assert_eq!(audit["evicted"][0]["id"], saved["id"]);
            assert_eq!(audit["evicted"][0]["udid"], event["args"][0]);
        } else {
            assert_eq!(saved["state"], "booting");
            assert_eq!(saved["workspace_id"], workspace["id"]);
            assert_eq!(saved["lease_name"], "default");
            assert_eq!(audit["simulator_id"], saved["id"]);
            assert_eq!(audit["udid"], saved["udid"]);
            if failure == "create" {
                assert!(saved["udid"].is_null());
                assert_eq!(audit["action"], "create");
                assert_eq!(
                    event["args"][0],
                    format!("shoal-{}", saved["id"].as_str().unwrap())
                );
            } else {
                assert_eq!(saved["udid"], event["args"][0]);
                assert_eq!(audit["action"], "erase");
                assert_eq!(audit["erase_completed"], failure == "bootstatus");
            }
        }
        let retained = fixture.ok(&["sim", "--all"])["simulators"][0].clone();
        assert_eq!(retained["id"], saved["id"]);
        assert_eq!(retained["workspace_id"], saved["workspace_id"]);
        assert_eq!(
            retained["state"],
            if failure == "delete" {
                "idle"
            } else {
                "failed"
            }
        );
        assert_eq!(
            fixture.ok(&["sim", "history", "--all"])[0]["status"],
            "failed"
        );
        fixture.restart();
        assert_eq!(fixture.ok(&["sim", "--all"])["simulators"][0], retained);
    }
}

const RESOURCE_CONFIG: &str = r#"
[resources.signing]
capacity = 1
reason = "Signing service"
[resource_pools.devices]
capacity = 2
[resource_pools.devices.resources.alpha]
capacity = 1
reason = "Alpha test device"
[resource_pools.devices.resources.beta]
capacity = 2
"#;

#[test]
fn resources_enforce_pool_and_member_capacity_and_named_permits() {
    let fixture = Fixture::with_config(Some(RESOURCE_CONFIG));
    fixture.add("first");
    fixture.add("second");
    fixture.add("third");
    assert!(
        fixture
            .ok(&["resource", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .all(|overview| overview["leases"] == serde_json::json!([]))
    );
    let first = fixture.ok(&["resource", "acquire", "devices", "first"]);
    assert_eq!(first["resource"], "alpha");
    assert_eq!(first["reason"], "Alpha test device");
    assert_eq!(
        fixture.ok(&["resource", "acquire", "devices", "first"]),
        first
    );
    assert_eq!(
        fixture
            .run(&[
                "resource",
                "acquire",
                "devices",
                "second",
                "--resource",
                "alpha"
            ])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&[
        "resource",
        "acquire",
        "devices",
        "second",
        "--resource",
        "beta",
    ]);
    let busy = fixture.run(&[
        "--json",
        "resource",
        "acquire",
        "devices",
        "third",
        "--resource",
        "beta",
    ]);
    assert_eq!(busy.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&busy.stdout).unwrap()["acquired"],
        false
    );
    fixture.ok(&["resource", "release", "devices", "first"]);
    fixture.ok(&[
        "resource",
        "acquire",
        "devices",
        "third",
        "--resource",
        "beta",
    ]);
    let overview = fixture.ok(&["resource", "third"]);
    let pool = overview["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "devices")
        .unwrap();
    assert_eq!(pool["used"], 2);
    assert_eq!(pool["available"], 0);
    let beta = pool["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "beta")
        .unwrap();
    assert_eq!(beta["used"], 2);
    fixture.ok(&["resource", "release", "devices", "second"]);
    fixture.ok(&[
        "resource",
        "acquire",
        "devices",
        "third",
        "--resource",
        "beta",
        "--name",
        "parallel",
        "--reason",
        "Parallel job",
    ]);
    assert_eq!(
        fixture.ok(&["inspect", "third"])["resources"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let lock = fixture.ok(&["resource", "acquire", "signing", "first"]);
    assert_eq!(lock["resource"], "signing");
    assert_eq!(lock["scope"], "global");
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "signing", "second"])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&["rm", "first"]);
    fixture.ok(&["resource", "acquire", "signing", "second"]);
    fixture.ok(&["rm", "third"]);
    let overviews = fixture.ok(&["resource", "--all"]);
    let mut device_leases = overviews
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|overview| overview["leases"].as_array().unwrap());
    assert!(device_leases.all(|l| l["pool"] != "devices"));
}

#[test]
fn resource_claims_are_atomic_persistent_and_wait_for_release() {
    let mut fixture = Fixture::with_config(Some("[resources.workers]\ncapacity=3\n"));
    let workspace = fixture.add("worker");
    let children: Vec<_> = (0..10)
        .map(|i| {
            fixture
                .command()
                .args([
                    "--json",
                    "resource",
                    "acquire",
                    "workers",
                    "worker",
                    "--name",
                    &format!("job-{i}"),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut successes = vec![];
    for child in children {
        let output = child.wait_with_output().unwrap();
        if output.status.success() {
            successes.push(serde_json::from_slice::<Value>(&output.stdout).unwrap());
        } else {
            assert_eq!(
                output.status.code(),
                Some(2),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    assert_eq!(successes.len(), 3);
    let leases = fixture.ok(&["resource", "--all"]);
    assert_eq!(leases[0]["leases"].as_array().unwrap().len(), 3);
    fixture.daemon.child.kill().unwrap();
    fixture.daemon.child.wait().unwrap();
    fixture.restart();
    assert_eq!(fixture.ok(&["resource", "--all"]), leases);
    let mut waiter = fixture
        .command()
        .args([
            "--json", "resource", "acquire", "workers", "worker", "--name", "waiter", "--wait",
            "10",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(150));
    assert!(waiter.try_wait().unwrap().is_none());
    fixture.ok(&[
        "resource",
        "release",
        "workers",
        "worker",
        "--name",
        successes[0]["name"].as_str().unwrap(),
    ]);
    assert!(waiter.wait_with_output().unwrap().status.success());
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join("dirty"),
        "retain",
    )
    .unwrap();
    assert!(!fixture.run(&["rm", "worker"]).status.success());
    assert_eq!(
        fixture.ok(&["resource", "--all"])[0]["leases"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    fixture.ok(&["rm", "worker", "--yes", "--keep-branch"]);
    assert!(
        fixture
            .ok(&["resource", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .all(|overview| overview["leases"] == serde_json::json!([]))
    );
}

#[test]
fn inline_repository_config_resolves_relative_paths_in_the_callers_directory() {
    let fixture = Fixture::new();
    for args in [
        vec!["config", "set", "default_agent", "claude", "--repo", "."],
        vec!["config", "unset", "default_agent", "--repo", "."],
    ] {
        let output = fixture
            .command()
            .current_dir(&fixture.repo)
            .arg("--json")
            .args(&args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result["repository_id"],
            fixture.ok(&["repo", "list"])[0]["id"]
        );
        assert_eq!(
            result,
            fixture.ok(&["repo", "config", fixture.repo.to_str().unwrap()])
        );
    }
}

#[test]
fn inline_repository_config_edits_preserve_layers_and_serialize_updates() {
    let mut fixture = Fixture::new();
    commit_resource_config(&fixture.repo, "default_agent = 'codex'\n");
    fixture.add("worker");
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let saved = fixture.ok(&["config", "set", "default_agent", "claude", "--repo", id]);
    assert_eq!(saved["repository_id"], id);
    let agent = || {
        fixture
            .ok(&["config", "show", "worker"])
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["key"] == "default_agent")
            .unwrap()
            .clone()
    };
    assert_eq!(agent()["value"], "claude");
    assert_eq!(agent()["layer"], "saved_repository_config");
    for (key, value) in [
        ("auto_cleanup.enabled", "maybe"),
        ("root_dir", "/tmp/no"),
        ("default_agnet", "codex"),
    ] {
        assert!(
            !fixture
                .run(&["config", "set", key, value, "--repo", id])
                .status
                .success()
        );
        assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    }
    fixture.ok(&["config", "unset", "default_agent", "--repo", id]);
    assert_eq!(agent()["value"], "codex");
    assert_eq!(agent()["layer"], "worktree_file");
    assert_eq!(
        fs::read_to_string(fixture.repo.join(".shoal.toml")).unwrap(),
        "default_agent = 'codex'\n"
    );
    thread::scope(|scope| {
        for index in 0..8 {
            let fixture = &fixture;
            scope.spawn(move || {
                fixture.ok(&[
                    "config",
                    "set",
                    &format!("commands.check{index}"),
                    "['true']",
                    "--repo",
                    id,
                ]);
            });
        }
    });
    let saved = fixture.ok(&["repo", "config", id]);
    let config: toml::Value = toml::from_str(saved["toml"].as_str().unwrap()).unwrap();
    assert_eq!(config["commands"].as_table().unwrap().len(), 8);
    fixture.restart();
    assert_eq!(fixture.ok(&["repo", "config", id]), saved);
}

#[test]
fn local_repository_config_is_copied_shared_persistent_and_reversible() {
    let mut fixture = Fixture::new();
    let first = fixture.add("first");
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    assert!(fixture.ok(&["repo", "config", id])["toml"].is_null());
    let input = fixture.root.path().join("local.toml");
    let text = "# Local preferences\n[ports.web]\nenv='LOCAL_PORT'\n[resources.lock]\ncapacity=1\n";
    fs::write(&input, text).unwrap();
    let saved = fixture.ok(&["repo", "config", id, "--file", input.to_str().unwrap()]);
    assert_eq!(saved["repository_id"], id);
    assert_eq!(saved["toml"], text);
    assert_eq!(fixture.run(&["repo", "config", id]).stdout, text.as_bytes());
    fs::remove_file(&input).unwrap();
    fixture.ok(&["repo", "rename", id, "renamed"]);
    fixture.restart();
    assert_eq!(fixture.ok(&["repo", "config", "renamed"]), saved);
    fixture.add("second");
    for path in [&fixture.repo, Path::new(first["path"].as_str().unwrap())] {
        assert!(git(path, &["status", "--porcelain"]).is_empty());
        assert!(!path.join(".shoal.toml").exists());
        assert!(!path.join(".shoal").exists());
    }
    for name in ["first", "second"] {
        assert_eq!(
            fixture.ok(&["port", name])["configured"]["web"]["env"],
            "LOCAL_PORT"
        );
    }
    let port = fixture.ok(&["port", "acquire", "web", "first"]);
    assert_eq!(port["env_var"], "LOCAL_PORT");
    fixture.ok(&["resource", "acquire", "lock", "first"]);
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "lock", "second"])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&["resource", "release", "lock", "first"]);
    fixture.ok(&["rm", "second", "--yes", "--delete-branch"]);
    assert_eq!(fixture.ok(&["repo", "config", id]), saved);

    let path = Path::new(first["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[ports.checked_in]\nenv='CHECKED_IN'\n",
    )
    .unwrap();
    fs::create_dir(path.join(".shoal")).unwrap();
    fs::write(path.join(".shoal/config.toml"), "invalid TOML").unwrap();
    // The worktree file is a layer of its own; its errors show through the saved config.
    assert!(!fixture.run(&["port", "first"]).status.success());
    fs::remove_file(path.join(".shoal/config.toml")).unwrap();
    let configured = fixture.ok(&["port", "first"])["configured"].clone();
    assert_eq!(configured.as_object().unwrap().len(), 2);
    assert_eq!(configured["web"]["env"], "LOCAL_PORT");
    assert_eq!(configured["checked_in"]["env"], "CHECKED_IN");
    // Layered names must agree: the saved `lock` resource meets a `lock` pool.
    fs::write(
        path.join(".shoal.toml"),
        "[resource_pools.lock.resources.a]\ncapacity=1\n",
    )
    .unwrap();
    assert!(!fixture.run(&["port", "first"]).status.success());
    fs::write(
        path.join(".shoal.toml"),
        "[ports.checked_in]\nenv='CHECKED_IN'\n",
    )
    .unwrap();
    // Invalid replacements must leave the saved config intact.
    for invalid in [
        "invalid TOML",
        "unknown=true",
        "[ports.web]\nport=0",
        "[resources.lock]\ncapacity=0",
    ] {
        fs::write(&input, invalid).unwrap();
        assert!(
            !fixture
                .run(&["repo", "config", id, "--file", input.to_str().unwrap()])
                .status
                .success()
        );
        assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    }
    // An empty saved config sets nothing, so every option falls through.
    fs::write(&input, "").unwrap();
    fixture.ok(&["repo", "config", id, "--file", input.to_str().unwrap()]);
    let configured = fixture.ok(&["port", "first"])["configured"].clone();
    assert_eq!(configured.as_object().unwrap().len(), 1);
    assert_eq!(configured["checked_in"]["env"], "CHECKED_IN");
    assert!(fixture.ok(&["repo", "config", id, "--clear"])["toml"].is_null());
    assert_eq!(
        fixture.ok(&["port", "first"])["configured"]["checked_in"]["env"],
        "CHECKED_IN"
    );
    assert_eq!(fixture.ok(&["port", "first"])["reserved"][0], port);
    fixture.ok(&["repo", "config", id, "--clear"]);
}

#[test]
fn local_repository_config_is_isolated_and_deleted_only_with_its_repository() {
    let fixture = Fixture::new();
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let input = fixture.root.path().join("local.toml");
    fs::write(&input, "[ports.web]\n").unwrap();
    let saved = fixture.ok(&["repo", "config", id, "--file", input.to_str().unwrap()]);
    let other_path = fixture.root.path().join("other-repo");
    fs::create_dir(&other_path).unwrap();
    git(&other_path, &["init", "-b", "main"]);
    let other = fixture.ok(&["repo", "add", other_path.to_str().unwrap()]);
    assert!(fixture.ok(&["repo", "config", other["id"].as_str().unwrap()])["toml"].is_null());
    let outside = fixture.root.path().join("outside");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "outside",
            outside.to_str().unwrap(),
        ],
    );
    assert!(!fixture.run(&["repo", "rm", id, "--yes"]).status.success());
    assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    git(
        &fixture.repo,
        &["worktree", "remove", outside.to_str().unwrap()],
    );
    fixture.ok(&["repo", "rm", id, "--yes"]);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM repository_configs", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(input.exists());
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
}

#[test]
#[cfg(target_os = "macos")]
fn local_repository_config_selects_simulator_preferences() {
    let fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    fixture.add("worker");
    let input = fixture.root.path().join("local.toml");
    fs::write(&input, "[simulators]\npreferred=['tablet']\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        input.to_str().unwrap(),
    ]);
    let sim = fixture.ok(&["sim", "acquire", "worker"]);
    assert_eq!(sim["device"], "type.Tablet");
}

fn commit_resource_config(repo: &Path, config: &str) {
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

#[test]
fn repo_resources_share_across_branches_and_refuse_conflicting_definitions() {
    let fixture = Fixture::new();
    commit_resource_config(&fixture.repo, RESOURCE_CONFIG);
    let first = fixture.add("first");
    let second = fixture.add("second");
    let first_path = Path::new(first["path"].as_str().unwrap());
    let second_path = Path::new(second["path"].as_str().unwrap());
    let lease = fixture.ok(&["resource", "acquire", "signing", "first"]);
    assert!(lease["scope"].as_str().unwrap().starts_with("repo/"));
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "signing", "second"])
            .status
            .code(),
        Some(2)
    );
    fs::write(
        second_path.join(".shoal.toml"),
        RESOURCE_CONFIG.replace(
            "capacity = 1\nreason = \"Signing service\"",
            "capacity = 2\nreason = \"Signing service\"",
        ),
    )
    .unwrap();
    let conflict = fixture.run(&["resource", "acquire", "signing", "second"]);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("definition changed"));
    let overview = fixture.ok(&["resource", "second"]);
    let signing = overview["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "signing")
        .unwrap();
    assert_eq!(signing["configuration_matches"], false);
    fs::remove_file(first_path.join(".shoal.toml")).unwrap();
    fixture.ok(&["resource", "release", "signing", "first"]);
    fixture.ok(&["resource", "acquire", "signing", "second"]);
    fixture.ok(&[
        "resource", "acquire", "signing", "second", "--name", "another",
    ]);
    let output = fixture
        .command()
        .current_dir(second_path)
        .args(["--json", "resource"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["leases"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn resource_scopes_separate_repos_share_global_capacity_and_limit_agents() {
    let fixture = Fixture::with_config(Some("[resources.machine]\ncapacity=1\n"));
    commit_resource_config(&fixture.repo, "[resources.local]\ncapacity=1\n");
    fixture.add("first");
    let other = fixture.root.path().join("other");
    fs::create_dir(&other).unwrap();
    git(&other, &["init", "-b", "main"]);
    commit_resource_config(&other, "[resources.local]\ncapacity=1\n");
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let second = fixture.ok(&["add", other.to_str().unwrap(), "second"]);
    let local_first = fixture.ok(&["resource", "acquire", "local", "first"]);
    let local_second = fixture.ok(&["resource", "acquire", "local", "second"]);
    assert_ne!(local_first["scope"], local_second["scope"]);
    fixture.ok(&["resource", "acquire", "machine", "first"]);
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "machine", "second"])
            .status
            .code(),
        Some(2)
    );
    let scoped = |args: &[&str]| {
        fixture
            .command()
            .args(["exec", "first", "--", env!("CARGO_BIN_EXE_shoal")])
            .args(args)
            .output()
            .unwrap()
    };
    assert!(scoped(&["resource", "acquire", "local"]).status.success());
    assert!(
        !scoped(&["resource", "release", "local", "second"])
            .status
            .success()
    );
    assert!(!scoped(&["resource", "second"]).status.success());
    let listed: Value =
        serde_json::from_slice(&scoped(&["--json", "resource", "--all"]).stdout).unwrap();
    let leases = listed[0]["leases"].as_array().unwrap();
    assert_eq!(leases.len(), 2);
    assert!(
        leases
            .iter()
            .all(|l| l["workspace_id"] == local_first["workspace_id"])
    );
    fs::write(
        Path::new(second["path"].as_str().unwrap()).join(".shoal.toml"),
        "[resources.machine]\ncapacity=10\n",
    )
    .unwrap();
    let conflict = fixture.run(&["resource", "acquire", "machine", "second"]);
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("conflicts with the global"));
    fixture.ok(&["resource", "release", "local", "second"]);
}

// Local bare origin plus a separate author checkout: no network or personal repos.
fn upstream_remote(fixture: &Fixture) -> PathBuf {
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

#[test]
fn add_uses_develop_as_the_repository_default() {
    let fixture = Fixture::new();
    git(&fixture.repo, &["branch", "-m", "develop"]);
    let author = upstream_remote(&fixture);
    let expected = git(&author, &["rev-parse", "HEAD"]);
    let workspace = fixture.add("henrik/8374-set-league-season-player-profile");
    assert_eq!(workspace["base_ref"], "refs/heads/develop");
    assert_eq!(workspace["base_commit"], expected.trim());
    let name = workspace["name"].as_str().unwrap();
    assert_eq!(fixture.ok(&["rm", name])["branch_deleted"], true);
}

#[test]
fn add_base_accepts_branches_tags_commits_and_revision_expressions() {
    let fixture = Fixture::new();
    let initial = git(&fixture.repo, &["rev-parse", "HEAD"]);
    git(&fixture.repo, &["checkout", "-b", "feature/source"]);
    fs::write(fixture.repo.join("source-only"), "base branch content\n").unwrap();
    git(&fixture.repo, &["add", "."]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "source change",
        ],
    );
    let source = git(&fixture.repo, &["rev-parse", "HEAD"]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "tag",
            "-a",
            "v1",
            "-m",
            "release",
        ],
    );
    git(
        &fixture.repo,
        &["update-ref", "refs/remotes/origin/topic", source.trim()],
    );
    git(&fixture.repo, &["checkout", "main"]);
    // Explicit non-default bases must work even when the default cannot refresh.
    git(
        &fixture.repo,
        &["remote", "add", "origin", "/nonexistent/shoal-test-remote"],
    );
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    for (index, (base, expected, reference)) in [
        (
            "feature/source",
            source.trim(),
            Some("refs/heads/feature/source"),
        ),
        (
            "refs/heads/feature/source",
            source.trim(),
            Some("refs/heads/feature/source"),
        ),
        (
            "origin/topic",
            source.trim(),
            Some("refs/remotes/origin/topic"),
        ),
        ("v1", source.trim(), Some("refs/tags/v1")),
        (source.trim(), source.trim(), None),
        ("feature/source~1", initial.trim(), None),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("from-base-{index}");
        let workspace = fixture.ok(&["add", fixture.repo.to_str().unwrap(), &name, "--base", base]);
        let path = Path::new(workspace["path"].as_str().unwrap());
        assert_eq!(git(path, &["rev-parse", "HEAD"]).trim(), expected);
        assert_eq!(workspace["base_commit"], expected);
        assert_eq!(workspace["base_ref"], serde_json::json!(reference));
        fs::write(path.join("tracked"), "workspace change\n").unwrap();
        let output = fixture.run(&["diff", &name]);
        assert!(output.status.success(), "{output:?}");
        let diff = String::from_utf8(output.stdout).unwrap();
        assert!(diff.contains("workspace change"));
        assert!(!diff.contains("source-only"));
    }
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), initial);
    assert_eq!(git(&fixture.repo, &["rev-parse", "feature/source"]), source);
}

#[test]
fn add_refreshes_main_before_creating_the_worktree() {
    let fixture = Fixture::new();
    let author = upstream_remote(&fixture);
    let expected = git(&author, &["rev-parse", "HEAD"]);
    let workspace = fixture.add("fresh");
    let path = Path::new(workspace["path"].as_str().unwrap());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), expected);
    assert_eq!(git(path, &["rev-parse", "HEAD"]), expected);
    assert_eq!(workspace["base_ref"], "refs/heads/main");
    assert_eq!(workspace["base_commit"], expected.trim());
    assert_eq!(
        fs::read_to_string(path.join("upstream")).unwrap(),
        "from remote\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("upstream")).unwrap(),
        "from remote\n"
    );
}

#[test]
fn rwlock_readers_share_one_slot_and_writers_exclude_everyone() {
    let fixture = Fixture::with_config(Some("[resources.cache]\nkind='rwlock'\n"));
    fixture.add("first");
    fixture.add("second");
    let first = fixture.ok(&["resource", "acquire", "cache", "first", "--mode", "read"]);
    assert_eq!(first["mode"], "read");
    assert_eq!(
        fixture.ok(&["resource", "acquire", "cache", "first"]),
        first
    );
    assert!(
        !fixture
            .run(&["resource", "acquire", "cache", "first", "--mode", "write"])
            .status
            .success()
    );
    let children: Vec<_> = (0..16)
        .map(|i| {
            fixture
                .command()
                .args([
                    "--json",
                    "resource",
                    "acquire",
                    "cache",
                    "second",
                    "--mode",
                    "read",
                    "--name",
                    &format!("reader-{i}"),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let overview = fixture.ok(&["resource", "first"]);
    let pool = &overview["pools"][0];
    assert_eq!(pool["used"], 1);
    assert_eq!(pool["available"], 0);
    assert_eq!(pool["resources"][0]["readers"], 17);
    assert_eq!(pool["resources"][0]["read_available"], true);
    assert_eq!(pool["resources"][0]["write_available"], false);
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "cache", "second", "--mode", "write"])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&["resource", "release", "cache", "first"]);
    // The final reader, not the first release, makes a writer possible.
    for i in 0..15 {
        fixture.ok(&[
            "resource",
            "release",
            "cache",
            "second",
            "--name",
            &format!("reader-{i}"),
        ]);
    }
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "cache", "first"])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&[
        "resource",
        "release",
        "cache",
        "second",
        "--name",
        "reader-15",
    ]);
    let writer = fixture.ok(&["resource", "acquire", "cache", "first"]);
    assert_eq!(writer["mode"], "write");
    for mode in ["read", "write"] {
        assert_eq!(
            fixture
                .run(&["resource", "acquire", "cache", "second", "--mode", mode])
                .status
                .code(),
            Some(2)
        );
    }
    let status = fixture.ok(&["resource", "second"]);
    assert_eq!(status["pools"][0]["resources"][0]["writers"], 1);
    assert_eq!(status["pools"][0]["resources"][0]["read_available"], false);
    fixture.ok(&["rm", "first"]);
    fixture.ok(&["resource", "acquire", "cache", "second", "--mode", "read"]);
}

#[test]
fn rwlock_mixed_pools_apply_capacity_to_occupied_members_and_filter_modes() {
    let fixture = Fixture::with_config(Some(
        "[resource_pools.mixed]\ncapacity=2\n[resource_pools.mixed.resources.a]\nkind='rwlock'\n[resource_pools.mixed.resources.b]\nkind='rwlock'\n[resource_pools.mixed.resources.worker]\ncapacity=2\n",
    ));
    fixture.add("owner");
    fixture.ok(&[
        "resource",
        "acquire",
        "mixed",
        "owner",
        "--resource",
        "worker",
        "--name",
        "job",
    ]);
    let reader = fixture.ok(&[
        "resource", "acquire", "mixed", "owner", "--mode", "read", "--name", "read-one",
    ]);
    assert_eq!(reader["resource"], "a");
    let more = fixture.ok(&[
        "resource", "acquire", "mixed", "owner", "--mode", "read", "--name", "read-two",
    ]);
    assert_eq!(more["resource"], "a");
    for resource in ["b", "worker"] {
        assert_eq!(
            fixture
                .run(&[
                    "resource",
                    "acquire",
                    "mixed",
                    "owner",
                    "--resource",
                    resource,
                    "--name",
                    "extra"
                ])
                .status
                .code(),
            Some(2)
        );
    }
    for (resource, mode) in [("worker", "read"), ("worker", "write"), ("a", "permit")] {
        let output = fixture.run(&[
            "resource",
            "acquire",
            "mixed",
            "owner",
            "--resource",
            resource,
            "--mode",
            mode,
            "--name",
            "invalid",
        ]);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("incompatible"));
    }
    fixture.ok(&["resource", "release", "mixed", "owner", "--name", "job"]);
    let writer = fixture.ok(&[
        "resource", "acquire", "mixed", "owner", "--mode", "write", "--name", "writer",
    ]);
    assert_eq!(writer["resource"], "b");
    let overview = fixture.ok(&["resource", "owner"]);
    assert_eq!(overview["pools"][0]["used"], 2);
    assert_eq!(overview["pools"][0]["resources"][0]["read_available"], true);
    assert_eq!(
        overview["pools"][0]["resources"][1]["read_available"],
        false
    );
}

#[test]
fn rwlock_claims_are_atomic_across_readers_and_writers() {
    let fixture = Fixture::with_config(Some("[resources.cache]\nkind='rwlock'\n"));
    fixture.add("owner");
    let children: Vec<_> = (0..20)
        .map(|i| {
            fixture
                .command()
                .args([
                    "--json",
                    "resource",
                    "acquire",
                    "cache",
                    "owner",
                    "--mode",
                    if i % 2 == 0 { "write" } else { "read" },
                    "--name",
                    &format!("lease-{i}"),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut reads = 0;
    let mut writes = 0;
    for child in children {
        let output = child.wait_with_output().unwrap();
        if output.status.success() {
            let lease: Value = serde_json::from_slice(&output.stdout).unwrap();
            if lease["mode"] == "read" {
                reads += 1;
            } else {
                writes += 1;
            }
        } else {
            assert_eq!(
                output.status.code(),
                Some(2),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    assert!((reads == 10 && writes == 0) || (writes == 1 && reads == 0));
}

#[test]
fn rwlock_modes_survive_restart_wait_and_failed_removal() {
    let mut fixture = Fixture::with_config(Some(
        "[resources.cache]\nkind='rwlock'\n[resources.index]\nkind='rwlock'\n",
    ));
    let workspace = fixture.add("reader");
    fixture.add("writer");
    let lease = fixture.ok(&["resource", "acquire", "cache", "reader", "--mode", "read"]);
    let writer = fixture.ok(&["resource", "acquire", "index", "writer", "--mode", "write"]);
    fixture.daemon.child.kill().unwrap();
    fixture.daemon.child.wait().unwrap();
    fixture.restart();
    assert_eq!(
        fixture.ok(&["resource", "acquire", "cache", "reader", "--mode", "read"]),
        lease
    );
    assert_eq!(
        fixture.ok(&["resource", "acquire", "index", "writer", "--mode", "write"]),
        writer
    );
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "index", "reader", "--mode", "read"])
            .status
            .code(),
        Some(2)
    );
    let mut waiter = fixture
        .command()
        .args([
            "--json", "resource", "acquire", "cache", "writer", "--mode", "write", "--wait", "10",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(150));
    assert!(waiter.try_wait().unwrap().is_none());
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("unfinished"), "retain").unwrap();
    assert!(!fixture.run(&["rm", "reader"]).status.success());
    assert_eq!(fixture.ok(&["resource", "reader"])["leases"][0], lease);
    fixture.ok(&["resource", "release", "cache", "reader"]);
    let output = waiter.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["mode"],
        "write"
    );
}

#[test]
fn rwlock_scope_and_kind_drift_preserve_active_leases() {
    let fixture = Fixture::new();
    commit_resource_config(&fixture.repo, "[resources.cache]\nkind='rwlock'\n");
    let first = fixture.add("first");
    let second = fixture.add("second");
    let binary = env!("CARGO_BIN_EXE_shoal");
    let output = fixture.run(&[
        "exec", "first", "--", binary, "--json", "resource", "acquire", "cache", "--mode", "read",
    ]);
    assert!(output.status.success());
    let lease: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(lease["mode"], "read");
    assert!(
        !fixture
            .run(&[
                "exec", "second", "--", binary, "resource", "release", "cache", "first"
            ])
            .status
            .success()
    );
    let path = Path::new(second["path"].as_str().unwrap());
    fs::write(path.join(".shoal.toml"), "[resources.cache]\ncapacity=2\n").unwrap();
    assert!(
        !fixture
            .run(&["resource", "acquire", "cache", "second"])
            .status
            .success()
    );
    assert_eq!(
        fixture.ok(&["resource", "second"])["pools"][0]["configuration_matches"],
        false
    );
    // Release still works when its own definition is removed entirely.
    fs::remove_file(Path::new(first["path"].as_str().unwrap()).join(".shoal.toml")).unwrap();
    fixture.ok(&["resource", "release", "cache", "first"]);
    assert_eq!(
        fixture.ok(&["resource", "acquire", "cache", "second"])["mode"],
        "permit"
    );
}

#[test]
fn cd_previous_checks_existence_and_json_never_navigates() {
    let fixture = Fixture::new();
    let workspace = fixture.add("previous");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let directive = fixture.root.path().join("cd-directive");
    fs::write(&directive, "").unwrap();
    let output = fixture
        .command()
        .env("OLDPWD", path)
        .env_remove("SHOAL_PREVIOUS_DIR")
        .env("SHOAL_SHELL_DIRECTIVE", &directive)
        .args(["--json", "cd", "-"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["path"],
        fs::canonicalize(path).unwrap().to_str().unwrap()
    );
    assert_eq!(fs::read_to_string(&directive).unwrap(), "");
    fixture.ok(&["rm", "previous"]);
    let deleted = fixture
        .command()
        .env("OLDPWD", path)
        .env_remove("SHOAL_PREVIOUS_DIR")
        .env("SHOAL_SHELL_DIRECTIVE", &directive)
        .args(["cd", "-"])
        .output()
        .unwrap();
    assert!(!deleted.status.success());
    assert!(String::from_utf8_lossy(&deleted.stderr).contains("no longer exists"));
    assert_eq!(fs::read_to_string(&directive).unwrap(), "");
    for previous in [None, Some("relative/directory")] {
        let mut command = fixture.command();
        command
            .env_remove("OLDPWD")
            .env_remove("SHOAL_PREVIOUS_DIR");
        if let Some(previous) = previous {
            command.env("OLDPWD", previous);
        }
        assert!(!command.args(["cd", "-"]).output().unwrap().status.success());
    }
}

#[test]
fn cd_previous_cannot_escape_execution_scope() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    let other = fixture.add("other");
    let binary = env!("CARGO_BIN_EXE_shoal");
    let scoped = |destination: &str| {
        fixture
            .command()
            .env("SHOAL_PREVIOUS_DIR", destination)
            .args(["exec", "first", "--", binary, "--json", "cd", "-"])
            .output()
            .unwrap()
    };
    assert!(scoped(first["path"].as_str().unwrap()).status.success());
    for path in [
        other["path"].as_str().unwrap(),
        fixture.repo.to_str().unwrap(),
    ] {
        let output = scoped(path);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot navigate outside"));
    }
}

#[test]
fn menu_bindings_preserve_enter_inspect_cancel_and_scope() {
    let fixture = Fixture::new();
    let workspace = fixture.add("menu-worker");
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let picker = bin.join("fzf");
    for scoped in [false, true] {
        for key in ["", "ctrl-o", "ctrl-z", "cancel"] {
            fs::write(
                &picker,
                format!(
                    r#"#!/bin/sh
printf '%s\n' "$@" > "$HOME/picker-args"
cat > "$HOME/picker-input"
[ '{key}' != cancel ] || exit 130
printf '%s\n' '{key}'
head -n 1 "$HOME/picker-input"
"#
                ),
            )
            .unwrap();
            fs::set_permissions(&picker, fs::Permissions::from_mode(0o755)).unwrap();
            let args = if scoped {
                vec!["exec", "menu-worker", "--", env!("CARGO_BIN_EXE_shoal")]
            } else {
                vec![]
            };
            fs::remove_file(fixture.root.path().join("picker-input")).ok();
            let (output, transcript) = fixture.interactive(&args, "");
            assert!(
                fixture.root.path().join("picker-input").exists(),
                "picker did not run: {transcript}"
            );
            let args = fs::read_to_string(fixture.root.path().join("picker-args")).unwrap();
            let input = fs::read_to_string(fixture.root.path().join("picker-input")).unwrap();
            assert!(input.contains(workspace["id"].as_str().unwrap()));
            assert_eq!(input.contains("+ Add workspace"), !scoped);
            let (keys, header) = if scoped {
                (
                    "ctrl-e,ctrl-o,ctrl-f",
                    "enter: enter   ctrl-e: execute   ctrl-o: inspect   ctrl-f: diff",
                )
            } else {
                (
                    "ctrl-d,ctrl-e,ctrl-a,ctrl-o,ctrl-s,ctrl-f",
                    "enter: enter   ctrl-d: delete   ctrl-e: execute   ctrl-a: add   ctrl-o: inspect   ctrl-s: stop   ctrl-f: diff",
                )
            };
            assert!(args.contains(&format!("--expect={keys}\n--header\n{header}\n")));
            match key {
                "cancel" => {
                    assert!(!output.status.success());
                    assert!(transcript.contains("selection canceled"), "{transcript}");
                }
                "ctrl-z" => {
                    assert!(!output.status.success());
                    assert!(transcript.contains("unknown picker action"), "{transcript}");
                }
                _ => {
                    assert!(output.status.success(), "{transcript}");
                    assert!(String::from_utf8_lossy(&output.stdout).contains("menu-worker"));
                }
            }
        }
    }
}

#[test]
fn cd_always_picks_even_inside_a_workspace_and_cancel_does_not_navigate() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    let second = fixture.add("second");
    let mut broken = fixture.command();
    let failed = broken
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "missing",
            "--base",
            "not-a-ref",
        ])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fzf = bin.join("fzf");
    fs::write(&fzf, "#!/bin/sh\ncat > \"$SHOAL_TEST_PICK_INPUT\"\n[ \"$SHOAL_TEST_PICK_ID\" != cancel ] || exit 130\nawk -F '\\t' -v id=\"$SHOAL_TEST_PICK_ID\" '$1 == id { print }' \"$SHOAL_TEST_PICK_INPUT\"\n").unwrap();
    fs::set_permissions(&fzf, fs::Permissions::from_mode(0o755)).unwrap();
    let directive = fixture.root.path().join("cd-directive");
    let input = fixture.root.path().join("picker-input");
    for choice in [second["id"].as_str().unwrap(), "cancel"] {
        fs::write(&directive, "").unwrap();
        let (_master, slave) = pty::open();
        let output = fixture
            .command()
            .current_dir(first["path"].as_str().unwrap())
            .env("SHOAL_TEST_PICK_INPUT", &input)
            .env("SHOAL_TEST_PICK_ID", choice)
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .arg("cd")
            .stdin(slave.try_clone().unwrap())
            .stderr(slave)
            .output()
            .unwrap();
        let choices = fs::read_to_string(&input).unwrap();
        assert!(choices.contains(first["id"].as_str().unwrap()));
        assert!(choices.contains(second["id"].as_str().unwrap()));
        assert!(!choices.contains("missing"));
        if choice == "cancel" {
            assert!(!output.status.success());
            assert_eq!(fs::read_to_string(&directive).unwrap(), "");
        } else {
            assert!(output.status.success());
            assert_eq!(
                fs::read_to_string(&directive).unwrap().trim(),
                second["path"].as_str().unwrap()
            );
        }
    }
    // JSON/piped invocations must not silently choose the current workspace.
    let output = fixture
        .command()
        .current_dir(first["path"].as_str().unwrap())
        .args(["--json", "cd"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("explicit target"));
}

#[test]
fn doctor_daemon_reports_untracked_worktrees_without_adopting_them() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixStream,
    };
    let fixture = Fixture::new();
    let owned = fixture.add("owned");
    let owned_path = Path::new(owned["path"].as_str().unwrap());
    let orphan = owned_path.parent().unwrap().join("nested/orphan");
    let outside = fixture.root.path().join("outside");
    for (path, branch) in [(&orphan, "orphan"), (&outside, "outside")] {
        git(
            &fixture.repo,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
    }
    fs::write(orphan.join("dirty"), "keep me").unwrap();
    let call = |request: Value| {
        let mut socket =
            UnixStream::connect(fixture.root.path().join("state/daemon.sock")).unwrap();
        writeln!(socket, "{request}").unwrap();
        let mut line = String::new();
        BufReader::new(socket).read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let protocol =
        call(serde_json::json!({"protocol":0,"id":1,"method":"status"}))["protocol"].clone();
    let response = call(serde_json::json!({"protocol":protocol,"id":2,"method":"diagnose"}));
    assert_eq!(response["type"], "diagnostics");
    let findings: Vec<_> = response["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["name"].as_str().unwrap().starts_with("worktrees:"))
        .collect();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["status"], "warning");
    assert!(
        findings[0]["message"]
            .as_str()
            .unwrap()
            .contains(orphan.to_str().unwrap())
    );
    assert_eq!(fs::read_to_string(orphan.join("dirty")).unwrap(), "keep me");
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

fn repaired_workspaces(fixture: &Fixture, args: &[&str]) -> Value {
    let reports = recovery_report(fixture, args);
    assert!(
        reports
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["issues"].as_array().unwrap().is_empty()),
        "{reports}"
    );
    reports
}

fn recovery_report(fixture: &Fixture, args: &[&str]) -> Value {
    let output = fixture.command().arg("--json").args(args).output().unwrap();
    assert!(
        matches!(output.status.code(), Some(0 | 2)),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["workspaces"].take()
}

#[test]
fn daemon_startup_quarantines_operations_and_retains_claims_across_restarts() {
    let mut fixture = Fixture::with_tools(Some("[resources.lock]\n"), true);
    let states = [
        "preparing",
        "removing",
        "stopping",
        "reconciling",
        "ready",
        "failed",
    ];
    let workspaces: Vec<_> = states.iter().map(|name| fixture.add(name)).collect();
    let port = fixture.ok(&["port", "acquire", "web", "preparing"]);
    let resource = fixture.ok(&["resource", "acquire", "lock", "preparing"]);
    let owner = workspaces[0]["id"].as_str().unwrap();
    let simulator = serde_json::json!({
        "id": "simulator", "udid": null, "device": "type.Phone", "runtime": "runtime.iOS",
        "workspace_id": owner, "last_workspace_id": owner, "lease_name": "default",
        "reason": null, "state": "creating", "last_used": 1, "error": null
    });
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    for (state, workspace) in states.iter().zip(&workspaces) {
        db.execute(
            "UPDATE workspaces SET state=?1,error='original error' WHERE id=?2",
            rusqlite::params![state, workspace["id"].as_str().unwrap()],
        )
        .unwrap();
    }
    db.execute("INSERT INTO executions(id,workspace_id,state) VALUES ('execution',?1,'running'),('legacy',?1,'unknown')", [owner]).unwrap();
    db.execute(
        "INSERT INTO simulators(id,record) VALUES ('simulator',?1)",
        [simulator.to_string()],
    )
    .unwrap();
    for status in ["requested", "acquired", "busy", "failed", "interrupted"] {
        db.execute("INSERT INTO simulator_clean_requests(request_id,workspace_id,record) VALUES (?1,?2,?3)",
            rusqlite::params![status, owner, serde_json::json!({"status": status, "reason": "preserve audit", "evicted": ["recorded-device"]}).to_string()]).unwrap();
    }
    for _ in 0..2 {
        fixture.restart();
        for (state, workspace) in states.iter().zip(&workspaces) {
            let inspection = fixture.ok(&["inspect", state]);
            let saved = &inspection["workspace"];
            assert_eq!(
                saved["state"],
                if *state == "ready" { "ready" } else { "failed" }
            );
            assert_eq!(saved["git_dir"], workspace["git_dir"]);
            assert_eq!(saved["git_dir_id"], workspace["git_dir_id"]);
            let expected_error = if ["ready", "failed"].contains(state) {
                "original error"
            } else {
                "daemon stopped during workspace operation; inspect before cleanup"
            };
            assert_eq!(saved["error"], expected_error);
            assert!(
                Path::new(saved["path"].as_str().unwrap())
                    .join("tracked")
                    .exists()
            );
            if *state == "preparing" {
                let executions = inspection["executions"].as_array().unwrap();
                assert_eq!(executions.len(), 2);
                assert!(
                    executions
                        .iter()
                        .all(|execution| execution["state"] == "unknown")
                );
            }
        }
        assert_eq!(fixture.ok(&["port", "preparing"])["reserved"][0], port);
        assert_eq!(
            fixture.ok(&["resource", "preparing"])["leases"][0],
            resource
        );
        let saved: String = db
            .query_row("SELECT record FROM simulators", [], |r| r.get(0))
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), simulator);
        let audits = db
            .prepare("SELECT request_id,record FROM simulator_clean_requests")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(audits.len(), 5);
        for (status, record) in audits {
            let expected = if status == "requested" {
                "interrupted"
            } else {
                &status
            };
            assert_eq!(
                serde_json::from_str::<Value>(&record).unwrap(),
                serde_json::json!({
                    "status": expected, "reason": "preserve audit", "evicted": ["recorded-device"]
                })
            );
        }
    }
}

#[test]
fn doctor_repairs_interrupted_state_and_preserves_work_and_leases() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
    let workspace = fixture.add("interrupted");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("uncommitted"), "preserve me").unwrap();
    let port = fixture.ok(&["port", "acquire", "web", "interrupted"]);
    let resource = fixture.ok(&["resource", "acquire", "lock", "interrupted"]);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "UPDATE workspaces SET state='removing' WHERE name='interrupted'",
        [],
    )
    .unwrap();
    fixture.restart();
    assert_eq!(
        fixture.ok(&["inspect", "interrupted"])["workspace"]["state"],
        "failed"
    );
    let preview = recovery_report(&fixture, &["doctor", "interrupted"]);
    assert_eq!(preview[0]["directory"], "valid");
    let human = fixture.run(&["doctor", "interrupted"]);
    assert!(String::from_utf8_lossy(&human.stdout).contains("interrupted: failed (valid)"));
    let issue = preview[0]["issues"][0].as_str().unwrap();
    assert!(issue.starts_with(preview[0]["workspace"]["error"].as_str().unwrap()));
    assert!(issue.contains("--repair"));
    assert_eq!(
        fixture.ok(&["inspect", "interrupted"])["workspace"]["state"],
        "failed"
    );
    db.execute(
        "UPDATE workspaces SET error=NULL WHERE name='interrupted'",
        [],
    )
    .unwrap();
    let preview = recovery_report(&fixture, &["doctor", "interrupted"]);
    assert!(
        preview[0]["issues"][0]
            .as_str()
            .unwrap()
            .contains("--repair")
    );
    let repaired = repaired_workspaces(&fixture, &["doctor", "interrupted", "--repair"]);
    assert_eq!(repaired[0]["workspace"]["state"], "ready");
    assert_eq!(
        fs::read_to_string(path.join("uncommitted")).unwrap(),
        "preserve me"
    );
    assert_eq!(fixture.ok(&["port", "interrupted"])["reserved"][0], port);
    assert_eq!(
        fixture.ok(&["resource", "interrupted"])["leases"][0],
        resource
    );
    assert!(
        repaired_workspaces(&fixture, &["doctor", "interrupted", "--repair"])[0]["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !fixture
            .run(&[
                "exec",
                "interrupted",
                "--",
                env!("CARGO_BIN_EXE_shoal"),
                "doctor",
                "--all",
                "--repair"
            ])
            .status
            .success()
    );
}

#[test]
fn doctor_detects_moved_and_replaced_worktrees_without_deleting_data() {
    let fixture = Fixture::new();
    let workspace = fixture.add("original");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("dirty"), "saved").unwrap();
    let moved = fixture.root.path().join("moved workspace");
    git(
        &fixture.repo,
        &[
            "worktree",
            "move",
            path.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    let report = recovery_report(&fixture, &["doctor", "original", "--repair"]);
    assert_eq!(report[0]["directory"], "moved");
    let human = fixture.run(&["doctor", "original"]);
    assert!(String::from_utf8_lossy(&human.stdout).contains("original: failed (moved)"));
    let preview = recovery_report(&fixture, &["doctor", "original"]);
    assert_eq!(preview[0]["issues"], report[0]["issues"]);
    assert_eq!(preview[0]["issues"].as_array().unwrap().len(), 1);
    assert!(!fixture.run(&["rm", "original"]).status.success());
    assert_eq!(fs::read_to_string(moved.join("dirty")).unwrap(), "saved");
    git(
        &fixture.repo,
        &[
            "worktree",
            "move",
            moved.to_str().unwrap(),
            path.to_str().unwrap(),
        ],
    );
    repaired_workspaces(&fixture, &["doctor", "original", "--repair"]);
    // Replace the admin directory at its SAME path, proving pathname checks alone are insufficient.
    let admin = Path::new(workspace["git_dir"].as_str().unwrap());
    let old = fixture.root.path().join("old-admin");
    fs::rename(admin, &old).unwrap();
    fs::create_dir(admin).unwrap();
    for entry in fs::read_dir(&old).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::copy(entry.path(), admin.join(entry.file_name())).unwrap();
        }
    }
    let report = recovery_report(&fixture, &["doctor", "original", "--repair"]);
    assert_eq!(report[0]["directory"], "unverified");
    assert!(
        report[0]["issues"]
            .to_string()
            .contains("metadata was replaced")
    );
    assert!(
        !fixture
            .run(&["rm", "original", "--yes", "--delete-branch"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["exec", "original", "--", "true"])
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(path.join("dirty")).unwrap(), "saved");
}

#[test]
fn deleted_worktrees_are_forgotten_with_their_resources_but_moved_ones_are_kept() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\ncapacity = 3\n"));
    let mut paths = Vec::new();
    for name in ["directory-only", "git-removed", "moved"] {
        let workspace = fixture.add(name);
        fixture.ok(&["port", "acquire", "web", name]);
        fixture.ok(&["resource", "acquire", "lock", name]);
        paths.push(PathBuf::from(workspace["path"].as_str().unwrap()));
    }
    fs::remove_dir_all(&paths[0]).unwrap();
    git(
        &fixture.repo,
        &["worktree", "remove", paths[1].to_str().unwrap()],
    );
    let elsewhere = fixture.root.path().join("elsewhere");
    git(
        &fixture.repo,
        &[
            "worktree",
            "move",
            paths[2].to_str().unwrap(),
            elsewhere.to_str().unwrap(),
        ],
    );
    // The startup sweep forgets deleted worktrees without touching moved ones.
    fixture.restart();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let names: Vec<String> = fixture
            .ok(&["list"])
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].as_str().unwrap().to_owned())
            .collect();
        if names == ["moved"] {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "deleted worktrees remain: {names:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fixture.ok(&["inspect", "moved"])["workspace"]["state"],
        "failed"
    );
    assert!(!fixture.run(&["rm", "moved"]).status.success());
    assert_eq!(
        fixture
            .ok(&["port", "list", "--all"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .ok(&["resource", "list", "--all"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let worktrees = git(&fixture.repo, &["worktree", "list", "--porcelain"]);
    for (name, path) in ["directory-only", "git-removed"].iter().zip(&paths) {
        assert!(!git(&fixture.repo, &["rev-parse", &format!("refs/heads/{name}")]).is_empty());
        assert!(!worktrees.contains(path.to_str().unwrap()));
    }
    // Explicit removal of a deleted worktree needs no reconciliation first.
    let workspace = fixture.add("explicit");
    fs::remove_dir_all(workspace["path"].as_str().unwrap()).unwrap();
    let result = fixture.ok(&["rm", "explicit"]);
    assert_eq!(result["branch_deleted"], false);
    assert!(!git(&fixture.repo, &["rev-parse", "refs/heads/explicit"]).is_empty());
}

fn wait_registered_execution(fixture: &Fixture, workspace: &str) -> Value {
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

#[test]
fn doctor_stops_identity_verified_orphans_after_wrapper_death() {
    let fixture = Fixture::new();
    fixture.add("orphan");
    let mut wrapper = fixture
        .command()
        .args(["exec", "orphan", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let execution = wait_registered_execution(&fixture, "orphan");
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.ok(&["inspect", "orphan"])["executions"][0]["state"] != "unknown" {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let report = recovery_report(&fixture, &["doctor", "orphan", "--repair"]);
    assert!(
        report[0]["executions"][0]["processes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["pid"] == execution["child"]["pid"])
    );
    assert_eq!(
        fixture.ok(&["inspect", "orphan"])["executions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    repaired_workspaces(
        &fixture,
        &[
            "doctor",
            "orphan",
            "--repair",
            "--stop",
            "--acknowledge-stopped",
        ],
    );
    assert_eq!(
        fixture.ok(&["inspect", "orphan"])["executions"],
        serde_json::json!([])
    );
    assert_eq!(
        fixture.ok(&["inspect", "orphan"])["workspace"]["state"],
        "ready"
    );
}

#[test]
fn doctor_recovers_daemon_crash_and_requires_acknowledgement_for_legacy_records() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("crash");
    let port = fixture.ok(&["port", "acquire", "web", "crash"]);
    let mut wrapper = fixture
        .command()
        .args(["exec", "crash", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "crash");
    fixture.restart();
    wrapper.wait().unwrap();
    let report = recovery_report(&fixture, &["doctor", "crash"]);
    assert_eq!(report[0]["executions"][0]["state"], "unknown");
    repaired_workspaces(
        &fixture,
        &["doctor", "crash", "--repair", "--acknowledge-stopped"],
    );
    assert_eq!(fixture.ok(&["port", "crash"])["reserved"][0], port);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO executions(id,workspace_id,state) VALUES ('legacy',?1,'unknown')",
        [workspace["id"].as_str().unwrap()],
    )
    .unwrap();
    let report = recovery_report(&fixture, &["doctor", "crash", "--repair"]);
    assert!(!report[0]["executions"][0]["cleared"].as_bool().unwrap());
    repaired_workspaces(
        &fixture,
        &["doctor", "crash", "--repair", "--acknowledge-stopped"],
    );
    assert_eq!(
        fixture.ok(&["inspect", "crash"])["executions"],
        serde_json::json!([])
    );
}

#[test]
fn doctor_finds_detached_tagged_children_even_after_the_command_exits() {
    let fixture = Fixture::new();
    fixture.add("detached");
    let script = "import os, subprocess, sys; subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], start_new_session=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)";
    let output = fixture.run(&["exec", "detached", "--", "python3", "-c", script]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("surviving or unverified"));
    let report = recovery_report(&fixture, &["doctor", "detached"]);
    assert!(
        !report[0]["executions"][0]["processes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    repaired_workspaces(
        &fixture,
        &[
            "doctor",
            "detached",
            "--repair",
            "--stop",
            "--acknowledge-stopped",
        ],
    );
    assert_eq!(
        fixture.ok(&["inspect", "detached"])["executions"],
        serde_json::json!([])
    );
}

#[test]
fn manual_removal_stops_recorded_orphans_before_releasing_resources() {
    let fixture = Fixture::with_config(Some("[resources.lock]\n"));
    fixture.add("orphan");
    fixture.ok(&["resource", "acquire", "lock", "orphan"]);
    let mut wrapper = fixture
        .command()
        .args(["exec", "orphan", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let execution = wait_registered_execution(&fixture, "orphan");
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.ok(&["inspect", "orphan"])["executions"][0]["state"] != "unknown" {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    fixture.ok(&["rm", "orphan"]);
    assert_eq!(
        fixture.ok(&["resource", "list", "--all"]),
        serde_json::json!([])
    );
    let pid = execution["child"]["pid"].as_u64().unwrap().to_string();
    let output = Command::new("ps")
        .args(["-p", &pid, "-o", "stat="])
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&output.stdout);
    assert!(
        status.trim().is_empty() || status.trim().starts_with('Z'),
        "owned child survived removal"
    );
}

#[test]
fn doctor_all_reports_each_workspace_independently() {
    let fixture = Fixture::new();
    fixture.add("healthy");
    let missing = fixture.add("missing");
    fs::remove_dir_all(missing["path"].as_str().unwrap()).unwrap();
    let reports = recovery_report(&fixture, &["doctor", "--all", "--repair"]);
    assert_eq!(reports.as_array().unwrap().len(), 2);
    assert_eq!(reports[0]["workspace"]["name"], "healthy");
    assert_eq!(reports[0]["workspace"]["state"], "ready");
    assert_eq!(reports[1]["workspace"]["state"], "failed");
}

#[test]
fn doctor_preserves_connected_commands_until_stop_is_explicit() {
    let fixture = Fixture::new();
    fixture.add("connected");
    let port = fixture.ok(&["port", "acquire", "web", "connected"]);
    let mut wrapper = fixture
        .command()
        .args(["exec", "connected", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "connected");
    let report = repaired_workspaces(&fixture, &["doctor", "connected", "--repair"]);
    assert_eq!(report[0]["executions"][0]["connected"], true);
    assert_eq!(report[0]["executions"][0]["cleared"], false);
    assert!(wrapper.try_wait().unwrap().is_none());
    repaired_workspaces(&fixture, &["doctor", "connected", "--repair", "--stop"]);
    wrapper.wait().unwrap();
    assert_eq!(
        fixture.ok(&["inspect", "connected"])["executions"],
        serde_json::json!([])
    );
    assert_eq!(fixture.ok(&["port", "connected"])["reserved"][0], port);
}

#[test]
fn stopping_disconnected_execution_does_not_hold_up_other_workspaces() {
    let fixture = Fixture::new();
    let orphan = fixture.add("orphan");
    fixture.add("other");
    let root = Path::new(orphan["path"].as_str().unwrap());
    let script = r#"
import pathlib, signal, sys, time
root = pathlib.Path.cwd()
def stopping(*_):
    (root / 'stopping').touch()
    while not (root / 'other-ran').exists():
        time.sleep(0.01)
    (root / 'saw-other').touch()
    sys.exit(0)
signal.signal(signal.SIGTERM, stopping)
(root / 'ready').touch()
time.sleep(30)
"#;
    let mut wrapper = fixture
        .command()
        .args(["exec", "orphan", "--", "python3", "-c", script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "orphan");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("ready").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    while fixture.ok(&["inspect", "orphan"])["executions"][0]["state"] != "unknown" {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let mut stopping = fixture
        .command()
        .args(["stop", "orphan"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    while !root.join("stopping").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let marker = root.join("other-ran");
    let output = fixture.run(&["exec", "other", "--", "touch", marker.to_str().unwrap()]);
    assert!(output.status.success());
    stopping.wait().unwrap(); // Recovery may still require acknowledgement of unreadable environments.
    assert!(
        root.join("saw-other").exists(),
        "other workspace was blocked until orphan was forcibly killed"
    );
}

#[test]
fn desktop_shortcuts_open_workspaces_without_cli_flags_or_execution_records() {
    let fixture = Fixture::new();
    let workspace = fixture.add("desktop");
    let path = workspace["path"].as_str().unwrap();
    let bin = fixture.root.path().join("desktop-bin");
    fs::create_dir(&bin).unwrap();
    for program in ["codex", "t3"] {
        let stub = bin.join(program);
        fs::write(&stub, "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\"\nexit 7\n").unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
        for explicit in [true, false] {
            let mut command = fixture.command();
            command.arg(program);
            if program == "codex" {
                command.arg("--app");
            }
            if explicit {
                command.arg("desktop");
            } else {
                command.current_dir(path);
            }
            let output = command
                .args(["--", "--example", "literal spaces; $(false)"])
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(7),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!(
                    "{}\napp\n{path}\n--example\nliteral spaces; $(false)\n",
                    fs::canonicalize(path).unwrap().display()
                )
            );
            assert_eq!(
                fixture.ok(&["inspect", "desktop"])["executions"],
                serde_json::json!([])
            );
        }
    }
    fixture.add("other");
    for args in [vec!["codex", "other", "--app"], vec!["t3", "other"]] {
        let output = fixture
            .command()
            .args(["exec", "desktop", "--", env!("CARGO_BIN_EXE_shoal")])
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("another worktree"));
    }
    assert!(!fixture.run(&["codex", "desktop"]).status.success());
}

fn merge_commit(path: &Path, file: &str, contents: &str) {
    fs::write(path.join(file), contents).unwrap();
    git(path, &["add", file]);
    git(
        path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            file,
        ],
    );
}

#[test]
fn merge_scoped_local_branch_only_changes_own_workspace() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let other = fixture.add("other");
    let worker_path = Path::new(worker["path"].as_str().unwrap());
    let other_path = Path::new(other["path"].as_str().unwrap());
    merge_commit(other_path, "incoming", "from another workspace\n");
    let expected = git(other_path, &["rev-parse", "HEAD"]);
    let main = git(&fixture.repo, &["rev-parse", "main"]);
    let binary = env!("CARGO_BIN_EXE_shoal");
    let denied = fixture.run(&["exec", "worker", "--", binary, "merge", "other", "other"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("cannot access another worktree"));
    let output = fixture.run(&["exec", "worker", "--", binary, "--json", "merge", "other"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["source_commit"], expected.trim());
    assert_eq!(result["success"], true);
    assert_eq!(git(worker_path, &["rev-parse", "HEAD"]), expected);
    assert_eq!(git(other_path, &["rev-parse", "HEAD"]), expected);
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), main);
    assert_eq!(
        git(worker_path, &["symbolic-ref", "--short", "HEAD"]),
        "worker\n"
    );
    assert!(
        fixture.ok(&["inspect", "worker"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn merge_refreshes_local_source_from_upstream_unless_local_or_owned_by_a_workspace() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    let stale = git(&fixture.repo, &["rev-parse", "main"]);
    let author = upstream_remote(&fixture);
    let upstream = git(&author, &["rev-parse", "HEAD"]);
    let binary = env!("CARGO_BIN_EXE_shoal");
    // A scoped agent merging main refreshes and uses the upstream state.
    let output = fixture.run(&["exec", "worker", "--", binary, "--json", "merge", "main"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["success"], true);
    assert_eq!(result["source_refresh"]["updated"], true);
    assert_eq!(result["source_refresh"]["previous_commit"], stale.trim());
    assert_eq!(result["source_commit"], upstream.trim());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), upstream);
    assert_eq!(git(path, &["rev-parse", "HEAD"]), upstream);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("upstream")).unwrap(),
        "from remote\n"
    );
    // --local merges the local branch as it is and leaves upstream alone.
    merge_commit(&author, "upstream", "second\n");
    git(&author, &["push", "origin", "main"]);
    let result = fixture.ok(&["merge", "main", "worker", "--local"]);
    assert!(result["source_refresh"].is_null());
    assert_eq!(result["source_commit"], upstream.trim());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), upstream);
    // A dirty main checkout blocks the refresh instead of merging stale work.
    fs::write(fixture.repo.join("scratch"), "dirty\n").unwrap();
    let output = fixture.run(&["merge", "main", "worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--local"), "{stderr}");
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), upstream);
    assert_eq!(git(path, &["rev-parse", "HEAD"]), upstream);
    fs::remove_file(fixture.repo.join("scratch")).unwrap();
    // Another workspace's branch is merged as it is, even with an upstream.
    let other = fixture.add("other");
    let other_path = Path::new(other["path"].as_str().unwrap());
    git(
        &fixture.repo,
        &["branch", "--set-upstream-to=origin/main", "other"],
    );
    merge_commit(other_path, "theirs", "other work\n");
    let theirs = git(other_path, &["rev-parse", "HEAD"]);
    let result = fixture.ok(&["merge", "other", "worker"]);
    assert_eq!(result["source_commit"], theirs.trim());
    assert!(
        result["source_refresh"]["skipped"]
            .as_str()
            .unwrap()
            .contains("workspace other")
    );
    assert_eq!(git(other_path, &["rev-parse", "HEAD"]), theirs);
    assert_eq!(git(&fixture.repo, &["for-each-ref", "refs/shoal/"]), "");
}

#[test]
fn merge_fetches_remote_only_branch_and_refreshes_qualified_sources() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    let author = upstream_remote(&fixture);
    git(&fixture.repo, &["remote", "rename", "origin", "source"]);
    git(&author, &["switch", "-c", "feature/remote-only"]);
    merge_commit(&author, "remote-only", "first\n");
    git(&author, &["push", "origin", "feature/remote-only"]);
    let before = git(&fixture.repo, &["rev-parse", "main"]);
    // This branch has never been fetched into a local or remote-tracking ref.
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "for-each-ref",
                "refs/remotes/source/feature/remote-only",
                "refs/heads/feature/remote-only"
            ]
        ),
        ""
    );
    let fetch_head = fixture.repo.join(".git/FETCH_HEAD");
    fs::write(&fetch_head, "unrelated fetch sentinel\n").unwrap();
    let binary = env!("CARGO_BIN_EXE_shoal");
    let output = fixture.run(&[
        "exec",
        "worker",
        "--",
        binary,
        "--json",
        "merge",
        "feature/remote-only",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        git(path, &["rev-parse", "HEAD"]),
        git(&author, &["rev-parse", "HEAD"])
    );
    merge_commit(&author, "remote-only", "latest\n");
    git(&author, &["push", "origin", "feature/remote-only"]);
    let result = fixture.ok(&["merge", "source/feature/remote-only", "worker"]);
    assert_eq!(result["success"], true);
    assert_eq!(
        fs::read_to_string(path.join("remote-only")).unwrap(),
        "latest\n"
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
    assert_eq!(
        fs::read_to_string(fetch_head).unwrap(),
        "unrelated fetch sentinel\n"
    );
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "for-each-ref",
                "refs/shoal/merge/",
                "refs/heads/feature/remote-only",
                "refs/remotes/source/feature/remote-only"
            ]
        ),
        ""
    );
    // A deleted remote branch must fail even if a stale tracking ref remains.
    git(
        &fixture.repo,
        &[
            "update-ref",
            "refs/remotes/source/feature/remote-only",
            "worker",
        ],
    );
    git(
        &author,
        &["push", "origin", "--delete", "feature/remote-only"],
    );
    assert!(
        !fixture
            .run(&["merge", "source/feature/remote-only", "worker"])
            .status
            .success()
    );
    assert_eq!(
        git(&fixture.repo, &["for-each-ref", "refs/shoal/merge/"]),
        ""
    );
}

#[test]
fn merge_remote_ambiguity_local_precedence_and_explicit_remote() {
    let fixture = Fixture::new();
    fixture.add("worker");
    let author = upstream_remote(&fixture);
    git(&author, &["switch", "-c", "topic"]);
    git(&author, &["push", "origin", "topic"]);
    let remote = fixture.root.path().join("origin.git");
    git(
        &fixture.repo,
        &["remote", "add", "second", remote.to_str().unwrap()],
    );
    let before = git(&fixture.repo, &["rev-parse", "worker"]);
    let output = fixture.run(&["merge", "topic", "worker"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("multiple remotes"));
    assert_eq!(git(&fixture.repo, &["rev-parse", "worker"]), before);
    git(&fixture.repo, &["branch", "topic", "main"]);
    assert_eq!(
        fixture.ok(&["merge", "topic", "worker"])["commit"],
        before.trim()
    );
    let result = fixture.ok(&["merge", "topic", "worker", "--remote", "second"]);
    assert_eq!(
        result["source_commit"],
        git(&author, &["rev-parse", "HEAD"]).trim()
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "topic"]), before);
    assert!(
        !fixture
            .run(&["merge", "topic", "worker", "--remote", "missing"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["merge", "missing-branch", "worker"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["merge", "topic:worker", "worker"])
            .status
            .success()
    );
    assert_eq!(
        git(&fixture.repo, &["for-each-ref", "refs/shoal/merge/"]),
        ""
    );
}

#[test]
fn merge_preserves_conflicts_and_refuses_changed_destination_branch() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let other = fixture.add("other");
    let path = Path::new(worker["path"].as_str().unwrap());
    let other_path = Path::new(other["path"].as_str().unwrap());
    git(&fixture.repo, &["config", "user.name", "Test"]);
    git(
        &fixture.repo,
        &["config", "user.email", "test@example.invalid"],
    );
    merge_commit(path, "tracked", "ours\n");
    merge_commit(other_path, "tracked", "theirs\n");
    let ours = git(path, &["rev-parse", "HEAD"]);
    let theirs = git(other_path, &["rev-parse", "HEAD"]);
    let output = fixture.run(&["--json", "merge", "other", "worker"]);
    assert_eq!(output.status.code(), Some(1));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["success"], false);
    assert!(result["stdout"].as_str().unwrap().contains("CONFLICT"));
    assert_eq!(git(path, &["rev-parse", "HEAD"]), ours);
    assert_eq!(git(path, &["rev-parse", "MERGE_HEAD"]), theirs);
    assert!(
        fs::read_to_string(path.join("tracked"))
            .unwrap()
            .contains("<<<<<<<")
    );
    git(path, &["merge", "--abort"]);
    git(path, &["switch", "-c", "unowned"]);
    let output = fixture.run(&["merge", "other", "worker"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("own recorded branch"));
    assert_eq!(git(path, &["rev-parse", "HEAD"]), ours);
    git(path, &["switch", "--detach"]);
    assert!(!fixture.run(&["merge", "other", "worker"]).status.success());
}

#[test]
fn merge_creates_merge_commit_and_preserves_uncommitted_edits() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let other = fixture.add("other");
    let path = Path::new(worker["path"].as_str().unwrap());
    let other_path = Path::new(other["path"].as_str().unwrap());
    git(&fixture.repo, &["config", "user.name", "Test"]);
    git(
        &fixture.repo,
        &["config", "user.email", "test@example.invalid"],
    );
    merge_commit(path, "ours", "ours\n");
    merge_commit(other_path, "tracked", "theirs\n");
    fs::write(path.join("tracked"), "uncommitted\n").unwrap();
    let before = git(path, &["rev-parse", "HEAD"]);
    assert!(!fixture.run(&["merge", "other", "worker"]).status.success());
    assert_eq!(
        fs::read_to_string(path.join("tracked")).unwrap(),
        "uncommitted\n"
    );
    assert_eq!(git(path, &["rev-parse", "HEAD"]), before);
    git(path, &["restore", "tracked"]);
    let result = fixture.ok(&["merge", "other", "worker"]);
    assert_eq!(result["success"], true);
    assert_eq!(git(path, &["rev-parse", "HEAD^1"]), before);
    assert_eq!(
        git(path, &["rev-parse", "HEAD^2"]),
        git(other_path, &["rev-parse", "HEAD"])
    );
    assert_eq!(fs::read_to_string(path.join("ours")).unwrap(), "ours\n");
}

#[test]
fn repository_removal_deletes_local_checkout_workspaces_and_leases_and_stops_commands() {
    let fixture = Fixture::with_config(Some("[resources.global-lock]\n"));
    let repo = fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "doomed",
    ]);
    let first = fixture.add("first");
    let second = fixture.add("second");
    let first_path = Path::new(first["path"].as_str().unwrap());
    fs::write(first_path.join("dirty"), "uncommitted work").unwrap();
    fs::write(first_path.join(".shoal.toml"), "[resources.local-lock]\n").unwrap();
    fs::write(fixture.repo.join("untracked"), "repo changes").unwrap();
    fixture.ok(&["port", "acquire", "web", "first"]);
    fixture.ok(&["resource", "acquire", "local-lock", "first"]);
    fixture.ok(&["resource", "acquire", "global-lock", "second"]);
    let declined = fixture.run(&["repo", "rm", "doomed"]);
    assert!(!declined.status.success());
    assert!(String::from_utf8_lossy(&declined.stderr).contains("pass --yes"));
    let scoped = fixture.run(&[
        "exec",
        "second",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "repo",
        "rm",
        "doomed",
        "--yes",
    ]);
    assert!(!scoped.status.success());
    assert!(String::from_utf8_lossy(&scoped.stderr).contains("workspace processes can only"));
    let mut command = fixture
        .command()
        .args(["exec", "first", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.ok(&["inspect", "first"])["executions"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let result = fixture.ok(&["repo", "rm", "doomed", "-y"]);
    assert_eq!(result["removed"], true);
    assert_eq!(result["repository_id"], repo["id"]);
    assert_eq!(result["workspaces_removed"], 2);
    assert!(!command.wait().unwrap().success());
    assert!(!fixture.repo.exists());
    assert!(!first_path.exists());
    assert!(!Path::new(second["path"].as_str().unwrap()).exists());
    assert!(fixture.ok(&["repo", "list"]).as_array().unwrap().is_empty());
    assert!(fixture.ok(&["list"]).as_array().unwrap().is_empty());
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    for table in [
        "ports",
        "resource_leases",
        "executions",
        "repository_removals",
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM resource_pools WHERE scope<>'global'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM resource_pools WHERE scope='global'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn repository_removal_preserves_external_worktrees_and_deletes_clone_on_retry() {
    let fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let repo = fixture.ok(&["repo", "add", &url, "--name", "cloned"]);
    let path = Path::new(repo["path"].as_str().unwrap());
    let external = fixture.root.path().join("external");
    git(
        path,
        &[
            "worktree",
            "add",
            "-b",
            "external",
            external.to_str().unwrap(),
        ],
    );
    let refused = fixture.run(&["repo", "rm", "cloned", "--yes"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("worktree outside Shoal"));
    assert!(external.exists() && path.exists());
    git(path, &["worktree", "remove", external.to_str().unwrap()]);
    fs::write(path.join("local-work"), "discarded explicitly").unwrap();
    fixture.ok(&["repo", "remove", &url, "--yes"]);
    assert!(!path.exists());
    assert!(fixture.repo.exists());
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
}

#[test]
fn repository_removal_ignores_only_missing_prunable_unlocked_worktrees() {
    let fixture = Fixture::new();
    let external = fixture.root.path().join("external with spaces");
    let target = fixture.repo.to_str().unwrap();
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "external",
            external.to_str().unwrap(),
        ],
    );
    git(
        &fixture.repo,
        &["worktree", "lock", external.to_str().unwrap()],
    );
    fs::remove_dir_all(&external).unwrap();
    let refused = fixture.run(&["repo", "rm", target, "--yes"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("worktree outside Shoal"));
    git(
        &fixture.repo,
        &["worktree", "unlock", external.to_str().unwrap()],
    );
    std::os::unix::fs::symlink(fixture.root.path().join("missing"), &external).unwrap();
    assert!(
        !fixture
            .run(&["repo", "rm", target, "--yes"])
            .status
            .success()
    );
    fs::remove_file(&external).unwrap();
    assert!(git(&fixture.repo, &["worktree", "list", "--porcelain"]).contains("prunable"));
    fixture.ok(&["repo", "rm", target, "--yes"]);
    assert!(!fixture.repo.exists());
    assert!(fixture.ok(&["repo", "list"]).as_array().unwrap().is_empty());
}

#[test]
fn removal_picker_keeps_or_deletes_the_selected_branch_and_can_cancel() {
    let fixture = Fixture::new();
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let picker = bin.join("fzf");
    for (name, label) in [
        ("cancel-choice", "Cancel"),
        ("keep-choice", "Keep branch"),
        ("delete-choice", "Delete branch"),
    ] {
        let workspace = fixture.add(name);
        let path = Path::new(workspace["path"].as_str().unwrap());
        fs::write(path.join("uncommitted"), "requires a branch choice").unwrap();
        fs::write(
            &picker,
            format!("#!/bin/sh\nawk -F '\\t' '$2 ~ /^{label} / {{ print }}'\n"),
        )
        .unwrap();
        fs::set_permissions(&picker, fs::Permissions::from_mode(0o755)).unwrap();
        let (output, transcript) = fixture.interactive(&["rm", name], "y\n");
        assert_eq!(output.status.success(), label != "Cancel", "{transcript}");
        assert_eq!(path.exists(), label == "Cancel");
        let branch = git(
            &fixture.repo,
            &["branch", "--list", workspace["branch"].as_str().unwrap()],
        );
        assert_eq!(!branch.trim().is_empty(), label != "Delete branch");
        if label == "Cancel" {
            assert!(transcript.contains("removal canceled"), "{transcript}");
            assert!(path.join("uncommitted").exists());
        } else {
            assert!(transcript.contains("?? uncommitted"), "{transcript}");
        }
    }
}

#[test]
fn interactive_removal_confirms_and_defaults_to_no_without_affecting_scripts() {
    let fixture = Fixture::new();
    fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    for flag in ["--keep-branch", "--delete-branch"] {
        let workspace = fixture.add("worker");
        let path = Path::new(workspace["path"].as_str().unwrap());
        fs::write(path.join("uncommitted"), "keep until approved").unwrap();
        let piped = fixture.run(&["rm", "worker", flag]);
        assert!(!piped.status.success());
        assert!(String::from_utf8_lossy(&piped.stderr).contains("pass --yes"));
        let (no, prompt) = fixture.interactive(&["rm", "worker", flag], "n\n");
        assert!(!no.status.success());
        assert!(prompt.contains("Are you sure? [y/N]"), "{prompt}");
        assert!(prompt.contains("?? uncommitted"), "{prompt}");
        assert!(path.join("uncommitted").exists());
        let (yes, prompt) = fixture.interactive(&["rm", "worker", flag], "maybe\ny\n");
        assert!(yes.status.success(), "{prompt}");
        assert!(prompt.contains("Please enter y or n."), "{prompt}");
        assert!(!path.exists());
        let branch_exists = git(
            &fixture.repo,
            &["branch", "--list", workspace["branch"].as_str().unwrap()],
        );
        assert_eq!(!branch_exists.trim().is_empty(), flag == "--keep-branch");
    }
    for answer in ["n\n", "\n", "\x04"] {
        let (no, prompt) = fixture.interactive(&["repo", "rm", "project"], answer);
        assert!(!no.status.success(), "{prompt}");
        assert!(prompt.contains("Are you sure? [y/N]"), "{prompt}");
        assert!(fixture.repo.exists());
    }
    let (json, prompt) = fixture.interactive(&["--json", "repo", "rm", "project"], "y\n");
    assert!(!json.status.success());
    assert!(!prompt.contains("Are you sure?"));
    assert!(fixture.repo.exists());
    let (yes, prompt) = fixture.interactive(&["repo", "rm", "project"], "Y\n");
    assert!(yes.status.success(), "{prompt}");
    assert!(!fixture.repo.exists());
}

#[test]
fn removal_confirmation_lists_git_changes_before_asking() {
    let fixture = Fixture::new();
    let workspace = fixture.add("preview");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let (no, prompt) = fixture.interactive(&["rm", "preview", "--keep-branch"], "n\n");
    assert!(!no.status.success());
    assert!(!prompt.contains("(Git status)"), "{prompt}");

    for name in ["modified", "deleted"] {
        fs::write(path.join(name), "original\n").unwrap();
    }
    git(path, &["add", "."]);
    git(
        path,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "-m",
            "Files for removal preview",
        ],
    );
    git(path, &["mv", "tracked", "renamed file"]);
    fs::write(path.join("modified"), "changed\n").unwrap();
    fs::remove_file(path.join("deleted")).unwrap();
    fs::write(path.join("added"), "staged\n").unwrap();
    git(path, &["add", "added"]);
    fs::create_dir(path.join("new directory")).unwrap();
    fs::write(path.join("new directory/child"), "untracked\n").unwrap();
    fs::write(path.join("line\nbreak"), "untracked\n").unwrap();
    fs::create_dir(path.join("ignored")).unwrap();
    fs::write(path.join("ignored/cache"), "ignored\n").unwrap();
    git(path, &["config", "status.relativePaths", "true"]);
    git(path, &["config", "status.showUntrackedFiles", "no"]);

    let (no, prompt) = fixture.interactive(&["rm", "preview", "--delete-branch"], "n\n");
    assert!(!no.status.success(), "{prompt}");
    let confirmation = prompt.find("Are you sure? [y/N]").unwrap();
    for entry in [
        "A  added",
        " D deleted",
        " M modified",
        "R  tracked -> \"renamed file\"",
        "?? \"new directory/child\"",
        "?? \"line\\nbreak\"",
    ] {
        let position = prompt.find(entry).unwrap_or_else(|| panic!("{prompt}"));
        assert!(position < confirmation, "{prompt}");
    }
    assert!(!prompt.contains("ignored/cache"), "{prompt}");
    assert_eq!(
        fs::read_to_string(path.join("modified")).unwrap(),
        "changed\n"
    );
    assert!(path.join("new directory/child").exists());
    let removed = fixture.run(&["rm", "preview", "--keep-branch", "--yes"]);
    assert!(removed.status.success());
    assert!(!String::from_utf8_lossy(&removed.stderr).contains("(Git status)"));
    assert!(!path.exists());
}

#[test]
fn removal_preview_limits_large_untracked_directories() {
    let fixture = Fixture::new();
    let workspace = fixture.add("large-preview");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::create_dir(path.join("untracked")).unwrap();
    for index in 0..500 {
        fs::write(
            path.join(format!("untracked/{index:03}-{}", "x".repeat(140))),
            "keep until approved",
        )
        .unwrap();
    }
    let (no, prompt) = fixture.interactive(&["rm", "large-preview", "--keep-branch"], "n\n");
    assert!(!no.status.success(), "{prompt}");
    assert_eq!(prompt.matches("?? untracked/").count(), 50, "{prompt}");
    assert!(prompt.contains("450 more entries omitted"), "{prompt}");
    assert!(prompt.contains("Are you sure? [y/N]"), "{prompt}");
    assert_eq!(fs::read_dir(path.join("untracked")).unwrap().count(), 500);
    let piped = fixture.run(&["rm", "large-preview", "--keep-branch"]);
    assert!(!piped.status.success());
    let error = String::from_utf8_lossy(&piped.stderr);
    assert!(error.contains("pass --yes"), "{error}");
    assert!(!error.contains("?? untracked/"), "{error}");
    fixture.ok(&["rm", "large-preview", "--keep-branch", "--yes"]);
    assert!(!path.exists());
}

#[test]
fn repository_removal_retries_partial_file_deletion_after_restart_but_rejects_replacement() {
    use std::os::unix::fs::MetadataExt;
    let mut fixture = Fixture::new();
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let metadata = fs::metadata(&fixture.repo).unwrap();
    let identity = format!("{}:{}", metadata.dev(), metadata.ino());
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute("INSERT INTO repository_removals(repository_id,directory_id,deleting_files) VALUES (?1,?2,1)", rusqlite::params![id, identity]).unwrap();
    let refused = fixture.run(&["add", id, "too-late"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("removal is incomplete"));
    fs::remove_dir_all(fixture.repo.join(".git")).unwrap();
    fixture.restart();
    let saved = fixture.root.path().join("original-directory");
    fs::rename(&fixture.repo, &saved).unwrap();
    fs::create_dir(&fixture.repo).unwrap();
    fs::write(fixture.repo.join("keep"), "replacement").unwrap();
    let refused = fixture.run(&["repo", "rm", id, "--yes"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("was replaced"));
    assert_eq!(
        fs::read_to_string(fixture.repo.join("keep")).unwrap(),
        "replacement"
    );
    fs::remove_dir_all(&fixture.repo).unwrap();
    fs::rename(saved, &fixture.repo).unwrap();
    fixture.ok(&["repo", "rm", id, "--yes"]);
    assert!(!fixture.repo.exists());
    assert!(fixture.ok(&["repo", "list"]).as_array().unwrap().is_empty());
}

#[test]
fn repository_removal_rejects_symlinks_and_nested_registered_repositories() {
    let fixture = Fixture::new();
    let id = fixture.ok(&["repo", "list"])[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let saved = fixture.root.path().join("original");
    fs::rename(&fixture.repo, &saved).unwrap();
    std::os::unix::fs::symlink(&saved, &fixture.repo).unwrap();
    let refused = fixture.run(&["repo", "rm", &id, "--yes"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("symlink"));
    assert!(saved.join("tracked").exists());
    fs::remove_file(&fixture.repo).unwrap();
    fs::rename(saved, &fixture.repo).unwrap();
    let nested = fixture.repo.join("nested");
    fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-b", "main"]);
    fixture.ok(&["repo", "add", nested.to_str().unwrap()]);
    let refused = fixture.run(&["repo", "rm", &id, "--yes"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("another registered repository"));
    assert!(fixture.repo.exists() && nested.exists());
}

#[test]
fn repository_removal_serializes_with_workspace_creation() {
    let fixture = Fixture::new();
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap().to_owned();
    let add = fixture
        .command()
        .args(["add", &id, "racing"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    fixture.ok(&["repo", "rm", &id, "--yes"]);
    let _ = add.wait_with_output().unwrap();
    assert!(fixture.ok(&["repo", "list"]).as_array().unwrap().is_empty());
    assert!(fixture.ok(&["list"]).as_array().unwrap().is_empty());
    // The emptied repository directory goes with the registration.
    assert!(!Path::new(repo["workspaces_dir"].as_str().unwrap()).exists());
    assert!(!fixture.repo.exists());
}

#[test]
#[cfg(target_os = "macos")]
fn repository_removal_preserves_resources_on_failure_and_retries_after_restart() {
    let config = format!("{SIM_CONFIG}\n[resources.lock]\n");
    let mut fixture = Fixture::with_tools(Some(&config), true);
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let input = fixture.root.path().join("local.toml");
    fs::write(&input, "[ports.web]\n").unwrap();
    let saved = fixture.ok(&["repo", "config", id, "--file", input.to_str().unwrap()]);
    let workspace = fixture.add("worker");
    let port = fixture.ok(&["port", "acquire", "web", "worker"]);
    let resource = fixture.ok(&["resource", "acquire", "lock", "worker"]);
    fixture.ok(&[
        "sim",
        "acquire",
        "worker",
        "--clean",
        "--reason",
        "repository removal test",
    ]);
    fs::write(fixture.root.path().join("sim-fail"), "delete").unwrap();
    assert!(!fixture.run(&["repo", "rm", id, "--yes"]).status.success());
    assert!(fixture.repo.exists());
    assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    assert_eq!(fixture.ok(&["port", "worker"])["reserved"][0], port);
    assert_eq!(fixture.ok(&["resource", "worker"])["leases"][0], resource);
    fixture.restart();
    assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    let blocked = fixture.run(&["repo", "config", id, "--clear"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("removal is incomplete"));
    assert!(!fixture.run(&["add", id, "blocked"]).status.success());
    fs::remove_file(fixture.root.path().join("sim-fail")).unwrap();
    fixture.ok(&["repo", "rm", id, "--yes"]);
    assert!(!fixture.repo.exists());
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM repository_configs", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        fixture.ok(&["sim", "--all"])["simulators"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("sim-devices.json")).unwrap(),
        "[]"
    );
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    assert!(
        db.query_row("SELECT COUNT(*) FROM simulator_clean_requests", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap()
            > 0
    );
}

#[test]
fn setup_cmd_resolves_worktree_paths_and_runs_before_agents() {
    let fixture = Fixture::new();
    let script = "setup 'literal' $name.sh";
    fs::write(
        fixture.repo.join(script),
        r#"#!/bin/sh
printf 'setup output\n'
printf '%s' "$PWD" > setup-cwd
printf '%s' "$SHOAL_WORKSPACE" > setup-workspace
"$SHOAL_TEST_BIN" --json status > during-setup.json || exit 91
"#,
    )
    .unwrap();
    fs::set_permissions(fixture.repo.join(script), fs::Permissions::from_mode(0o755)).unwrap();
    commit_resource_config(&fixture.repo, &format!("setup_cmd = {script:?}\n"));
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("codex"),
        "#!/bin/sh\ntest -f setup-cwd || exit 92\nprintf agent > agent-started\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
    let output = fixture
        .command()
        .args([
            "--json",
            "add",
            fixture.repo.to_str().unwrap(),
            "prepared",
            "--agent",
            "codex",
        ])
        .env("SHOAL_TEST_BIN", env!("CARGO_BIN_EXE_shoal"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(workspace["state"], "ready");
    assert!(String::from_utf8_lossy(&output.stderr).contains("setup output"));
    let path = Path::new(workspace["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(path.join("setup-cwd")).unwrap(),
        fs::canonicalize(path).unwrap().to_str().unwrap()
    );
    assert_eq!(
        fs::read_to_string(path.join("setup-workspace")).unwrap(),
        "prepared"
    );
    assert!(path.join("agent-started").exists());
    assert!(!fixture.repo.join("setup-cwd").exists());
    let during: Value =
        serde_json::from_slice(&fs::read(path.join("during-setup.json")).unwrap()).unwrap();
    assert_eq!(during["workspace"]["state"], "preparing");
    assert_eq!(during["setup_finished"], false);
    assert_eq!(during["executions"].as_array().unwrap().len(), 1);
}

#[test]
fn setup_cmd_local_override_absolute_path_failure_and_retry() {
    let fixture = Fixture::new();
    commit_resource_config(&fixture.repo, "setup_cmd = 'must-not-run'\n");
    let script = fixture.root.path().join("system setup.sh");
    fs::write(
        &script,
        "#!/bin/sh\nprintf partial > setup-result\nexit 17\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let input = fixture.root.path().join("local.toml");
    fs::write(
        &input,
        format!("setup_cmd = {:?}\n", script.to_str().unwrap()),
    )
    .unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        input.to_str().unwrap(),
    ]);
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("claude"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    let output = fixture.run(&[
        "--json",
        "add",
        fixture.repo.to_str().unwrap(),
        "failed-setup",
        "--agent",
        "claude",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("status 17"));
    let failed = fixture.ok(&["inspect", "failed-setup"]);
    assert_eq!(failed["workspace"]["state"], "failed");
    assert_eq!(failed["executions"], serde_json::json!([]));
    let report = recovery_report(&fixture, &["doctor", "failed-setup"]);
    assert_eq!(report[0]["directory"], "valid");
    let issues = report[0]["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1);
    let issue = issues[0].as_str().unwrap();
    assert!(issue.contains("setup failed (exit 17"));
    assert!(issue.contains("retry with shoal setup"));
    assert!(issue.contains("alternatively, use --repair"));
    let output = fixture.run(&["doctor", "failed-setup"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stdout).contains(issue));
    assert_eq!(
        fixture.ok(&["inspect", "failed-setup"])["workspace"],
        failed["workspace"]
    );
    assert_eq!(
        fixture.ok(&["status", "failed-setup"])["setup_finished"],
        false
    );
    let path = Path::new(failed["workspace"]["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(path.join("setup-result")).unwrap(),
        "partial"
    );
    assert!(!path.join("agent-started").exists());
    assert!(
        !fixture
            .run(&["exec", "failed-setup", "--", "true"])
            .status
            .success()
    );
    let output = fixture.run(&["prepare", "failed-setup"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown command \"prepare\""));
    fs::write(&script, "#!/bin/sh\nprintf complete > setup-result\n").unwrap();
    assert_eq!(fixture.ok(&["setup", "failed-setup"])["state"], "ready");
    assert_eq!(
        fixture.ok(&["status", "failed-setup"])["setup_finished"],
        true
    );
    assert_eq!(
        fs::read_to_string(path.join("setup-result")).unwrap(),
        "complete"
    );
}

#[test]
fn setup_failure_prompt_ignores_or_deletes_only_new_workspace() {
    let fixture = Fixture::new();
    fixture.add("existing");
    commit_resource_config(&fixture.repo, "setup_cmd = 'missing-script'\n");
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("codex"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
    let config = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        "[codex]\ndefault_mode = 'app'\n",
    )
    .unwrap();
    let (ignored, transcript) = fixture.interactive(
        &[
            "add",
            fixture.repo.to_str().unwrap(),
            "ignored",
            "--agent",
            "codex",
        ],
        "i\n",
    );
    assert!(ignored.status.success(), "{transcript}");
    assert!(transcript.contains("[d]elete workspace"));
    assert_eq!(
        fixture.ok(&["inspect", "ignored"])["workspace"]["state"],
        "ready"
    );
    assert_eq!(fixture.ok(&["status", "ignored"])["setup_finished"], false);
    let ignored_path = fixture.ok(&["inspect", "ignored"])["workspace"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(Path::new(&ignored_path).join("agent-started").exists());
    let (deleted, transcript) = fixture.interactive(
        &["add", fixture.repo.to_str().unwrap(), "deleted"],
        "d\ny\n",
    );
    assert!(!deleted.status.success(), "{transcript}");
    assert!(
        transcript.contains("Deleted workspace deleted"),
        "{transcript}"
    );
    assert!(!fixture.run(&["inspect", "deleted"]).status.success());
    assert!(
        git(&fixture.repo, &["branch", "--list", "deleted"])
            .trim()
            .is_empty()
    );
    assert!(fixture.repo.is_dir());
    assert_eq!(
        fixture.ok(&["inspect", "existing"])["workspace"]["state"],
        "ready"
    );
    let (canceled, transcript) =
        fixture.interactive(&["add", fixture.repo.to_str().unwrap(), "canceled"], "\n");
    assert!(!canceled.status.success(), "{transcript}");
    assert_eq!(
        fixture.ok(&["inspect", "canceled"])["workspace"]["state"],
        "failed"
    );
}

#[test]
fn setup_interruption_preserves_work_and_blocks_concurrent_execution() {
    let mut fixture = Fixture::new();
    fs::write(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\nprintf partial > setup-started\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::set_permissions(
        fixture.repo.join("setup.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_resource_config(&fixture.repo, "setup_cmd = 'setup.sh'\n");
    let mut add = fixture
        .command()
        .args([
            "--json",
            "add",
            fixture.repo.to_str().unwrap(),
            "interrupted",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let path = fixture
        .shoal_dir()
        .join("repo-with---quotes----literal/interrupted");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.join("setup-started").exists() {
        assert!(Instant::now() < deadline, "setup did not start");
        assert!(add.try_wait().unwrap().is_none());
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fixture.ok(&["inspect", "interrupted"])["workspace"]["state"],
        "preparing"
    );
    assert!(
        !fixture
            .run(&["exec", "interrupted", "--", "true"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["--json", "setup", "interrupted"])
            .status
            .success()
    );
    // Signal the wrapper so it can stop its recorded process group and report failure.
    assert_eq!(unsafe { libc::kill(add.id() as i32, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = add.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        assert!(Instant::now() < deadline, "setup did not stop");
        thread::sleep(Duration::from_millis(20));
    }
    let inspection = fixture.ok(&["inspect", "interrupted"]);
    assert_eq!(inspection["workspace"]["state"], "failed");
    assert_eq!(inspection["executions"], serde_json::json!([]));
    fixture.restart();
    assert_eq!(
        fixture.ok(&["inspect", "interrupted"])["workspace"]["state"],
        "failed"
    );
    assert!(path.join("setup-started").exists());
    fixture.ok(&["rm", "interrupted", "--yes", "--delete-branch"]);
    assert!(!path.exists());
}

#[test]
fn lifecycle_hooks_run_untracked_after_setup_and_before_removal() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let scripts = [
        ("setup.sh", "#!/bin/sh\nprintf setup > setup-done\n"),
        (
            "post-setup.sh",
            r#"#!/bin/sh
printf 'post-setup output\n'
test -z "$SHOAL_SCOPE_TOKEN" || exit 81
test -z "$SHOAL_EXECUTION_ID" || exit 82
test -z "$SHOAL_SHELL_DIRECTIVE" || exit 83
test "$SHOAL_HOOK" = post_setup || exit 84
test -f setup-done || exit 85
test -z "$SHOAL_TEST_FAIL_HOOK" || exit 5
printf '%s\n%s\n' "$PWD" "$SHOAL_WORKSPACE" > "$HOME/post-setup-ran"
sleep 30 < /dev/null > /dev/null 2>&1 &
"#,
        ),
        (
            "pre-remove.sh",
            r#"#!/bin/sh
test "$SHOAL_HOOK" = pre_remove || exit 86
test -z "$SHOAL_SCOPE_TOKEN" || exit 87
printf '%s\n%s\n' "$PWD" "$SHOAL_WORKSPACE" > "$HOME/pre-remove-ran"
test ! -f fail-removal || { echo 'session still busy' >&2; exit 3; }
"#,
        ),
    ];
    for (name, body) in scripts {
        fs::write(fixture.repo.join(name), body).unwrap();
        fs::set_permissions(fixture.repo.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    commit_resource_config(
        &fixture.repo,
        "setup_cmd = 'setup.sh'\npost_setup_cmd = 'post-setup.sh'\npre_remove_cmd = 'pre-remove.sh'\n",
    );
    let output = fixture
        .command()
        .args(["--json", "add", fixture.repo.to_str().unwrap(), "hooked"])
        .env("SHOAL_SHELL_DIRECTIVE", home.join("directive"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(workspace["state"], "ready");
    assert!(String::from_utf8_lossy(&output.stderr).contains("post-setup output"));
    let path = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
    assert_eq!(
        fs::read_to_string(home.join("post-setup-ran")).unwrap(),
        format!("{}\nhooked\n", path.display())
    );
    let inspection = fixture.ok(&["inspect", "hooked"]);
    assert_eq!(
        inspection["executions"],
        serde_json::json!([]),
        "hooks and what they leave behind are not tracked executions"
    );

    // A failing hook keeps the ready workspace and does not start the agent.
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("codex"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
    let output = fixture
        .command()
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "hook-fails",
            "--agent",
            "codex",
        ])
        .env("SHOAL_TEST_FAIL_HOOK", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("post_setup_cmd exited with 5"));
    let failed = fixture.ok(&["inspect", "hook-fails"]);
    assert_eq!(failed["workspace"]["state"], "ready");
    let failed_path = Path::new(failed["workspace"]["path"].as_str().unwrap());
    assert!(!failed_path.join("agent-started").exists());
    assert_eq!(fixture.ok(&["setup", "hook-fails"])["state"], "ready");
    assert!(
        fs::read_to_string(home.join("post-setup-ran"))
            .unwrap()
            .ends_with("\nhook-fails\n")
    );

    // The removal hook runs in the daemon before the worktree goes; failure retains it.
    fs::write(path.join("fail-removal"), "busy").unwrap();
    let output = fixture.run(&["rm", "hooked", "--yes", "--delete-branch"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("pre_remove_cmd exited with 3"), "{stderr}");
    assert!(stderr.contains("session still busy"), "{stderr}");
    assert_eq!(
        fs::read_to_string(home.join("pre-remove-ran")).unwrap(),
        format!("{}\nhooked\n", path.display())
    );
    let retained = fixture.ok(&["inspect", "hooked"]);
    assert_eq!(retained["workspace"]["state"], "ready");
    assert!(path.join("tracked").exists());
    fs::remove_file(path.join("fail-removal")).unwrap();
    fixture.ok(&["rm", "hooked", "--yes", "--delete-branch"]);
    assert!(!path.exists());
    assert!(
        !fixture
            .ok(&["list"])
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["name"] == "hooked")
    );
}

#[test]
fn claude_launch_marks_the_workspace_trusted_in_claude_config() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let workspace = fixture.add("trusted");
    let path = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
    let key = path.to_str().unwrap();
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("claude"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    let config = home.join(".claude.json");

    // First launch creates a trusted entry even before Claude has a config.
    assert!(fixture.run(&["claude", "trusted"]).status.success());
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(root["projects"][key]["hasTrustDialogAccepted"], true);

    fs::write(&config, r#"{"numStartups": 1, "projects": {}}"#).unwrap();
    assert!(fixture.run(&["claude", "trusted"]).status.success());
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(root["numStartups"], 1);
    assert_eq!(root["projects"][key]["hasTrustDialogAccepted"], true);

    // An absolute CLAUDE_CONFIG_DIR selects that directory's config instead.
    let config_dir = home.join("claude-config");
    fs::write(&config, r#"{"projects": {}}"#).unwrap();
    let output = fixture
        .command()
        .args(["claude", "trusted"])
        .env("CLAUDE_CONFIG_DIR", &config_dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let overridden: Value =
        serde_json::from_str(&fs::read_to_string(config_dir.join(".claude.json")).unwrap())
            .unwrap();
    assert_eq!(overridden["projects"][key]["hasTrustDialogAccepted"], true);
    let untouched: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert!(untouched["projects"][key].is_null());

    // A bad override warns and still launches.
    let output = fixture
        .command()
        .args(["claude", "trusted"])
        .env("CLAUDE_CONFIG_DIR", "relative")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be an absolute path"));
}

#[test]
fn codex_launches_trust_the_workspace_in_the_selected_user_config() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let workspace = fixture.add("trusted");
    let path = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
    let key = path.to_str().unwrap();
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // Inspect the config from the child so trust must precede the launch.
    fs::write(
        bin.join("codex"),
        "#!/bin/sh\ncat \"${CODEX_HOME:-$HOME/.codex}/config.toml\"\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
    let config = home.join(".codex/config.toml");
    for mode in [None, Some("--cli"), Some("--app")] {
        for overridden in [false, true] {
            let config_dir = home.join("custom-codex");
            let target = if overridden {
                config_dir.join("config.toml")
            } else {
                config.clone()
            };
            let mut command = fixture.command();
            command.arg("codex");
            if let Some(mode) = mode {
                command.args([mode, "trusted"]);
            } else {
                command.current_dir(&path);
            }
            if overridden {
                command.env("CODEX_HOME", &config_dir);
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let root: toml::Value =
                toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
            assert_eq!(
                root["projects"][key]["trust_level"].as_str(),
                Some("trusted")
            );
            assert_eq!(fs::read(&target).unwrap(), output.stdout);
            if overridden {
                assert!(!config.exists());
            }
            fs::remove_file(target).unwrap();
        }
    }

    // Malformed settings remain untouched and only produce a warning.
    fs::write(&config, "projects = []\n").unwrap();
    let output = fixture.run(&["codex", "trusted", "--cli"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not mark"));
    assert_eq!(fs::read_to_string(config).unwrap(), "projects = []\n");
}

#[test]
fn add_from_issue_uses_existing_forge_cli_and_passes_context_to_agents() {
    let fixture = Fixture::with_config(Some("[codex]\ndefault_mode = 'app'\n"));
    let bin = fixture.root.path().join("issue-bin");
    fs::create_dir(&bin).unwrap();
    for tool in ["gh", "fj"] {
        let script = bin.join(tool);
        fs::write(&script, "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$ISSUE_ARGS\"\npwd > \"$ISSUE_CWD\"\ncat \"$ISSUE_RESPONSE\"\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for tool in ["codex", "claude"] {
        let script = bin.join(tool);
        fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$AGENT_ARGS\"\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let response = fixture.root.path().join("issue-response");
    let issue_args = fixture.root.path().join("issue-args");
    let issue_cwd = fixture.root.path().join("issue-cwd");
    let agent_args = fixture.root.path().join("agent-args");
    let title = "Fix API timeout; $(false)";
    let body = "Reproduce with two clients.\nKeep the connection alive.";
    for (index, (host, agent, as_url)) in [
        ("github.com", "codex", false),
        ("github.com", "claude", true),
        ("forge.example", "codex", true),
        ("forge.example", "claude", false),
    ]
    .into_iter()
    .enumerate()
    {
        let number = index + 34;
        let url = format!("https://{host}/team/project/issues/{number}");
        let remote = format!("git@{host}:team/project.git");
        if index == 0 {
            git(&fixture.repo, &["remote", "add", "origin", &remote]);
        } else {
            git(&fixture.repo, &["remote", "set-url", "origin", &remote]);
        }
        let text = if host == "github.com" {
            serde_json::json!({"number": number, "title": title, "body": body}).to_string()
        } else {
            format!(
                "\u{2068}{title}\u{2069} #\u{2068}{number}\u{2069}\"\nBy user — Open\n\n> {body}\n\n0 comments\n"
            )
        };
        fs::write(&response, text).unwrap();
        let input = if as_url {
            url.clone()
        } else {
            number.to_string()
        };
        let output = fixture
            .command()
            .args([
                "--json",
                "add",
                fixture.repo.to_str().unwrap(),
                "--base",
                "HEAD",
                "--issue",
                &input,
                "--agent",
                agent,
                "--",
                "--model",
                "test-model",
            ])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("ISSUE_RESPONSE", &response)
            .env("ISSUE_ARGS", &issue_args)
            .env("ISSUE_CWD", &issue_cwd)
            .env("AGENT_ARGS", &agent_args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
        let name = format!("issue-{number}-fix-api-timeout-false");
        assert_eq!(workspace["name"], name);
        assert_eq!(workspace["branch"], name);
        assert_eq!(
            fs::read_to_string(&issue_cwd).unwrap().trim(),
            fixture.repo.to_str().unwrap()
        );
        let invocation = fs::read_to_string(&issue_args).unwrap();
        assert!(invocation.contains(&format!("issue\0view\0{number}\0")));
        if host == "github.com" {
            assert!(invocation.contains("--repo\0github.com/team/project\0"));
        } else {
            assert!(invocation.contains("--host\0forge.example\0--remote\0origin\0"));
        }
        let invocation = fs::read_to_string(&agent_args).unwrap();
        let prompt = invocation.split('\0').next().unwrap();
        assert!(prompt.contains(title));
        assert!(prompt.contains(&url));
        assert!(prompt.contains(body));
        assert!(invocation.contains("\0--model\0test-model\0"));
        assert_eq!(
            fixture.ok(&["inspect", &name])["executions"],
            serde_json::json!([])
        );
    }
    // An explicit branch name still loads the issue, with no agent required.
    let output = fixture
        .command()
        .args([
            "--json",
            "add",
            fixture.repo.to_str().unwrap(),
            "custom-issue-name",
            "--base",
            "HEAD",
            "--issue",
            "37",
        ])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("ISSUE_RESPONSE", &response)
        .env("ISSUE_ARGS", &issue_args)
        .env("ISSUE_CWD", &issue_cwd)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["name"],
        "custom-issue-name"
    );
}

#[test]
fn issue_templates_resolve_saved_config_then_worktree_then_global() {
    let fixture = Fixture::new();
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("issue-template.md"),
        "global {number}: {title} {body}",
    )
    .unwrap();
    let bin = fixture.root.path().join("template-bin");
    fs::create_dir(&bin).unwrap();
    for (tool, script) in [
        (
            "gh",
            r#"#!/bin/sh
printf '%s' '{"number":44,"title":"Literal {body}","body":"$(false)"}'
"#,
        ),
        ("claude", "#!/bin/sh\nprintf '%s' \"$1\"\n"),
    ] {
        let path = bin.join(tool);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fixture.add_github_origin();
    for (index, expected) in [
        "global 44: Literal {body} $(false)",
        "repo Literal {body}",
        "saved $(false)",
        "",
    ]
    .into_iter()
    .enumerate()
    {
        if index == 1 {
            fs::write(fixture.repo.join("issue-template.md"), "repo {title}").unwrap();
            git(&fixture.repo, &["add", "issue-template.md"]);
            git(
                &fixture.repo,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "-m",
                    "Add template",
                ],
            );
        }
        if index >= 2 {
            let local = fixture.root.path().join("local.toml");
            fs::write(
                &local,
                if index == 2 {
                    "issue_template = 'saved {body}'"
                } else {
                    "issue_template = ''"
                },
            )
            .unwrap();
            fixture.ok(&[
                "repo",
                "config",
                fixture.repo.to_str().unwrap(),
                "--file",
                local.to_str().unwrap(),
            ]);
        }
        let output = fixture
            .command()
            .args([
                "--json",
                "add",
                fixture.repo.to_str().unwrap(),
                &format!("template-{index}"),
                "--base",
                "HEAD",
                "--issue",
                "44",
                "--agent",
                "claude",
            ])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.split_once('\n').unwrap().1, expected);
    }
}

#[test]
fn issue_command_finds_the_repository_and_starts_the_default_agent() {
    let fixture = Fixture::with_config(Some("default_agent = 'claude'\n"));
    let other = fixture.root.path().join("other");
    git(
        &fixture.repo,
        &["clone", "-q", ".", other.to_str().unwrap()],
    );
    git(
        &other,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:team/other.git",
        ],
    );
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    fixture.add_github_origin();
    let bin = fixture.root.path().join("issue-bin");
    fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$ISSUE_ARGS\"\ncat \"$ISSUE_RESPONSE\"\n",
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    for tool in ["codex", "claude"] {
        let script = bin.join(tool);
        fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\0' \"$(basename \"$0\")\" \"$@\" > \"$AGENT_ARGS\"\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let response = fixture.root.path().join("issue-response");
    let issue_args = fixture.root.path().join("issue-args");
    let agent_args = fixture.root.path().join("agent-args");
    let body = "Paste the URL and go.";
    for (number, agent, expected) in [
        (41, None, "claude"),
        (42, Some("codex"), "codex"),
        (43, None, "codex"),
        (45, None, "codex"),
        (46, None, "codex"),
        (47, None, "codex"),
    ] {
        if number == 43 {
            // The repository's checked-in default wins over the global one.
            fs::write(
                fixture.repo.join(".shoal.toml"),
                "default_agent = 'codex'\n",
            )
            .unwrap();
            git(&fixture.repo, &["add", ".shoal.toml"]);
            git(
                &fixture.repo,
                &[
                    "-c",
                    "user.name=Shoal Test",
                    "-c",
                    "user.email=shoal@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "default agent",
                ],
            );
        }
        fs::write(
            &response,
            serde_json::json!({"number": number, "title": "Paste an issue", "body": body})
                .to_string(),
        )
        .unwrap();
        let url = format!("https://github.com/team/project/issues/{number}");
        let number_input = number.to_string();
        let input = if number >= 45 { &number_input } else { &url };
        let mut args = vec!["--json", "issue", input, "--base", "HEAD"];
        if number == 47 {
            args.extend(["--repo", fixture.repo.to_str().unwrap()]);
        }
        if let Some(agent) = agent {
            args.extend(["--agent", agent]);
        }
        args.extend(["--", "--model", "test-model"]);
        let cwd = match number {
            45 => fixture.repo.join("nested"),
            46 => PathBuf::from(
                fixture.ok(&["inspect", "issue-41-paste-an-issue"])["workspace"]["path"]
                    .as_str()
                    .unwrap(),
            )
            .join("nested"),
            _ => other.clone(),
        };
        fs::create_dir_all(&cwd).unwrap();
        let output = fixture
            .command()
            .args(&args)
            .current_dir(cwd)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("ISSUE_RESPONSE", &response)
            .env("ISSUE_ARGS", &issue_args)
            .env("AGENT_ARGS", &agent_args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
        let name = format!("issue-{number}-paste-an-issue");
        assert_eq!(workspace["name"], name);
        assert_eq!(workspace["branch"], name);
        assert!(fs::read_to_string(&issue_args).unwrap().contains(&format!(
            "issue\0view\0{number}\0--repo\0github.com/team/project\0"
        )));
        let invocation = fs::read_to_string(&agent_args).unwrap();
        let mut parts = invocation.split('\0');
        assert_eq!(parts.next(), Some(expected));
        let prompt = parts.next().unwrap();
        assert!(prompt.contains(&url) && prompt.contains(body), "{prompt}");
        assert!(invocation.contains("\0--model\0test-model\0"));
        assert_eq!(
            fixture.ok(&["inspect", &name])["executions"],
            serde_json::json!([])
        );
    }
    // `add` accepts numbers and URLs without applying either configured default agent.
    fs::remove_file(&agent_args).unwrap();
    for (number, repository, input) in [
        (44, None, "https://github.com/team/project/issues/44"),
        (68, Some(fixture.repo.to_str().unwrap()), "68"),
    ] {
        fs::write(
            &response,
            serde_json::json!({"number": number, "title": "Create workspace", "body": body})
                .to_string(),
        )
        .unwrap();
        let mut command = fixture.command();
        command.args(["--json", "add"]);
        if let Some(repository) = repository {
            command.arg(repository);
        }
        let output = command
            .args(["--issue", input, "--base", "HEAD"])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("ISSUE_RESPONSE", &response)
            .env("ISSUE_ARGS", &issue_args)
            .env("AGENT_ARGS", &agent_args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            workspace["name"],
            format!("issue-{number}-create-workspace")
        );
        let inspection = fixture.ok(&["inspect", workspace["name"].as_str().unwrap()]);
        assert_eq!(inspection["workspace"]["state"], "ready");
        assert_eq!(inspection["executions"], serde_json::json!([]));
        assert!(!agent_args.exists());
    }

    // The default agent applies to the issue command, not ordinary additions.
    let output = fixture
        .command()
        .args([
            "--json",
            "add",
            fixture.repo.to_str().unwrap(),
            "plain",
            "--base",
            "HEAD",
        ])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("AGENT_ARGS", &agent_args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!agent_args.exists());
}

#[test]
fn issue_number_picks_a_repository_before_lookup_interactively() {
    let fixture = Fixture::with_config(Some("default_agent = 'claude'\n"));
    fixture.add_github_origin();
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    for (tool, script) in [
        ("fzf", "#!/bin/sh\nhead -n 1\n"),
        (
            "gh",
            "#!/bin/sh\nprintf 'lookup in %s' \"$PWD\" >&2\nexit 1\n",
        ),
    ] {
        let path = bin.join(tool);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (output, transcript) = fixture.interactive(&["issue", "103", "--base", "HEAD"], "");
    assert!(!output.status.success(), "{output:?}\n{transcript}");
    assert!(
        transcript.contains(&format!("lookup in {}", fixture.repo.display())),
        "{transcript}"
    );
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
}

#[test]
fn issue_lookup_errors_never_create_a_workspace() {
    let fixture = Fixture::new();
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/team/project.git",
        ],
    );
    let bin = fixture.root.path().join("issue-bin");
    fs::create_dir(&bin).unwrap();
    // Keep the missing-tool case independent of any installed forge clients.
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file() && path.metadata().unwrap().permissions().mode() & 0o111 != 0)
        .unwrap()
        .canonicalize()
        .unwrap();
    std::os::unix::fs::symlink(git, bin.join("git")).unwrap();
    let tool = bin.join("gh");
    for (input, script, diagnostic) in [
        ("4", None, "install it"),
        (
            "4",
            Some("#!/bin/sh\necho login-required >&2\nexit 1\n"),
            "login-required",
        ),
        (
            "4",
            Some("#!/bin/sh\necho bad-json\n"),
            "invalid gh issue response",
        ),
        (
            "https://github.com/other/project/issues/4",
            None,
            "different repository",
        ),
    ] {
        if let Some(script) = script {
            fs::write(&tool, script).unwrap();
            fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
        } else if tool.exists() {
            fs::remove_file(&tool).unwrap();
        }
        let output = fixture
            .command()
            .args([
                "add",
                fixture.repo.to_str().unwrap(),
                "--base",
                "HEAD",
                "--issue",
                input,
            ])
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{output:?}"
        );
        assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    }
    for (args, diagnostic) in [
        (
            vec!["add", "--issue", "https://github.com/team/other/issues/4"],
            "shoal repo add",
        ),
        (vec!["add", "--issue", "4"], "missing argument"),
        (
            vec!["issue", "https://github.com/team/project/issues/4"],
            "no agent selected",
        ),
        (vec!["issue", "4", "--agent", "codex"], "pass --repo"),
        (
            vec!["issue", "not-an-issue", "--agent", "codex"],
            "expected an issue URL",
        ),
        (
            vec![
                "issue",
                "0",
                "--repo",
                fixture.repo.to_str().unwrap(),
                "--agent",
                "codex",
            ],
            "must be positive",
        ),
        (
            vec!["issue", "4", "--repo", "unknown", "--agent", "codex"],
            "not registered",
        ),
        (
            vec![
                "issue",
                "https://github.com/team/other/issues/4",
                "--repo",
                fixture.repo.to_str().unwrap(),
                "--agent",
                "codex",
            ],
            "different repository",
        ),
        (
            vec![
                "issue",
                "https://github.com/team/other/issues/4",
                "--agent",
                "codex",
            ],
            "shoal repo add",
        ),
        (
            vec![
                "issue",
                "https://github.com/team/project/pull/4",
                "--agent",
                "codex",
            ],
            "/issues/<number>",
        ),
    ] {
        let output = fixture
            .command()
            .args(&args)
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{output:?}"
        );
        assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    }
}

#[test]
fn add_existing_branch_runs_setup_once_and_denies_scoped_creation() {
    let fixture = Fixture::new();
    for hook in ["setup", "post"] {
        let path = fixture.repo.join(hook);
        fs::write(&path, format!("#!/bin/sh\necho {hook} >> runs\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "setup_cmd = './setup'\npost_setup_cmd = './post'\n",
    )
    .unwrap();
    git(&fixture.repo, &["add", "."]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "hooks",
        ],
    );
    git(&fixture.repo, &["branch", "coworker"]);
    let args = [
        "add",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "coworker",
    ];
    let opened = fixture.ok(&args);
    let path = Path::new(opened["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(path.join("runs")).unwrap(),
        "setup\npost\n"
    );
    let reopened = fixture.ok(&args);
    assert_eq!(opened["id"], reopened["id"]);
    assert_eq!(
        fs::read_to_string(path.join("runs")).unwrap(),
        "setup\npost\n"
    );
    let output = fixture.run(&[
        "exec",
        "coworker",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "add",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "main",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("workspace processes"),
        "{output:?}"
    );
    assert!(
        !fixture
            .run(&[
                "add",
                fixture.repo.to_str().unwrap(),
                "other",
                "--existing",
                "coworker",
            ])
            .status
            .success()
    );
    for flag in ["--base", "--issue"] {
        assert!(
            !fixture
                .run(&[
                    "add",
                    fixture.repo.to_str().unwrap(),
                    "--existing",
                    "coworker",
                    flag,
                    "other"
                ])
                .status
                .success()
        );
    }
}

#[test]
fn interactive_add_picks_existing_branch_and_reopens_workspace() {
    let fixture = Fixture::new();
    git(&fixture.repo, &["branch", "coworker/topic"]);
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let picker = bin.join("fzf");
    fs::write(&picker, "#!/bin/sh\nawk -F '\\t' '$2 == \"Use an existing branch\" || $1 == \"refs/heads/coworker/topic\" {print}'\n").unwrap();
    fs::set_permissions(&picker, fs::Permissions::from_mode(0o755)).unwrap();
    let directive = fixture.root.path().join("destination");
    for _ in 0..2 {
        let (_master, slave) = pty::open();
        let output = fixture
            .command()
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .args(["add", fixture.repo.to_str().unwrap()])
            .stdin(slave.try_clone().unwrap())
            .stderr(slave)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let workspace = fixture.ok(&["inspect", "coworker-topic"]);
        assert_eq!(
            fs::read_to_string(&directive).unwrap().trim(),
            workspace["workspace"]["path"].as_str().unwrap()
        );
    }
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

fn wait_removed(fixture: &Fixture, name: &str) {
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

fn wait_pr_error(fixture: &Fixture, name: &str, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let inspection = fixture.ok(&["inspect", name]);
        if inspection["pr_cleanup"]["error"]
            .as_str()
            .is_some_and(|s| s.contains(message))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {message}: {inspection}"
        );
        thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn merged_stops_agent_and_releases_resources_without_idle_delay() {
    let fixture = Fixture::with_config(Some("[auto_cleanup]\nenabled=false\n[resources.device]\n"));
    let workspace = fixture.add("merged");
    fixture.ok(&["port", "acquire", "web", "merged"]);
    fixture.ok(&["resource", "acquire", "device", "merged"]);
    let mut wrapper = fixture
        .command()
        .args(["exec", "merged", "--", "sleep", "60"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "merged");
    fixture.ok(&["pr", "merged", "merged"]);
    wait_removed(&fixture, "merged");
    assert!(!wrapper.wait().unwrap().success());
    assert!(!Path::new(workspace["path"].as_str().unwrap()).exists());
    assert!(
        fixture
            .ok(&["resource", "list", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .ok(&["port", "list", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn merged_retains_dirty_work_and_changed_head_across_restart_and_can_be_cancelled() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("retain");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("dirty"), "keep").unwrap();
    fixture.ok(&[
        "exec",
        "retain",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "--json",
        "pr",
        "merged",
    ]);
    wait_pr_error(&fixture, "retain", "uncommitted");
    git(path, &["add", "dirty"]);
    git(
        path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "new work",
        ],
    );
    fixture.restart();
    wait_pr_error(&fixture, "retain", "HEAD changed");
    assert!(path.join("dirty").exists());
    fixture.ok(&["pr", "clear", "retain"]);
    assert!(fixture.ok(&["inspect", "retain"])["pr_cleanup"].is_null());
}

#[test]
fn pr_cleanup_can_be_disabled_independently() {
    let fixture = Fixture::with_config(Some("[pr_cleanup]\nenabled=false\n"));
    fixture.add("keep");
    let output = fixture.run(&["pr", "merged", "keep"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("PR cleanup is disabled"));
    assert!(fixture.ok(&["inspect", "keep"])["pr_cleanup"].is_null());
}

#[test]
fn legacy_pr_registrations_survive_disabled_cleanup_and_clear_after_restart() {
    let mut fixture = Fixture::with_config(Some(
        "[pr_cleanup]\nenabled=false\n[auto_cleanup]\nenabled=false\n",
    ));
    for name in ["watch", "acknowledged"] {
        let workspace = fixture.add(name);
        let head = git(
            Path::new(workspace["path"].as_str().unwrap()),
            &["rev-parse", "HEAD"],
        );
        let record = if name == "watch" {
            serde_json::json!({"url": "https://forge.example/team/repo/pulls/7", "head": null, "error": "previous lookup failure"})
        } else {
            serde_json::json!({"url": null, "head": head.trim(), "error": null})
        };
        let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
        db.execute(
            "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
            rusqlite::params![workspace["id"].as_str().unwrap(), record.to_string()],
        )
        .unwrap();
        fixture.restart();
        assert_eq!(fixture.ok(&["inspect", name])["pr_cleanup"], record);
        for command in ["merged", "7"] {
            let output = fixture.run(&["pr", command, name]);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("PR cleanup is disabled"));
        }
        assert_eq!(fixture.ok(&["inspect", name])["pr_cleanup"], record);
        // Clear remains allowed while disabled, including for a scoped caller.
        assert_eq!(
            fixture.ok(&[
                "exec",
                name,
                "--",
                env!("CARGO_BIN_EXE_shoal"),
                "--json",
                "pr",
                "clear"
            ]),
            serde_json::json!({"registered": false})
        );
        fixture.restart();
        assert!(fixture.ok(&["inspect", name])["pr_cleanup"].is_null());
        assert_eq!(
            fixture.ok(&["pr", "clear", name]),
            serde_json::json!({"registered": false})
        );
        assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    }
}

#[test]
fn invalid_persisted_pr_registration_retains_workspace_and_can_be_cleared() {
    let mut fixture = Fixture::with_config(Some("[auto_cleanup]\nenabled=false\n"));
    let workspace = fixture.add("invalid");
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    for record in [
        serde_json::json!({"url": null, "head": null, "error": null}),
        serde_json::json!({"url": "https://forge.example/team/repo/pulls/7", "head": "abc123", "error": null}),
    ] {
        db.execute(
            "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
            rusqlite::params![workspace["id"].as_str().unwrap(), record.to_string()],
        )
        .unwrap();
        fixture.restart();
        let output = fixture.run(&["inspect", "invalid"]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("expected exactly one of url or head")
        );
        assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
        fixture.ok(&["pr", "clear", "invalid"]);
        assert!(fixture.ok(&["inspect", "invalid"])["pr_cleanup"].is_null());
    }
}

#[test]
fn repository_config_sets_pr_cleanup_over_the_global_default() {
    let fixture = Fixture::with_config(Some("[pr_cleanup]\nenabled=false\n"));
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "[pr_cleanup]\nenabled=true\n",
    )
    .unwrap();
    git(&fixture.repo, &["add", ".shoal.toml"]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "-q",
            "-m",
            "enable pr cleanup",
        ],
    );
    fixture.add("merged");
    fixture.ok(&["pr", "merged", "merged"]);
    wait_removed(&fixture, "merged");
    // The saved config is the top layer.
    let saved = fixture.root.path().join("saved.toml");
    fs::write(&saved, "[pr_cleanup]\nenabled=false\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    fixture.add("kept");
    let output = fixture.run(&["pr", "merged", "kept"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("PR cleanup is disabled"));
}

#[test]
fn pr_watch_checks_github_state_and_commit_and_survives_restart() {
    let mut fixture = Fixture::with_config(Some("[auto_cleanup]\nenabled=false\n"));
    let workspace = fixture.add("watch");
    let failed = fixture.run(&["pr", "56", "watch"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("origin remote"));
    assert!(fixture.ok(&["inspect", "watch"])["pr_cleanup"].is_null());
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/team/repo.git",
        ],
    );
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("gh"), "#!/bin/sh\ncat \"$HOME/pr.json\"\n").unwrap();
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    let failed = fixture.run(&["pr", "https://github.com/team/repo/pull/56", "watch"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("shoal pr merged"));
    assert!(fixture.ok(&["inspect", "watch"])["pr_cleanup"].is_null());
    assert!(
        !fixture
            .run(&["pr", "https://github.com/other/repo/pull/56", "watch"])
            .status
            .success()
    );
    let response = fixture.root.path().join("pr.json");
    let write_response = |state: &str, head: &str| {
        fs::write(&response, serde_json::json!({"number":56,"state":state,"headRefName":"watch","commits":[{"oid":head}]}).to_string()).unwrap()
    };
    let head = git(
        Path::new(workspace["path"].as_str().unwrap()),
        &["rev-parse", "HEAD"],
    )
    .trim()
    .to_owned();
    write_response("OPEN", &head);
    fixture.ok(&["pr", "https://github.com/team/repo/pull/56", "watch"]);
    fixture.ok(&["pr", "clear", "watch"]);
    fixture.ok(&["pr", "56", "watch"]);
    fixture.ok(&["pr", "clear", "watch"]);
    // Scoped callers can omit the workspace and register by number too.
    fixture.ok(&[
        "exec",
        "watch",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "--json",
        "pr",
        "56",
    ]);
    fixture.restart();
    assert_eq!(
        fixture.ok(&["inspect", "watch"])["pr_cleanup"]["url"],
        "https://github.com/team/repo/pull/56"
    );
    // A persisted number must not silently follow a changed origin.
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/other/repo.git",
        ],
    );
    fixture.restart();
    wait_pr_error(&fixture, "watch", "different repository");
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/team/repo.git",
        ],
    );
    write_response("MERGED", &"a".repeat(40));
    fixture.restart();
    wait_pr_error(&fixture, "watch", "does not contain");
    fs::write(&response, "malformed output").unwrap();
    fixture.restart();
    wait_pr_error(&fixture, "watch", "expected");
    write_response("CLOSED", &head);
    fixture.restart();
    assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    write_response("MERGED", &head);
    fixture.restart();
    wait_removed(&fixture, "watch");
}

#[test]
fn pr_watch_checks_forgejo_merge_and_commits_with_fixture_cli() {
    let fixture = Fixture::new();
    let workspace = fixture.add("fj-watch");
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://forge.example/team/repo.git",
        ],
    );
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("fj"), "#!/bin/sh\nfor arg; do last=$arg; done\nif [ \"$last\" = commits ]; then cat \"$HOME/commits\"; else printf 'Title #56\\nBy user — Merged — +1 -0\\nFrom `fj-watch` into `main`\\n'; fi\n").unwrap();
    fs::set_permissions(bin.join("fj"), fs::Permissions::from_mode(0o755)).unwrap();
    let head = git(
        Path::new(workspace["path"].as_str().unwrap()),
        &["rev-parse", "HEAD"],
    );
    fs::write(
        fixture.root.path().join("commits"),
        format!("commit {} (+1, -0)\nAuthor: Test\n", head.trim()),
    )
    .unwrap();
    fixture.ok(&["pr", "56", "fj-watch"]);
    wait_removed(&fixture, "fj-watch");
}

#[test]
fn merged_rechecks_head_after_removal_hooks() {
    for key in ["pre_remove_cmd", "pre_resource_release_cmd"] {
        let fixture = Fixture::new();
        fs::write(
            fixture.repo.join(".shoal.toml"),
            format!("{key} = 'hook.sh'\n[resources.signing]\n"),
        )
        .unwrap();
        fs::write(fixture.repo.join("hook.sh"), "#!/bin/sh\ngit -c user.name=Test -c user.email=test@example.invalid commit --allow-empty -m 'hook work'\n").unwrap();
        fs::set_permissions(
            fixture.repo.join("hook.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        git(&fixture.repo, &["add", "."]);
        git(
            &fixture.repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "hook",
            ],
        );
        let workspace = fixture.add("hook");
        if key == "pre_resource_release_cmd" {
            fixture.ok(&["resource", "acquire", "signing", "hook"]);
        }
        fixture.ok(&["pr", "merged", "hook"]);
        wait_pr_error(&fixture, "hook", "HEAD changed during removal hooks");
        assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    }
}

#[test]
fn land_merges_into_main_without_a_remote_and_is_denied_to_scoped_processes() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    merge_commit(path, "landed", "from the workspace\n");
    let expected = git(path, &["rev-parse", "HEAD"]);
    let before = git(&fixture.repo, &["rev-parse", "main"]);
    let binary = env!("CARGO_BIN_EXE_shoal");
    let denied = fixture.run(&["exec", "worker", "--", binary, "--json", "land"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("cannot land"));
    let denied = fixture.run(&["exec", "worker", "--", binary, "land-internal", "{}"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("only an authorized landing"));
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
    let result = fixture.ok(&["land", "worker"]);
    assert_eq!(result["updated"], true);
    assert_eq!(result["fast_forward"], true);
    assert_eq!(result["default_branch"], "main");
    assert_eq!(result["previous_commit"], before.trim());
    assert_eq!(result["commit"], expected.trim());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), expected);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("landed")).unwrap(),
        "from the workspace\n"
    );
    assert_eq!(fixture.ok(&["land", "worker"])["updated"], false);
    // Landed work needs no branch choice.
    assert_eq!(fixture.ok(&["rm", "worker"])["branch_deleted"], true);
}

#[test]
fn land_interruptions_stop_merge_drivers_and_restore_the_default_checkout() {
    for interruption in ["stop", "daemon", "interrupt"] {
        let mut fixture = Fixture::new();
        git(&fixture.repo, &["config", "user.name", "Test"]);
        git(
            &fixture.repo,
            &["config", "user.email", "test@example.invalid"],
        );
        fs::write(fixture.repo.join(".gitattributes"), "tracked merge=slow\n").unwrap();
        merge_commit(&fixture.repo, ".gitattributes", "tracked merge=slow\n");
        let worker = fixture.add("worker");
        let path = Path::new(worker["path"].as_str().unwrap());
        merge_commit(path, "tracked", "worker\n");
        merge_commit(path, "a-added", "new file\n");
        merge_commit(&fixture.repo, "tracked", "main\n");
        let before = git(&fixture.repo, &["rev-parse", "HEAD"]);
        fs::write(
            fixture.repo.join(".git/info/exclude"),
            ".merge_file_ABC123\n",
        )
        .unwrap();
        fs::write(fixture.repo.join(".merge_file_ABC123"), "preexisting\n").unwrap();
        git(
            &fixture.repo,
            &[
                "config",
                "merge.slow.driver",
                "echo $$ > .git/land-driver.pid; exec sleep 60",
            ],
        );
        let stderr_path = fixture.root.path().join("land.stderr");
        let mut land = fixture
            .command()
            .args(["land", "worker"])
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr_path).unwrap())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fixture.repo.join(".git/land-driver.pid").exists() {
            assert!(
                land.try_wait().unwrap().is_none(),
                "land exited before merge driver"
            );
            assert!(Instant::now() < deadline, "merge driver did not start");
            thread::sleep(Duration::from_millis(20));
        }
        let pid: i32 = fs::read_to_string(fixture.repo.join(".git/land-driver.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let during = fixture.ok(&["inspect", "worker"]);
        assert_eq!(during["executions"].as_array().unwrap().len(), 1);
        assert!(during["executions"][0]["child"].is_object());
        if interruption == "daemon" {
            fixture.daemon.child.kill().unwrap();
            fixture.daemon.child.wait().unwrap();
        } else if interruption == "interrupt" {
            assert_eq!(unsafe { libc::kill(land.id() as i32, libc::SIGINT) }, 0);
        } else {
            fixture.ok(&["stop", "worker"]);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = land.try_wait().unwrap() {
                assert!(!status.success());
                break;
            }
            assert!(Instant::now() < deadline, "landing wrapper did not stop");
            thread::sleep(Duration::from_millis(20));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "stat="])
                .output()
                .unwrap();
            let status = String::from_utf8_lossy(&output.stdout);
            if status.trim().is_empty() || status.trim().starts_with('Z') {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "merge driver survived landing stop"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD"]), before);
        assert_eq!(
            git(&fixture.repo, &["status", "--porcelain"]),
            "",
            "{interruption}: {}",
            fs::read_to_string(&stderr_path).unwrap()
        );
        assert_eq!(
            git(&fixture.repo, &["write-tree"]),
            git(&fixture.repo, &["rev-parse", "HEAD^{tree}"])
        );
        assert!(!fixture.repo.join(".git/index.lock").exists());
        assert!(!fixture.repo.join(".git/MERGE_HEAD").exists());
        assert_eq!(
            fs::read_to_string(fixture.repo.join(".merge_file_ABC123")).unwrap(),
            "preexisting\n"
        );
        assert!(path.exists());
    }
}

/// A stand-in for Happy's CLI that records how it was started, writes a line
/// of output, and then waits to be stopped.
fn install_fake_happy(fixture: &Fixture) -> PathBuf {
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("happy");
    fs::write(
        &script,
        r#"#!/bin/sh
{
  printf 'cwd=%s\n' "$PWD"
  printf 'args='; printf '%s\0' "$@"; printf '\n'
  printf 'scope=%s\nexecution=%s\nworkspace=%s\nport=%s\n' "$SHOAL_SCOPE_TOKEN" "$SHOAL_EXECUTION_ID" "$SHOAL_WORKSPACE" "$SHOAL_PORT_WEB"
  printf 'reconnect=%s|%s|%s|%s|%s|%s\n' "$HAPPY_RECONNECT_SESSION_ID" "$HAPPY_RECONNECT_ENCRYPTION_KEY" "$HAPPY_RECONNECT_ENCRYPTION_VARIANT" "$HAPPY_RECONNECT_SEQ" "$HAPPY_RECONNECT_METADATA_VERSION" "$HAPPY_RECONNECT_AGENT_STATE_VERSION"
  if read -r _line; then printf 'stdin=data\n'; else printf 'stdin=eof\n'; fi
  test -t 1 && printf 'stdout=tty\n' || printf 'stdout=notty\n'
} > "$HAPPY_RECORD"
echo "hello from happy"
echo "happy stderr" >&2
# A real session connects to Happy's server and heartbeats; tell the fake server.
if [ -n "$HAPPY_RECONNECT_SESSION_ID" ] && [ -n "$HAPPY_SERVER_URL" ]; then
  sleep 1
  curl -s -X POST "$HAPPY_SERVER_URL/test/activate/$HAPPY_RECONNECT_SESSION_ID" > /dev/null
fi
sleep 300
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.root.path().join("happy-record")
}

fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn happy_sessions_launch_detached_tracked_and_stop_with_the_workspace() {
    let fixture = Fixture::new();
    let record = install_fake_happy(&fixture);
    let state_file = fixture.root.path().join(".happy/daemon.state.json");
    for (operation, agent) in [("stop", "codex"), ("rm", "claude")] {
        let name = format!("happy-{agent}");
        let launch = if agent == "codex" {
            // No Happy daemon state: warn, but launch anyway.
            let output = fixture
                .command()
                .args([
                    "--json",
                    "add",
                    fixture.repo.to_str().unwrap(),
                    &name,
                    "--agent",
                    "happy-codex",
                    "--",
                    "--yolo",
                ])
                .env("HAPPY_RECORD", &record)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("daemon.state.json") && stderr.contains("happy daemon start"),
                "{stderr}"
            );
            let mut lines = output
                .stdout
                .split(|b| *b == b'\n')
                .filter(|l| !l.is_empty());
            let workspace: Value = serde_json::from_slice(lines.next().unwrap()).unwrap();
            assert_eq!(workspace["name"], name);
            let launch: Value = serde_json::from_slice(lines.next().unwrap()).unwrap();
            assert!(lines.next().is_none());
            assert_eq!(launch["happy_daemon_recorded"], false);
            assert_eq!(launch["prompt_file"], Value::Null);
            let config: toml::Value = toml::from_str(
                &fs::read_to_string(fixture.root.path().join(".codex/config.toml")).unwrap(),
            )
            .unwrap();
            let key = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
            assert_eq!(
                config["projects"][key.to_str().unwrap()]["trust_level"].as_str(),
                Some("trusted")
            );
            launch
        } else {
            let workspace = fixture.add(&name);
            fixture.ok(&["port", "acquire", "web", &name, "--reason", "server"]);
            fs::create_dir_all(state_file.parent().unwrap()).unwrap();
            fs::write(&state_file, "{}").unwrap();
            let claude_config = fixture.root.path().join(".claude.json");
            fs::write(&claude_config, "{}").unwrap();
            let output = fixture
                .command()
                .args(["--json", "happy", "claude", &name, "--", "--model", "test"])
                .env("HAPPY_RECORD", &record)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(output.stderr, b"", "{output:?}");
            let launch: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(launch["workspace"]["id"], workspace["id"]);
            assert_eq!(launch["happy_daemon_recorded"], true);
            // Detached Claude launches skip the trust dialog like `shoal claude`.
            let config: Value =
                serde_json::from_str(&fs::read_to_string(&claude_config).unwrap()).unwrap();
            let key = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
            assert_eq!(
                config["projects"][key.to_str().unwrap()]["hasTrustDialogAccepted"],
                true
            );
            launch
        };
        assert_eq!(launch["agent"], agent);
        let pid = launch["pid"].as_u64().unwrap() as u32;
        let execution_id = launch["execution_id"].as_str().unwrap();
        let log = PathBuf::from(launch["log"].as_str().unwrap());
        assert!(log.starts_with(fixture.root.path().join("state/workspaces")));
        let inspection = fixture.ok(&["inspect", &name]);
        let executions = inspection["executions"].as_array().unwrap();
        assert_eq!(executions.len(), 1, "{inspection}");
        assert_eq!(executions[0]["id"], execution_id);
        assert_eq!(executions[0]["group_id"], pid);
        assert_eq!(executions[0]["state"], "running");
        wait_until("fake happy record", || record.exists());
        let recorded = fs::read_to_string(&record).unwrap();
        let path = fs::canonicalize(inspection["workspace"]["path"].as_str().unwrap()).unwrap();
        assert!(
            recorded.contains(&format!("cwd={}\n", path.display())),
            "{recorded}"
        );
        let expected_args = if agent == "codex" {
            "args=codex\0--happy-starting-mode\0remote\0--started-by\0daemon\0--yolo\0\n"
        } else {
            "args=claude\0--happy-starting-mode\0remote\0--started-by\0daemon\0--model\0test\0\n"
        };
        assert!(recorded.contains(expected_args), "{recorded}");
        assert!(recorded.contains(&format!("execution={execution_id}\n")));
        assert!(recorded.contains(&format!("workspace={name}\n")));
        assert!(!recorded.contains("scope=\n"), "{recorded}");
        assert!(recorded.contains("stdin=eof\nstdout=notty\n"), "{recorded}");
        if agent == "claude" {
            let port = fixture.ok(&["port", &name])["reserved"][0]["port"]
                .as_u64()
                .unwrap();
            assert!(recorded.contains(&format!("port={port}\n")), "{recorded}");
        }
        wait_until("happy output in the log", || {
            fs::read_to_string(&log).is_ok_and(|text| text.contains("hello from happy"))
        });
        let text = fs::read_to_string(&log).unwrap();
        assert!(text.starts_with("shoal: starting happy "), "{text}");
        assert!(text.contains("happy stderr"), "{text}");
        assert!(process_alive(pid));

        fixture.ok(&[operation, &name]);
        wait_until("happy to exit", || !process_alive(pid));
        if operation == "stop" {
            assert_eq!(
                fixture.ok(&["inspect", &name])["executions"],
                serde_json::json!([])
            );
            assert!(log.exists());
            fixture.ok(&["rm", &name]);
        }
        assert!(
            !log.parent().unwrap().exists(),
            "workspace run data removed"
        );
        fs::remove_file(&record).unwrap();
    }
}

#[test]
fn happy_issue_prompts_reach_claude_and_are_saved_for_codex() {
    let fixture = Fixture::new();
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("agent-template.md"), "General {workspace}").unwrap();
    fs::write(
        config_dir.join("issue-template.md"),
        "Issue {title}: {body}",
    )
    .unwrap();
    let record = install_fake_happy(&fixture);
    let gh = fixture.root.path().join("bin/gh");
    let title = "Fix API timeout";
    let body = "Keep the connection alive.";
    fs::write(
        &gh,
        format!(
            "#!/bin/sh\nprintf '%s' '{}'\n",
            serde_json::json!({"number": 34, "title": title, "body": body})
        ),
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.add_github_origin();
    for agent in ["happy-claude", "happy-codex"] {
        let output = fixture
            .command()
            .args([
                "--json",
                "add",
                fixture.repo.to_str().unwrap(),
                "--base",
                "HEAD",
                "--issue",
                "34",
                "--agent",
                agent,
                "--",
                "--model",
                "test",
            ])
            .env("HAPPY_RECORD", &record)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let launch: Value =
            serde_json::from_slice(output.stdout.split(|b| *b == b'\n').nth(1).unwrap()).unwrap();
        let name = launch["workspace"]["name"].as_str().unwrap().to_owned();
        assert_eq!(name, "issue-34-fix-api-timeout");
        wait_until("fake happy record", || record.exists());
        let recorded = fs::read_to_string(&record).unwrap();
        // The prompt spans lines; the record ends the NUL-separated list before `scope=`.
        let args: Vec<&str> = recorded
            .split_once("args=")
            .unwrap()
            .1
            .split_once("\nscope=")
            .unwrap()
            .0
            .split('\0')
            .collect();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if agent == "happy-claude" {
            assert_eq!(
                args[..5],
                [
                    "claude",
                    "--happy-starting-mode",
                    "remote",
                    "--started-by",
                    "daemon"
                ]
            );
            assert!(
                args[5].contains(title) && args[5].contains(body),
                "{args:?}"
            );
            assert_eq!(
                args[6..8],
                ["--append-system-prompt", "General issue-34-fix-api-timeout"]
            );
            assert_eq!(args[8..10], ["--model", "test"]);
            assert_eq!(launch["prompt_file"], Value::Null);
            assert_eq!(launch["prompt_delivered"], true, "{launch}");
            assert!(!stderr.contains("initial prompt"), "{stderr}");
        } else {
            assert_eq!(
                args[..7],
                [
                    "codex",
                    "--happy-starting-mode",
                    "remote",
                    "--started-by",
                    "daemon",
                    "--model",
                    "test"
                ]
            );
            // Not logged in to Happy: the prompt is saved and the user is told.
            let prompt_file = PathBuf::from(launch["prompt_file"].as_str().unwrap());
            let prompt = fs::read_to_string(&prompt_file).unwrap();
            assert_eq!(
                prompt,
                format!("General issue-34-fix-api-timeout\n\nIssue {title}: {body}")
            );
            assert_eq!(launch["prompt_delivered"], false);
            assert_eq!(launch["happy_session_id"], Value::Null);
            assert!(
                stderr.contains("cannot deliver the prompt through Happy")
                    && stderr.contains("access.key")
                    && stderr.contains(prompt_file.to_str().unwrap()),
                "{stderr}"
            );
            assert!(recorded.contains("reconnect=|||||\n"), "{recorded}");
        }
        let pid = launch["pid"].as_u64().unwrap() as u32;
        fixture.ok(&["rm", &name, "--yes", "--delete-branch"]);
        wait_until("happy to exit", || !process_alive(pid));
        fs::remove_file(&record).unwrap();
    }
}

/// A local stand-in for Happy's server; killed when dropped.
struct FakeHappyServer {
    child: Child,
    url: String,
    record: PathBuf,
}

impl FakeHappyServer {
    fn start(fixture: &Fixture) -> Self {
        use std::io::BufRead;
        let script = fixture.root.path().join("happy_server.py");
        fs::write(&script, include_str!("fixtures/happy_server.py")).unwrap();
        let record = fixture.root.path().join("happy-server.json");
        let mut child = Command::new("python3")
            .arg(&script)
            .arg(&record)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut port = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        Self {
            child,
            url: format!("http://127.0.0.1:{}", port.trim()),
            record,
        }
    }

    fn state(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(&self.record).unwrap()).unwrap()
    }
}

impl Drop for FakeHappyServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn happy_codex_prompts_are_delivered_through_a_seeded_session() {
    use base64::Engine;
    let base64 = base64::engine::general_purpose::STANDARD;
    let fixture = Fixture::new();
    let record = install_fake_happy(&fixture);
    let server = FakeHappyServer::start(&fixture);
    let happy_home = fixture.root.path().join(".happy");
    fs::create_dir_all(&happy_home).unwrap();
    fs::write(
        happy_home.join("settings.json"),
        r#"{"machineId": "machine-1"}"#,
    )
    .unwrap();
    fs::write(happy_home.join("daemon.state.json"), "{}").unwrap();
    let key = base64.encode([7u8; 32]);
    let prompt = "Fix the login bug; keep the API stable.";
    let plaintext = serde_json::json!({
        "role": "user",
        "content": {"type": "text", "text": prompt},
        "meta": {"sentFrom": "shoal"},
    })
    .to_string()
    .len();
    for (variant, credentials) in [
        (
            "dataKey",
            serde_json::json!({"token": "test-token", "encryption": {"publicKey": key, "machineKey": key}}),
        ),
        (
            "legacy",
            serde_json::json!({"token": "test-token", "secret": key}),
        ),
    ] {
        fs::write(happy_home.join("access.key"), credentials.to_string()).unwrap();
        let name = format!("seeded-{}", variant.to_lowercase());
        fixture.add(&name);
        let output = fixture
            .command()
            .args([
                "--json", "happy", "codex", &name, "--prompt", prompt, "--", "--yolo",
            ])
            .env("HAPPY_RECORD", &record)
            .env("HAPPY_SERVER_URL", &server.url)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let launch: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(launch["prompt_delivered"], true, "{launch}");
        let session_id = launch["happy_session_id"].as_str().unwrap().to_owned();
        assert!(session_id.starts_with("session-"));
        assert_eq!(
            fs::read_to_string(launch["prompt_file"].as_str().unwrap()).unwrap(),
            prompt
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("warning"),
            "{output:?}"
        );

        // The CLI was launched attached to the seeded session.
        wait_until("fake happy record", || record.exists());
        let recorded = fs::read_to_string(&record).unwrap();
        let reconnect = recorded
            .lines()
            .find_map(|line| line.strip_prefix("reconnect="))
            .unwrap();
        let fields: Vec<&str> = reconnect.split('|').collect();
        assert_eq!(fields[0], session_id);
        assert_eq!(base64.decode(fields[1]).unwrap().len(), 32);
        assert_eq!(fields[2..], [variant, "0", "1", "1"]);
        if variant == "legacy" {
            assert_eq!(fields[1], key, "legacy sessions use the account secret");
        }
        assert!(recorded.contains(
            "args=codex\0--happy-starting-mode\0remote\0--started-by\0daemon\0--yolo\0\n"
        ));

        // The server saw a session created the way happy-cli creates them,
        // a wait for it to come alive, and one encrypted user message.
        let state = server.state();
        let requests = state["requests"].as_array().unwrap();
        let created = requests
            .iter()
            .rev()
            .find(|r| r["path"] == "/v1/sessions" && r["body"]["tag"].is_string())
            .unwrap();
        assert_eq!(created["authorization"], "Bearer test-token");
        assert!(created["client"].as_str().unwrap().starts_with("shoal/"));
        let metadata = base64
            .decode(created["body"]["metadata"].as_str().unwrap())
            .unwrap();
        let sealed = &created["body"]["dataEncryptionKey"];
        if variant == "dataKey" {
            assert_eq!(metadata[0], 0);
            assert_eq!(
                base64.decode(sealed.as_str().unwrap()).unwrap().len(),
                1 + 32 + 24 + 32 + 16
            );
        } else {
            assert!(metadata.len() > 24 + 16);
            assert_eq!(*sealed, Value::Null);
        }
        assert!(requests.iter().any(|r| r["path"] == "/v2/sessions/active"));
        let messages = state["messages"][&session_id].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert!(!messages[0]["localId"].as_str().unwrap().is_empty());
        let content = base64
            .decode(messages[0]["content"].as_str().unwrap())
            .unwrap();
        let expected = if variant == "dataKey" {
            1 + 12 + plaintext + 16
        } else {
            24 + plaintext + 16
        };
        assert_eq!(content.len(), expected);
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["path"] == "/v1/sessions" && r["method"] == "POST")
                .count(),
            if variant == "dataKey" { 1 } else { 2 }
        );

        let pid = launch["pid"].as_u64().unwrap() as u32;
        fixture.ok(&["rm", &name]);
        wait_until("happy to exit", || !process_alive(pid));
        fs::remove_file(&record).unwrap();
    }

    // Without a prompt nothing is seeded; a wrong token fails delivery softly.
    fs::write(
        happy_home.join("access.key"),
        r#"{"token": "wrong", "secret": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}"#,
    )
    .unwrap();
    fixture.add("unseeded");
    let output = fixture
        .command()
        .args(["--json", "happy", "codex", "unseeded", "--prompt", "hello"])
        .env("HAPPY_RECORD", &record)
        .env("HAPPY_SERVER_URL", &server.url)
        // Launched from inside another seeded session: its attachment must not leak.
        .env("HAPPY_RECONNECT_SESSION_ID", "parent-session")
        .env("HAPPY_RECONNECT_ENCRYPTION_KEY", &key)
        .env("HAPPY_RECONNECT_ENCRYPTION_VARIANT", "legacy")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let launch: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(launch["prompt_delivered"], false);
    assert_eq!(launch["happy_session_id"], Value::Null);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot deliver the prompt through Happy") && stderr.contains("401"),
        "{stderr}"
    );
    wait_until("fake happy record", || record.exists());
    let recorded = fs::read_to_string(&record).unwrap();
    assert!(recorded.contains("reconnect=|||||\n"), "{recorded}");
    let pid = launch["pid"].as_u64().unwrap() as u32;
    fixture.ok(&["rm", "unseeded"]);
    wait_until("happy to exit", || !process_alive(pid));

    // A seeded session that no launch will ever attach to is deleted again.
    fs::write(
        happy_home.join("access.key"),
        serde_json::json!({"token": "test-token", "secret": key}).to_string(),
    )
    .unwrap();
    fs::remove_file(fixture.root.path().join("bin/happy")).unwrap();
    fixture.add("orphan");
    let output = fixture
        .command()
        .args(["--json", "happy", "codex", "orphan", "--prompt", "hello"])
        .env("HAPPY_SERVER_URL", &server.url)
        // Only the fixture directory and system tools: a real happy must not be found.
        .env(
            "PATH",
            format!(
                "{}:/usr/bin:/bin",
                fixture.root.path().join("bin").display()
            ),
        )
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    let state = server.state();
    let methods: Vec<&str> = state["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["path"].as_str().unwrap().starts_with("/v1/sessions"))
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(methods.last(), Some(&"DELETE"), "{methods:?}");
    assert_eq!(state["sessions"].as_object().unwrap().len(), 2, "{state}");
}

#[test]
fn notifications_report_conflicts_agent_exits_and_removals_once() {
    let fixture = Fixture::with_config(Some(
        "[auto_cleanup]\nenabled=false\n[resources.lock]\ncapacity=1\n",
    ));
    fixture.add("holder");
    fixture.add("waiter");
    assert_eq!(
        fixture.run(&["notifications"]).stdout,
        b"No new notifications\n"
    );
    fixture.ok(&["resource", "acquire", "lock", "holder"]);
    for _ in 0..2 {
        let busy = fixture.run(&["resource", "acquire", "lock", "waiter"]);
        assert_eq!(busy.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&busy.stdout).contains("held by holder"),
            "{busy:?}"
        );
    }
    let taken = fixture.ok(&["port", "acquire", "web", "holder"])["port"]
        .as_u64()
        .unwrap();
    let moved = fixture.ok(&[
        "port",
        "acquire",
        "web",
        "waiter",
        "--port",
        &taken.to_string(),
        "--on-conflict",
        "auto",
    ]);
    assert_ne!(moved["port"], taken);
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("claude"), "#!/bin/sh\nexit 7\n").unwrap();
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    let agent = fixture
        .command()
        .args(["claude", "waiter"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    assert_eq!(agent.status.code(), Some(7), "{agent:?}");
    // Plain commands are the user's own; only agent shortcuts announce their exit.
    assert!(
        fixture
            .run(&["exec", "waiter", "--", "true"])
            .status
            .success()
    );

    let status = fixture.ok(&["daemon", "status"]);
    assert_eq!(status["daemon"]["unread_notifications"], 3);
    let list = fixture.run(&["list"]);
    assert!(list.status.success());
    assert_eq!(
        String::from_utf8_lossy(&list.stderr),
        "3 new notifications; run shoal notifications\n"
    );
    let scoped = fixture
        .command()
        .args([
            "exec",
            "holder",
            "--",
            env!("CARGO_BIN_EXE_shoal"),
            "notifications",
        ])
        .output()
        .unwrap();
    assert!(!scoped.status.success());

    // A limited listing shows the oldest new entries and says how many remain.
    let first = fixture
        .command()
        .args(["--json", "notifications", "--limit", "1"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&first.stderr),
        "2 more new notifications; run shoal notifications again\n"
    );
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first[0]["kind"], "resource_busy", "{first}");
    let shown = fixture.ok(&["notifications"]);
    assert_eq!(shown.as_array().unwrap().len(), 2, "{shown}");
    let mut shown = shown.as_array().unwrap().clone();
    shown.insert(0, first[0].clone());
    let shown = Value::Array(shown);
    let summary: Vec<(&str, &str, &str)> = shown
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["workspace"].as_str().unwrap(),
                n["kind"].as_str().unwrap(),
                n["message"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(summary.len(), 3, "{shown}");
    assert_eq!(
        summary[0],
        (
            "waiter",
            "resource_busy",
            "no compatible capacity for any resource in pool lock; held by holder"
        )
    );
    assert_eq!(
        (summary[1].0, summary[1].1),
        ("waiter", "port_conflict"),
        "{shown}"
    );
    assert!(
        summary[1]
            .2
            .starts_with(&format!("port web: {taken} is in use; reserved ")),
        "{shown}"
    );
    assert_eq!(
        summary[2],
        ("waiter", "agent_exited", "claude exited with code 7")
    );
    assert!(shown.as_array().unwrap().iter().all(|n| n["read"] == false));
    // Shown once. `--all` still has them, now read; the text form has the time first.
    assert_eq!(fixture.ok(&["notifications"]), serde_json::json!([]));
    let all = fixture.ok(&["notifications", "--all", "--limit", "2"]);
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert!(all.as_array().unwrap().iter().all(|n| n["read"] == true));
    assert_eq!(fixture.run(&["list"]).stderr, b"");
    let text = fixture.run(&["notifications", "--all"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.lines()
            .all(|line| line.contains("  waiter  ") && line.as_bytes()[4] == b'-'),
        "{text}"
    );
    // Once read, the same conflict is news again.
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "lock", "waiter"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(fixture.ok(&["notifications"]).as_array().unwrap().len(), 1);

    // A follower prints the daemon's removals as they happen and marks them read.
    let mut follower = fixture
        .command()
        .args(["--json", "notifications", "--follow"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = follower.stdout.take().unwrap();
    let (sender, lines) = std::sync::mpsc::channel();
    thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    fixture.ok(&["pr", "merged", "waiter"]);
    wait_removed(&fixture, "waiter");
    let line = lines
        .recv_timeout(Duration::from_secs(15))
        .expect("followed notification");
    let notification: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(notification["workspace"], "waiter");
    assert_eq!(notification["kind"], "workspace_removed");
    assert_eq!(
        notification["message"],
        "removed after the merge acknowledgement"
    );
    follower.kill().unwrap();
    follower.wait().unwrap();
    assert_eq!(fixture.ok(&["notifications"]), serde_json::json!([]));
    assert_eq!(
        fixture.ok(&["daemon", "status"])["daemon"]["unread_notifications"],
        0
    );
}

#[test]
fn git_profiles_layer_and_isolate_worktree_settings_before_setup() {
    let fixture = Fixture::with_config(Some(
        "git_profile = 'personal'\n[git.profiles.personal]\nuser.email = 'personal@example.invalid'\n\
         [git.profiles.work]\nuser.name = 'Work Name'\nuser.email = 'work@example.invalid'\ncommit.gpgsign = false\n",
    ));
    git(
        &fixture.repo,
        &["config", "user.email", "main@example.invalid"],
    );
    let personal = fixture.add("personal");
    let personal_path = Path::new(personal["path"].as_str().unwrap());
    assert_eq!(
        git(personal_path, &["config", "user.email"]).trim(),
        "personal@example.invalid"
    );
    fs::write(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\ngit config user.email > setup-email\n",
    )
    .unwrap();
    fs::set_permissions(
        fixture.repo.join("setup.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_resource_config(
        &fixture.repo,
        "git_profile = 'work'\nsetup_cmd = 'setup.sh'\n",
    );
    let work = fixture.add("work");
    let work_path = Path::new(work["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(work_path.join("setup-email"))
            .unwrap()
            .trim(),
        "work@example.invalid"
    );
    assert_eq!(
        git(work_path, &["config", "--worktree", "commit.gpgsign"]).trim(),
        "false"
    );
    assert_eq!(
        git(&fixture.repo, &["config", "user.email"]).trim(),
        "main@example.invalid"
    );
    assert_eq!(
        git(personal_path, &["config", "user.email"]).trim(),
        "personal@example.invalid"
    );
    let saved = fixture.root.path().join("local.toml");
    fs::write(&saved, "git_profile = 'personal'\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    git(&fixture.repo, &["branch", "existing"]);
    let existing = fixture.ok(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "existing",
    ]);
    let existing_path = Path::new(existing["path"].as_str().unwrap());
    assert_eq!(
        git(existing_path, &["config", "user.email"]).trim(),
        "personal@example.invalid"
    );
    // Reopening neither reapplies a changed profile nor runs setup again.
    git(
        existing_path,
        &[
            "config",
            "--worktree",
            "user.email",
            "edited@example.invalid",
        ],
    );
    fixture.ok(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "existing",
    ]);
    assert_eq!(
        git(existing_path, &["config", "user.email"]).trim(),
        "edited@example.invalid"
    );
}

#[test]
fn git_profiles_leave_unselected_repositories_alone_and_retain_failed_workspaces() {
    let fixture = Fixture::new();
    let plain = fixture.add("plain");
    assert!(
        !Path::new(plain["git_dir"].as_str().unwrap())
            .join("config.worktree")
            .exists()
    );
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "config",
                "--default",
                "false",
                "--get",
                "extensions.worktreeConfig"
            ]
        )
        .trim(),
        "false"
    );
    commit_resource_config(&fixture.repo, "git_profile = 'missing'\n");
    let output = fixture.run(&["add", fixture.repo.to_str().unwrap(), "unknown"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("git profile missing is not defined"));
    let failed = fixture.ok(&["inspect", "unknown"]);
    assert_eq!(failed["workspace"]["state"], "failed");
    assert!(Path::new(failed["workspace"]["path"].as_str().unwrap()).is_dir());
    fixture.ok(&["rm", "unknown", "--yes"]);
}

#[test]
fn git_profile_flag_overrides_config_for_new_and_existing_branches() {
    let fixture = Fixture::with_config(Some(
        "[git.profiles.manual]\nuser.email = 'manual@example.invalid'\n",
    ));
    commit_resource_config(&fixture.repo, "git_profile = 'missing'\n");
    git(&fixture.repo, &["branch", "existing-profile"]);
    for (existing, name) in [(false, "new-profile"), (true, "existing-profile")] {
        let mut args = vec!["add", fixture.repo.to_str().unwrap()];
        if existing {
            args.push("--existing");
        }
        args.extend([name, "--git-profile", "manual"]);
        let workspace = fixture.ok(&args);
        let path = Path::new(workspace["path"].as_str().unwrap());
        assert_eq!(
            git(path, &["config", "user.email"]).trim(),
            "manual@example.invalid"
        );
        let output = fixture.run(&[
            "add",
            fixture.repo.to_str().unwrap(),
            "--existing",
            name,
            "--git-profile",
            "manual",
        ]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("applies only to new worktrees"));
        assert_eq!(
            git(path, &["config", "user.email"]).trim(),
            "manual@example.invalid"
        );
    }
    let before = fixture.ok(&["list"]);
    let output = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "bad-profile",
        "--git-profile",
        "unknown",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("git profile unknown is not defined"));
    assert_eq!(fixture.ok(&["list"]), before);
    assert!(git(&fixture.repo, &["branch", "--list", "bad-profile"]).is_empty());
}

#[test]
fn configured_commands_preserve_arguments_scope_and_exit_status() {
    let fixture = Fixture::with_config(Some(
        "[commands]\ncheck = ['sh', '-c', 'cat; printf \"%s\\n\" \"$SHOAL_WORKSPACE\" \"$@\"; test -n \"$SHOAL_SCOPE_TOKEN\" || exit 99; exit 7', 'check', 'literal $HOME']\n",
    ));
    let workspace = fixture.ok(&["add", fixture.repo.to_str().unwrap(), "custom"]);
    let path = workspace["path"].as_str().unwrap();
    let mut child = fixture
        .command()
        .current_dir(path)
        .args(["check", "--", "two words", "--flag"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"pipe:").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"pipe:custom\nliteral $HOME\ntwo words\n--flag\n"
    );
    assert_eq!(
        fixture.ok(&["inspect", "custom"])["executions"],
        serde_json::json!([])
    );
    fs::write(
        Path::new(path).join(".shoal.toml"),
        "[commands]\ncheck = ['printf', '%s', 'from worktree']\n",
    )
    .unwrap();
    assert_eq!(fixture.run(&["check", "custom"]).stdout, b"from worktree");
    let saved = fixture.root.path().join("commands.toml");
    fs::write(&saved, "[commands]\ncheck = ['printf', '%s', 'saved']\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    assert_eq!(fixture.run(&["check", "custom"]).stdout, b"saved");
}

#[test]
fn run_lists_command_layers_and_executes_names_that_collide_with_built_ins() {
    let fixture = Fixture::with_config(Some(
        "[commands]\nglobal = ['printf', '%s', 'global command']\nshadowed = ['global']\nlist = ['printf', '%s', 'configured list']\nclaude = ['global-claude']\n",
    ));
    let workspace = fixture.add("run-command");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[commands]\nshadowed = ['worktree']\nworktree = ['worktree-only']\n",
    )
    .unwrap();
    let saved = fixture.root.path().join("run-commands.toml");
    fs::write(
        &saved,
        "[commands]\nshadowed = ['saved']\nsaved = ['saved-only']\n",
    )
    .unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);

    let output = fixture.run(&["run", "list", "run-command"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"configured list");
    assert!(fixture.ok(&["list"]).is_array());

    let output = fixture
        .command()
        .current_dir(path)
        .args(["--json", "run"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listed: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    let command = |name: &str| listed.iter().find(|entry| entry["name"] == name).unwrap();
    assert_eq!(command("codex")["layer"], "built_in_default");
    assert_eq!(command("codex")["bare_name"], "built_in");
    assert_eq!(command("claude")["layer"], "global_config");
    assert_eq!(command("global")["bare_name"], "shorthand");
    assert_eq!(command("list")["bare_name"], "built_in");
    assert_eq!(command("worktree")["layer"], "worktree_file");
    assert_eq!(command("saved")["layer"], "saved_repository_config");
    assert_eq!(command("shadowed")["argv"], serde_json::json!(["saved"]));
    assert_eq!(command("shadowed")["layer"], "saved_repository_config");
}

#[test]
fn cli_agent_command_defaults_can_be_replaced_at_launch() {
    let fixture = Fixture::new();
    let workspace = fixture.ok(&["add", fixture.repo.to_str().unwrap(), "configured-agent"]);
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join(".shoal.toml"),
        "[commands]\nclaude = ['printf', '%s\\n', '{workspace}', '{args}', '{branch}', '{path}', 'prompt={prompt}']\ncodex = ['printf', '%s\\n', '{args}', 'custom default', 'prompt={prompt}']\n"
    ).unwrap();
    let output = fixture.run(&["claude", "configured-agent", "--", "{path}", "two words"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "configured-agent\n{{path}}\ntwo words\nconfigured-agent\n{}\nprompt=\n",
            path.display()
        )
    );
    let output = fixture.run(&[
        "run",
        "claude",
        "configured-agent",
        "--",
        "{path}",
        "two words",
    ]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "configured-agent\n{{path}}\ntwo words\nconfigured-agent\n{}\nprompt=\n",
            path.display()
        )
    );
    let output = fixture.run(&[
        "codex",
        "configured-agent",
        "--cli",
        "--",
        "--model",
        "example",
    ]);
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"--model\nexample\ncustom default\nprompt=\n"
    );
    let output = fixture.run(&[
        "run",
        "codex",
        "configured-agent",
        "--",
        "--model",
        "example",
    ]);
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"--model\nexample\ncustom default\nprompt=\n"
    );
}

#[test]
fn unknown_commands_never_open_the_workspace_picker() {
    let fixture = Fixture::new();
    let workspace = fixture.add("command-target");
    // Noninteractive selection used to fail at the picker instead of name resolution.
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args(["lsit"])
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        error.contains("lsit") && error.contains("no current workspace"),
        "{error}"
    );
    assert!(!error.contains("non-interactive"), "{error}");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[commands]\nlocal-check = ['printf', '%s', 'repository command']\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .current_dir(path)
        .arg("local-check")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"repository command");
    assert_eq!(
        fixture.run(&["local-check", "command-target"]).stdout,
        b"repository command"
    );
    let output = fixture
        .command()
        .current_dir(path)
        .arg("lsit")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown command"));
}

#[test]
fn agent_auth_wrappers_are_inherited_without_changing_ordinary_executions() {
    let fixture = Fixture::new();
    let workspace = fixture.add("agent-auth");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    let wrappers = fixture.root.path().join("wrappers with spaces");
    fs::create_dir(&wrappers).unwrap();
    for tool in ["fj", "gh"] {
        let path = wrappers.join(tool);
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' 'agent {tool}' \"$@\"\n"),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config = format!(
        "[commands]\nclaude = ['sh', '-c', 'fj \"two words\" \"$literal\"; gh auth status; printf \"%s\\n\" \"$HOME\"; \"$SHOAL_TEST_BINARY\" exec -- fj nested']\n[agent_auth]\nfj = {:?}\ngh = {:?}\n",
        wrappers.join("fj"),
        wrappers.join("gh")
    );
    fs::write(worktree.join(".shoal.toml"), config).unwrap();
    let report = fixture.ok(&["config", "show", "agent-auth"]);
    let fj = report
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "agent_auth.fj")
        .unwrap();
    assert_eq!(fj["value"], wrappers.join("fj").to_str().unwrap());
    assert_eq!(fj["layer"], "worktree_file");
    let output = fixture
        .command()
        .env("SHOAL_TEST_BINARY", env!("CARGO_BIN_EXE_shoal"))
        .env("literal", "$() ; ' literal")
        .args(["claude", "agent-auth"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "agent fj\ntwo words\n$() ; ' literal\nagent gh\nauth\nstatus\n{}\nagent fj\nnested\n",
            fixture.root.path().display()
        )
    );
    let expected = fixture
        .command()
        .get_envs()
        .find(|(key, _)| *key == "PATH")
        .unwrap()
        .1
        .unwrap()
        .to_owned();
    let output = fixture.run(&["exec", "agent-auth", "--", "printenv", "PATH"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim_end(),
        expected.to_str().unwrap()
    );
    assert!(
        fs::read_dir(fixture.root.path().join("state"))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("agent-auth-"))
    );
}

#[test]
fn detached_agents_use_auth_wrappers_and_invalid_wrappers_prevent_launch() {
    let fixture = Fixture::new();
    let workspace = fixture.add("detached-auth");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    let wrapper = fixture.root.path().join("fj-agent");
    fs::write(
        &wrapper,
        "#!/bin/sh\nprintf '%s\\n' 'detached agent' \"$@\"\nexit 23\n",
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        worktree.join(".shoal.toml"),
        "[agent_auth]\nfj = '~/fj-agent'\n",
    )
    .unwrap();
    let log = fixture.root.path().join("agent.log");
    let output = fixture.run(&[
        "detached-internal",
        "detached-auth",
        "--log",
        log.to_str().unwrap(),
        "--agent",
        "happy claude",
        "--",
        "fj",
        "two words",
    ]);
    assert_eq!(
        output.status.code(),
        Some(23),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(&log)
            .unwrap()
            .contains("detached agent\ntwo words\n")
    );
    fs::remove_file(&wrapper).unwrap();
    let output = fixture.run(&["claude", "detached-auth"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("agent_auth.fj"));
    let status = fixture.ok(&["inspect", "detached-auth"]);
    assert!(
        status["executions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["state"] != "running")
    );
}

#[test]
fn one_off_workspace_paths_preserve_repository_defaults_and_ownership() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let destination = fixture
        .root
        .path()
        .join("elsewhere/one off ' {{ branch }} path");
    let path = destination.to_str().unwrap();
    let workspace = fixture.ok(&["add", repo, "custom/topic", "--path", path]);
    assert_eq!(
        Path::new(workspace["path"].as_str().unwrap()),
        fs::canonicalize(path).unwrap()
    );
    assert_eq!(workspace["name"], "custom-topic");
    assert_eq!(
        fixture.ok(&["add", repo, "--existing", "custom/topic", "--path", path])["id"],
        workspace["id"]
    );
    assert!(
        !fixture
            .run(&[
                "add",
                repo,
                "--existing",
                "custom/topic",
                "--path",
                fixture.root.path().join("different").to_str().unwrap()
            ])
            .status
            .success()
    );
    let ordinary = fixture.add("ordinary");
    assert!(Path::new(ordinary["path"].as_str().unwrap()).starts_with(fixture.shoal_dir()));
    fixture.ok(&["rm", "custom-topic", "--keep-branch", "--yes"]);
    assert!(!destination.exists());
    assert!(destination.parent().unwrap().exists());
    git(&fixture.repo, &["branch", "existing-path"]);
    fixture.ok(&["add", repo, "--existing", "existing-path", "--path", path]);
}

#[test]
fn one_off_workspace_paths_reject_existing_and_protected_directories() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let owned = fixture.add("owned");
    let owned = Path::new(owned["path"].as_str().unwrap());
    let alias = fixture.root.path().join("alias");
    symlink(&fixture.repo, &alias).unwrap();
    let occupied = fixture.root.path().join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep"), "precious").unwrap();
    for path in [
        occupied.clone(),
        fixture.repo.join("nested"),
        alias.join("nested"),
        fixture.root.path().join("state/nested"),
        owned.join("nested"),
        owned.parent().unwrap().to_path_buf(),
        fixture.root.path().to_path_buf(),
    ] {
        let output = fixture.run(&["add", repo, "rejected", "--path", path.to_str().unwrap()]);
        assert!(!output.status.success(), "accepted {}", path.display());
    }
    assert_eq!(
        fs::read_to_string(occupied.join("keep")).unwrap(),
        "precious"
    );
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
    assert!(!git(&fixture.repo, &["branch", "--list", "rejected"]).contains("rejected"));
}

#[test]
fn adopt_cli_preserves_work_and_uses_normal_lifecycle() {
    let mut fixture = Fixture::new();
    let path = fixture.root.path().join("existing worktree");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "adopt/topic",
            path.to_str().unwrap(),
        ],
    );
    fs::write(path.join("tracked"), "work in progress\n").unwrap();
    fs::write(
        path.join(".shoal.toml"),
        "setup_cmd = 'missing'\npost_setup_cmd = 'missing'\ngit_profile = 'missing'\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args([
            "--json",
            "adopt",
            fixture.repo.to_str().unwrap(),
            "existing worktree",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let w: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(w["name"], "adopt-topic");
    assert_eq!(w["state"], "ready");
    fixture.restart();
    let again = fixture.ok(&[
        "adopt",
        fixture.repo.to_str().unwrap(),
        "~/existing worktree",
    ]);
    assert_eq!(again["id"], w["id"]);
    let output = fixture.run(&["exec", "adopt-topic", "--", "cat", "tracked"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "work in progress\n"
    );
    let diff = fixture.run(&["diff", "adopt-topic"]);
    assert!(diff.status.success());
    assert!(String::from_utf8_lossy(&diff.stdout).contains("+work in progress"));
    // Full ownership includes resource release and repository removal of this external path.
    fixture.ok(&["port", "acquire", "test", "adopt-topic"]);
    fixture.ok(&["repo", "rm", fixture.repo.to_str().unwrap(), "--yes"]);
    assert!(!path.exists());
    assert!(fixture.ok(&["list"]).as_array().unwrap().is_empty());
}

#[test]
fn explicit_locations_do_not_require_unrelated_checkouts_to_be_readable() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let offline = fixture.root.path().join("offline-repo");
    fs::create_dir(&offline).unwrap();
    git(&offline, &["init", "-b", "main"]);
    let registration = fixture.ok(&["repo", "add", offline.to_str().unwrap()]);
    fs::remove_dir_all(&offline).unwrap();
    let destination = fixture.root.path().join("custom");
    fixture.ok(&[
        "add",
        repo,
        "custom",
        "--path",
        destination.to_str().unwrap(),
    ]);
    let adopted = fixture.root.path().join("adopted");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "adopted",
            adopted.to_str().unwrap(),
        ],
    );
    fixture.ok(&["adopt", repo, adopted.to_str().unwrap()]);
    for protected in [
        offline.join("nested"),
        Path::new(registration["workspaces_dir"].as_str().unwrap()).join("nested"),
    ] {
        assert!(
            !fixture
                .run(&[
                    "add",
                    repo,
                    "rejected",
                    "--path",
                    protected.to_str().unwrap()
                ])
                .status
                .success()
        );
    }
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 2);
}

#[test]
fn custom_agents_launch_with_layered_prompts_scope_and_notifications() {
    let fixture = Fixture::with_config(Some(
        "default_agent = 'pi'\nagent_template = 'Follow {branch}'\nissue_template = '{title}: {body}'\n[commands]\npi = ['missing-global-launcher']\n",
    ));
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    for (name, script) in [
        (
            "gh",
            "#!/bin/sh\nprintf '%s' '{\"number\":37,\"title\":\"Literal {branch}\",\"body\":\"$(false)\"}'\n",
        ),
        (
            "fake-agent",
            "#!/bin/sh\ntest -n \"$SHOAL_SCOPE_TOKEN\" || exit 99\nprintf '%s\\0' \"$@\" > \"$HOME/agent-args\"\nexit 7\n",
        ),
    ] {
        let path = bin.join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fixture.add_github_origin();
    let saved = fixture.root.path().join("saved.toml");
    fs::write(
        &saved,
        "[commands]\npi = ['fake-agent', '--prompt={prompt}', '{args}', '{branch}']\n",
    )
    .unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    let output = fixture.run(&[
        "--json",
        "issue",
        "37",
        "--repo",
        fixture.repo.to_str().unwrap(),
        "--base",
        "HEAD",
        "--",
        "literal {prompt}",
    ]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let workspace: Value =
        serde_json::from_slice(output.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
    let branch = workspace["branch"].as_str().unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("agent-args")).unwrap(),
        format!(
            "--prompt=Follow {branch}\n\nLiteral {{branch}}: $(false)\0literal {{prompt}}\0{branch}\0"
        )
    );
    assert_eq!(
        fixture.ok(&["inspect", workspace["id"].as_str().unwrap()])["executions"],
        serde_json::json!([])
    );
    let notifications = fixture.ok(&["notifications"]);
    assert!(notifications.to_string().contains("pi exited with code 7"));
    assert!(!fixture.root.path().join(".claude.json").exists());
    assert!(!fixture.root.path().join(".codex/config.toml").exists());

    let output = fixture.run(&[
        "run",
        "pi",
        workspace["id"].as_str().unwrap(),
        "--",
        "literal {prompt}",
    ]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("agent-args")).unwrap(),
        format!("--prompt=\0literal {{prompt}}\0{branch}\0")
    );

    // Without an explicit prompt slot, context precedes literal forwarded arguments.
    fs::write(&saved, "[commands]\npi = ['fake-agent', '{args}']\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    let output = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "custom-add",
        "--base",
        "HEAD",
        "--agent",
        "pi",
        "--",
        "user message",
    ]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("agent-args")).unwrap(),
        "Follow custom-add\0user message\0"
    );
    let before = fixture.ok(&["list"]);
    let unknown = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "unknown-agent",
        "--agent",
        "typo",
    ]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown agent"));
    assert_eq!(fixture.ok(&["list"]), before);
}

#[test]
fn resource_hooks_retain_leases_on_failure_and_run_during_removal() {
    let mut fixture = Fixture::new();
    fs::write(
        fixture.repo.join("permit.sh"),
        r#"#!/bin/sh
set -eu
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
test "$PWD" = "$SHOAL_WORKSPACE_PATH"
printf '%s\n' "$SHOAL_RESOURCE_LEASE" >> "$HOME/$SHOAL_HOOK"
test ! -f "$SHOAL_HOOK-fails" || { echo 'resource busy' >&2; exit 3; }
"#,
    )
    .unwrap();
    fs::set_permissions(
        fixture.repo.join("permit.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_resource_config(
        &fixture.repo,
        "post_resource_acquire_cmd = 'permit.sh'\npre_resource_release_cmd = 'permit.sh'\n[resources.signing]\n",
    );
    let workspace = fixture.add("hooked");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fixture.add("waiter");
    fs::write(path.join("post_resource_acquire-fails"), "").unwrap();
    let output = fixture.run(&[
        "resource",
        "acquire",
        "signing",
        "hooked",
        "--reason",
        "test signing",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("resource lease retained"));
    let inspection = fixture.ok(&["inspect", "hooked"]);
    let lease = &inspection["resources"][0];
    assert_eq!(lease["reason"], "test signing");
    assert_eq!(inspection["executions"], serde_json::json!([]));
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "signing", "waiter"])
            .status
            .code(),
        Some(2)
    );
    fixture.restart();
    fs::remove_file(path.join("post_resource_acquire-fails")).unwrap();
    assert_eq!(
        &fixture.ok(&["resource", "acquire", "signing", "hooked"]),
        lease
    );
    let events = fs::read_to_string(fixture.root.path().join("post_resource_acquire")).unwrap();
    let events: Vec<Value> = events
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events, vec![lease.clone(), lease.clone()]);
    fs::write(path.join("pre_resource_release-fails"), "").unwrap();
    for args in [
        vec!["resource", "release", "signing", "hooked"],
        vec!["rm", "hooked", "--yes", "--delete-branch"],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("resource busy"));
        assert_eq!(fixture.ok(&["inspect", "hooked"])["resources"][0], *lease);
        assert!(path.exists());
    }
    fs::remove_file(path.join("pre_resource_release-fails")).unwrap();
    // Removing the definition does not prevent release of an existing lease.
    fs::write(
        path.join(".shoal.toml"),
        "pre_resource_release_cmd = 'permit.sh'\n",
    )
    .unwrap();
    fixture.ok(&["resource", "release", "signing", "hooked"]);
    fixture.ok(&["resource", "acquire", "signing", "waiter"]);
    fixture.ok(&["rm", "waiter", "--yes", "--delete-branch"]);
    let events = fs::read_to_string(fixture.root.path().join("pre_resource_release")).unwrap();
    assert_eq!(events.lines().count(), 4);
    fixture.ok(&["rm", "hooked", "--yes", "--delete-branch"]);
}

#[test]
fn resource_hooks_exclude_concurrent_release_setup_and_removal() {
    let fixture = Fixture::new();
    fs::write(
        fixture.repo.join("permit.sh"),
        r#"#!/bin/sh
set -eu
touch "$HOME/hook-entered"
while test ! -f "$HOME/hook-continue"; do sleep 0.05; done
"#,
    )
    .unwrap();
    fs::set_permissions(
        fixture.repo.join("permit.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_resource_config(
        &fixture.repo,
        "post_resource_acquire_cmd = 'permit.sh'\nsetup_cmd = '/usr/bin/true'\n[resources.signing]\n",
    );
    fixture.add("hooked");
    let acquire = fixture
        .command()
        .args(["resource", "acquire", "signing", "hooked"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("permit hook", || {
        fixture.root.path().join("hook-entered").exists()
    });
    for args in [
        vec!["resource", "acquire", "signing", "hooked"],
        vec!["resource", "release", "signing", "hooked"],
        vec!["setup", "hooked"],
        vec!["rm", "hooked", "--yes", "--delete-branch"],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("resource operation is in progress"),
            "{output:?}"
        );
    }
    fs::write(fixture.root.path().join("hook-continue"), "").unwrap();
    assert!(acquire.wait_with_output().unwrap().status.success());
    fixture.ok(&["rm", "hooked", "--yes", "--delete-branch"]);
}

#[test]
fn workspace_hook_resolution_preserves_layers_and_directories() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixStream,
    };
    let mut fixture = Fixture::with_config(Some(""));
    let workspace = fixture.add("hooks");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    // Global hooks are installed only after creation, so these paths need not
    // exist: resolution must not run a hook or require its executable yet.
    fs::write(
        fixture.root.path().join(".config/shoal/config.toml"),
        "pre_setup_cmd = 'global'\npost_remove_cmd = 'global'\n\
         post_resource_acquire_cmd = 'global'\npre_resource_release_cmd = 'global'\n",
    )
    .unwrap();
    fixture.restart();
    let call = |request: Value| {
        let mut socket =
            UnixStream::connect(fixture.root.path().join("state/daemon.sock")).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(socket, "{request}").unwrap();
        let mut line = String::new();
        BufReader::new(socket).read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let protocol =
        call(serde_json::json!({"protocol":0,"id":1,"method":"status"}))["protocol"].clone();
    let resolve = |kind: &str| {
        call(serde_json::json!({"protocol":protocol,"id":2,
            "method":{"workspace_hook":{"workspace":workspace["id"],"kind":kind}}}))
    };
    let saved = fixture.root.path().join("saved.toml");
    let save = |text: &str| {
        fs::write(&saved, text).unwrap();
        fixture.ok(&[
            "repo",
            "config",
            fixture.repo.to_str().unwrap(),
            "--file",
            saved.to_str().unwrap(),
        ]);
    };
    for (kind, global) in [
        ("setup", false),
        ("pre_setup", true),
        ("post_setup", false),
        ("pre_remove", false),
        ("post_remove", true),
        ("post_resource_acquire", true),
        ("pre_resource_release", true),
    ] {
        let directory = if kind == "post_remove" {
            &fixture.repo
        } else {
            worktree
        };
        let file = worktree.join(".shoal.toml");
        fs::write(&file, "").unwrap();
        save("");
        let result = resolve(kind);
        assert_eq!(result["type"], "hook", "{result}");
        assert_eq!(
            result["data"],
            if global {
                serde_json::json!(directory.join("global"))
            } else {
                Value::Null
            },
            "{kind}"
        );

        fs::write(&file, format!("{kind}_cmd = 'file hook'\n")).unwrap();
        assert_eq!(
            resolve(kind)["data"],
            serde_json::json!(directory.join("file hook"))
        );
        // An unrelated saved option must not mask the worktree's hook.
        save("default_agent = 'claude'\n");
        assert_eq!(
            resolve(kind)["data"],
            serde_json::json!(directory.join("file hook"))
        );
        save(&format!("{kind}_cmd = 'saved hook'\n"));
        assert_eq!(
            resolve(kind)["data"],
            serde_json::json!(directory.join("saved hook"))
        );
        save(&format!("{kind}_cmd = '/absolute/hook'\n"));
        assert_eq!(resolve(kind)["data"], "/absolute/hook");
    }
}

#[test]
fn pre_setup_hook_gates_readiness_and_supports_global_defaults() {
    let fixture = Fixture::with_config(Some("pre_setup_cmd = 'before.sh'\n"));
    fs::write(
        fixture.repo.join("before.sh"),
        r#"#!/bin/sh
set -eu
test "$SHOAL_HOOK" = pre_setup
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
echo before >> "$HOME/setup-order"
test ! -f "$HOME/fail-before" || { echo 'prepare failed' >&2; exit 7; }
"#,
    )
    .unwrap();
    fs::write(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\necho setup >> \"$HOME/setup-order\"\n",
    )
    .unwrap();
    for name in ["before.sh", "setup.sh"] {
        fs::set_permissions(fixture.repo.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    commit_resource_config(&fixture.repo, "setup_cmd = 'setup.sh'\n");
    let home = fixture.root.path();
    fs::write(home.join("fail-before"), "").unwrap();
    let output = fixture.run(&["add", fixture.repo.to_str().unwrap(), "hooked"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("pre_setup_cmd exited with 7"));
    let inspection = fixture.ok(&["inspect", "hooked"]);
    assert_eq!(inspection["workspace"]["state"], "failed");
    assert_eq!(inspection["executions"], serde_json::json!([]));
    assert_eq!(
        fs::read_to_string(home.join("setup-order")).unwrap(),
        "before\n"
    );
    fs::remove_file(home.join("fail-before")).unwrap();
    assert_eq!(fixture.ok(&["setup", "hooked"])["state"], "ready");
    assert_eq!(
        fs::read_to_string(home.join("setup-order")).unwrap(),
        "before\nbefore\nsetup\n"
    );
    let path = Path::new(inspection["workspace"]["path"].as_str().unwrap());
    // A repository override wins; a pre-setup hook alone still gates readiness.
    fs::write(
        path.join(".shoal.toml"),
        "pre_setup_cmd = '/usr/bin/true'\n",
    )
    .unwrap();
    assert_eq!(fixture.ok(&["setup", "hooked"])["state"], "ready");
    fs::write(fixture.repo.join(".shoal.toml"), "").unwrap();
    git(&fixture.repo, &["add", ".shoal.toml"]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "-m",
            "hook only",
        ],
    );
    assert_eq!(fixture.add("hook-only")["state"], "ready");
    assert_eq!(
        fs::read_to_string(home.join("setup-order")).unwrap(),
        "before\nbefore\nsetup\nbefore\n"
    );
    fixture.ok(&["rm", "hooked", "--yes", "--delete-branch"]);
    fixture.ok(&["rm", "hook-only", "--yes", "--delete-branch"]);
}

#[test]
fn post_remove_hook_uses_checkout_and_reports_failure_after_removal() {
    let fixture = Fixture::with_config(Some("post_remove_cmd = '/usr/bin/false'\n"));
    fs::write(
        fixture.repo.join("after removal.sh"),
        r#"#!/bin/sh
set -eu
test "$SHOAL_HOOK" = post_remove
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
test ! -e "$SHOAL_WORKSPACE_PATH"
printf '%s\n%s\n%s\n' "$PWD" "$SHOAL_WORKSPACE" "$SHOAL_WORKSPACE_PATH" >> "$HOME/removed"
test ! -f "$HOME/fail-after" || { echo 'external cleanup failed' >&2; exit 8; }
"#,
    )
    .unwrap();
    fs::set_permissions(
        fixture.repo.join("after removal.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_resource_config(
        &fixture.repo,
        "post_remove_cmd = 'after removal.sh'\n[resources.signing]\n",
    );
    let workspace = fixture.add("hooked");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fixture.ok(&["resource", "acquire", "signing", "hooked"]);
    let config = path.join(".shoal.toml");
    fs::write(
        &config,
        "post_remove_cmd = 'after removal.sh'\npre_remove_cmd = '/usr/bin/false'\n",
    )
    .unwrap();
    assert!(
        !fixture
            .run(&["rm", "hooked", "--yes", "--delete-branch"])
            .status
            .success()
    );
    assert!(!fixture.root.path().join("removed").exists());
    fs::write(&config, "post_remove_cmd = 'after removal.sh'\n").unwrap();
    fs::write(fixture.root.path().join("fail-after"), "").unwrap();
    let result = fixture.ok(&["rm", "hooked", "--yes", "--delete-branch"]);
    assert_eq!(result["removed"], true);
    assert!(
        result["hook_error"]
            .as_str()
            .unwrap()
            .contains("external cleanup failed")
    );
    assert!(!fixture.run(&["inspect", "hooked"]).status.success());
    let notifications = fixture.ok(&["notifications"]);
    assert!(
        notifications
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "hook_failed" && n["workspace"] == "hooked")
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("removed")).unwrap(),
        format!("{}\nhooked\n{}\n", fixture.repo.display(), path.display())
    );
    fs::remove_file(fixture.root.path().join("fail-after")).unwrap();
    fixture.add("next");
    fixture.ok(&["resource", "acquire", "signing", "next"]);
    let missing = fixture.add("missing");
    fs::remove_dir_all(missing["path"].as_str().unwrap()).unwrap();
    fixture.ok(&["rm", "missing", "--yes", "--keep-branch"]);
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("removed"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    // Repository removal invokes the hook before deleting its checkout.
    fixture.ok(&["repo", "rm", fixture.repo.to_str().unwrap(), "--yes"]);
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("removed"))
            .unwrap()
            .lines()
            .count(),
        6
    );
}

fn scoped_command(fixture: &Fixture, workspace: &str, args: &[&str]) -> Output {
    fixture
        .command()
        .args([
            "exec",
            workspace,
            "--",
            env!("CARGO_BIN_EXE_shoal"),
            "--json",
        ])
        .args(args)
        .output()
        .unwrap()
}

fn pending_access(output: Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["code"], "approval_pending");
    value["request"].clone()
}

#[test]
fn resource_approvals_require_unscoped_decisions_and_preserve_capacity() {
    let mut fixture = Fixture::with_config(Some("[resources.signing]\nrequires_approval=true\n"));
    fixture.add("agent");
    fixture.add("human");
    let args = ["resource", "acquire", "signing", "--reason", "sign build"];
    let pending = pending_access(scoped_command(&fixture, "agent", &args));
    let id = pending["id"].as_str().unwrap();
    assert_eq!(pending["workspace"], "agent");
    assert_eq!(
        fixture.ok(&["resource", "agent"])["pools"][0]["resources"][0]["requires_approval"],
        true
    );
    assert_eq!(
        pending_access(scoped_command(&fixture, "agent", &args))["id"],
        id
    );
    assert_eq!(fixture.ok(&["access"])[0]["id"], id);
    assert!(
        fixture.ok(&["resource", "agent"])["leases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !scoped_command(&fixture, "agent", &["access", "approve", id])
            .status
            .success()
    );
    assert!(
        !scoped_command(&fixture, "agent", &["access", "deny", id])
            .status
            .success()
    );
    assert!(
        !scoped_command(&fixture, "human", &["access", "list", "agent"])
            .status
            .success()
    );
    let other: Value =
        serde_json::from_slice(&scoped_command(&fixture, "human", &["access"]).stdout).unwrap();
    assert!(other.as_array().unwrap().is_empty());
    fixture.ok(&["resource", "acquire", "signing", "human"]);
    fixture.ok(&["access", "approve", id]);
    fixture.restart();
    let busy: Value =
        serde_json::from_slice(&scoped_command(&fixture, "agent", &args).stdout).unwrap();
    assert_eq!(busy["code"], "resource_busy");
    fixture.ok(&["resource", "release", "signing", "human"]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    assert!(
        scoped_command(&fixture, "agent", &["resource", "release", "signing"])
            .status
            .success()
    );
    let next = pending_access(scoped_command(&fixture, "agent", &args));
    assert_ne!(next["id"], id);
    fixture.ok(&["access", "deny", next["id"].as_str().unwrap()]);
    let denied: Value =
        serde_json::from_slice(&scoped_command(&fixture, "agent", &args).stdout).unwrap();
    assert_eq!(denied["code"], "approval_denied");
    fixture.ok(&["resource", "release", "signing", "agent"]);
    assert!(fixture.ok(&["access"]).as_array().unwrap().is_empty());
}

#[test]
fn workspace_approvals_survive_release_but_do_not_expand_access_modes() {
    let mut fixture = Fixture::with_config(Some(
        "[resources.cache]\nkind='rwlock'\nrequires_approval=true\napproval_lifetime='workspace'\n",
    ));
    fixture.add("agent");
    let args = [
        "resource",
        "acquire",
        "cache",
        "--mode",
        "read",
        "--reason",
        "inspect cache",
    ];
    let pending = pending_access(scoped_command(&fixture, "agent", &args));
    fixture.ok(&["access", "approve", pending["id"].as_str().unwrap()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["resource", "release", "cache", "agent"]);
    fixture.restart();
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["resource", "release", "cache", "agent"]);
    let write = pending_access(scoped_command(
        &fixture,
        "agent",
        &[
            "resource",
            "acquire",
            "cache",
            "--mode",
            "write",
            "--reason",
            "rebuild cache",
        ],
    ));
    assert_ne!(write["id"], pending["id"]);
    fixture.ok(&["rm", "agent", "--yes"]);
    assert!(fixture.ok(&["access"]).as_array().unwrap().is_empty());
}

#[test]
fn port_approvals_bind_overrides_and_follow_both_lifetimes() {
    for lifetime in ["lease", "workspace"] {
        let mut fixture = Fixture::new();
        commit_resource_config(
            &fixture.repo,
            &format!("[ports.web]\nrequires_approval=true\napproval_lifetime='{lifetime}'\n"),
        );
        fixture.add("agent");
        let args = ["port", "acquire", "web", "--reason", "serve preview"];
        let pending = pending_access(scoped_command(&fixture, "agent", &args));
        assert!(
            fixture.ok(&["port", "agent"])["reserved"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let id = pending["id"].as_str().unwrap();
        fixture.ok(&["access", "approve", id]);
        assert!(
            !scoped_command(
                &fixture,
                "agent",
                &[
                    "port",
                    "acquire",
                    "web",
                    "--env",
                    "OTHER_PORT",
                    "--reason",
                    "serve preview"
                ]
            )
            .status
            .success()
        );
        fixture.restart();
        assert!(scoped_command(&fixture, "agent", &args).status.success());
        fixture.ok(&["port", "release", "web", "agent"]);
        let next = scoped_command(&fixture, "agent", &args);
        if lifetime == "workspace" {
            assert!(
                next.status.success(),
                "{}",
                String::from_utf8_lossy(&next.stderr)
            );
        } else {
            assert_ne!(pending_access(next)["id"], id);
        }
        fixture.ok(&["port", "release", "web", "agent"]);
    }
}

#[test]
#[cfg(target_os = "macos")]
fn simulator_approvals_precede_mutations_and_cannot_be_bypassed_by_device_args() {
    let config = SIM_CONFIG.replace(
        "[simulators.profiles.phone]",
        "[simulators.profiles.phone]\nrequires_approval=true\napproval_lifetime='workspace'",
    );
    let mut fixture = Fixture::with_tools(Some(&config), true);
    commit_resource_config(&fixture.repo, "[simulators]\nrequires_approval=false\n");
    fixture.add("agent");
    let args = ["sim", "acquire", "--reason", "test app"];
    let pending = pending_access(scoped_command(&fixture, "agent", &args));
    let explicit = [
        "sim",
        "acquire",
        "--device",
        "type.Phone",
        "--runtime",
        "iOS Test",
        "--reason",
        "test app",
    ];
    assert_eq!(
        pending_access(scoped_command(&fixture, "agent", &explicit))["id"],
        pending["id"]
    );
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    assert!(
        events
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).unwrap()[0] == "list")
    );
    assert!(!fixture.root.path().join("sim-devices.json").exists());
    fixture.ok(&["access", "approve", pending["id"].as_str().unwrap()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["sim", "release", "default", "agent"]);
    fixture.restart();
    assert!(
        scoped_command(&fixture, "agent", &explicit)
            .status
            .success()
    );
    fixture.ok(&["sim", "release", "default", "agent"]);
    let clean = pending_access(scoped_command(
        &fixture,
        "agent",
        &["sim", "acquire", "--clean", "--reason", "isolate app state"],
    ));
    assert_ne!(clean["id"], pending["id"]);
    fixture.ok(&["access", "deny", clean["id"].as_str().unwrap()]);
    let denied: Value = serde_json::from_slice(
        &scoped_command(
            &fixture,
            "agent",
            &["sim", "acquire", "--clean", "--reason", "isolate app state"],
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(denied["code"], "approval_denied");
    fixture.ok(&["sim", "release", "default", "agent"]);
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    assert!(!events.contains("\"erase\""));
    let child = fixture
        .command()
        .args([
            "exec",
            "agent",
            "--",
            env!("CARGO_BIN_EXE_shoal"),
            "--json",
            "sim",
            "acquire",
            "--clean",
            "--reason",
            "reset test data",
            "--wait",
            "10",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let id = loop {
        let requests = fixture.ok(&["access"]);
        if let Some(request) = requests
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["status"] == "pending")
        {
            break request["id"].as_str().unwrap().to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "waiting acquisition did not request approval"
        );
        thread::sleep(Duration::from_millis(20));
    };
    fixture.ok(&["access", "approve", &id]);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let history = fixture.ok(&["sim", "history", "agent"]);
    assert_eq!(history[0]["status"], "acquired");
    assert_eq!(history[0]["request"]["reason"], "reset test data");
    assert_eq!(history[0]["action"], "create");
}

#[test]
#[cfg(target_os = "macos")]
fn simulator_approval_defaults_layer_per_option_and_release_expires_lease_grants() {
    let config = SIM_CONFIG.replace(
        "[simulators]",
        "[simulators]\nrequires_approval=true\napproval_lifetime='workspace'",
    );
    let fixture = Fixture::with_tools(Some(&config), true);
    commit_resource_config(&fixture.repo, "[simulators]\napproval_lifetime='lease'\n");
    fixture.add("agent");
    let args = ["sim", "acquire", "--reason", "test app"];
    let pending = pending_access(scoped_command(&fixture, "agent", &args));
    assert_eq!(pending["lifetime"], "lease");
    fixture.ok(&["access", "approve", pending["id"].as_str().unwrap()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["sim", "release", "default", "agent"]);
    assert_ne!(
        pending_access(scoped_command(&fixture, "agent", &args))["id"],
        pending["id"]
    );
    fixture.ok(&["sim", "release", "default", "agent"]);
}

#[test]
fn tampered_resource_approvals_fail_instead_of_selecting_a_member() {
    let fixture = Fixture::with_config(Some("[resources.signing]\nrequires_approval=true\n"));
    fixture.add("agent");
    let args = ["resource", "acquire", "signing", "--reason", "sign build"];
    let pending = pending_access(scoped_command(&fixture, "agent", &args));
    fixture.ok(&["access", "approve", pending["id"].as_str().unwrap()]);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let mut record = pending.clone();
    record["status"] = "approved".into();
    record["specification"] = serde_json::json!({
        "preferred": null, "env": "PORT_WEB", "on_conflict": "suggest", "range": [3000, 3100]
    });
    let tamper = |record: &Value| {
        db.execute(
            "UPDATE access_requests SET record=?2 WHERE id=?1",
            rusqlite::params![pending["id"].as_str().unwrap(), record.to_string()],
        )
        .unwrap();
    };
    tamper(&record);
    let output = scoped_command(&fixture, "agent", &args);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not select a pool member"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    record["specification"] = serde_json::json!({"member": "signing"});
    tamper(&record);
    let output = scoped_command(&fixture, "agent", &args);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("has an invalid record"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fixture.ok(&["resource", "agent"])["leases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn releasing_resource_names_clears_requests_from_previous_pool_scopes() {
    let config = "[resources.signing]\nrequires_approval=true\n";
    let mut fixture = Fixture::with_config(Some(config));
    let workspace = fixture.add("agent");
    let args = ["resource", "acquire", "signing", "--reason", "sign build"];
    let previous = pending_access(scoped_command(&fixture, "agent", &args));
    fs::write(fixture.root.path().join(".config/shoal/config.toml"), "").unwrap();
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join(".shoal.toml"),
        config,
    )
    .unwrap();
    fixture.restart();
    let current = pending_access(scoped_command(&fixture, "agent", &args));
    assert_ne!(previous["target"], current["target"]);
    fixture.ok(&["access", "approve", current["id"].as_str().unwrap()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["resource", "release", "signing", "agent"]);
    assert!(fixture.ok(&["access"]).as_array().unwrap().is_empty());
    assert_ne!(
        pending_access(scoped_command(&fixture, "agent", &args))["id"],
        current["id"]
    );
}

// Fail one Git operation while leaving fixture setup and verification on real Git.
fn install_failing_git(fixture: &Fixture) {
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file() && path.metadata().unwrap().permissions().mode() & 0o111 != 0)
        .unwrap()
        .canonicalize()
        .unwrap();
    std::os::unix::fs::symlink(real_git, bin.join("real-git")).unwrap();
    fs::write(
        bin.join("git"),
        r#"#!/bin/sh
if [ -f "$HOME/git-failure" ]; then
    failure=$(cat "$HOME/git-failure")
    for arg; do
        if [ "$arg" = "$failure" ]; then
            echo "injected Git predicate failure" >&2
            exit 128
        fi
    done
fi
exec "$HOME/bin/real-git" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn daemon_git_predicate_failures_preserve_branches_and_workspaces() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    upstream_remote(&fixture);
    // Make the default's tree differ so removal must check ancestry.
    fs::write(path.join("tracked"), "preserved work\n").unwrap();
    git(path, &["add", "tracked"]);
    git(
        path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "work",
        ],
    );
    let before = git(&fixture.repo, &["rev-parse", "main"]);
    install_failing_git(&fixture);
    for (failure, args) in [
        ("--is-ancestor", vec!["land", "worker"]),
        (
            "show-ref",
            vec!["add", fixture.repo.to_str().unwrap(), "--existing", "main"],
        ),
        (
            "--is-ancestor",
            vec!["rm", "worker", "--yes", "--delete-branch"],
        ),
        ("show-ref", vec!["rm", "worker", "--yes", "--delete-branch"]),
    ] {
        fs::write(fixture.root.path().join("git-failure"), failure).unwrap();
        let output = fixture.run(&args);
        assert!(!output.status.success(), "{args:?}: {output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("injected Git predicate failure"),
            "{args:?}: {error}"
        );
        assert!(!error.contains("diverged"), "{error}");
        assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
        assert!(path.is_dir());
        assert_eq!(
            fs::read_to_string(path.join("tracked")).unwrap(),
            "preserved work\n"
        );
        assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
    }
}
