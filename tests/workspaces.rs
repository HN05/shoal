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
    root: TempDir,
    repo: PathBuf,
    daemon: Child,
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

    fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.run(&["daemon", "status"]).status.success() {
            assert!(self.daemon.try_wait().unwrap().is_none(), "daemon exited");
            assert!(Instant::now() < deadline, "daemon startup timeout");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn restart(&mut self) {
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
        self.ok(&["add", self.repo.to_str().unwrap(), "--name", name])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn cli(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_shoal"));
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
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

fn git(repo: &Path, args: &[&str]) -> String {
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
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
    fixture.ok(&["add", &url, "--name", "cloned"]);
    fixture.ok(&["rm", "cloned"]);
    assert!(Path::new(repo["path"].as_str().unwrap()).is_dir());
}

#[test]
fn registration_reuses_repositories_by_origin_across_paths_and_url_forms() {
    let fixture = Fixture::new();
    let original = fixture.ok(&["repo", "list"])[0].clone();
    let url = "https://example.invalid/team/project.git";
    let ssh_url = "git@example.invalid:team/project.git";
    git(&fixture.repo, &["remote", "add", "origin", url]);
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
    fixture.ok(&["add", ssh_url, "--name", "alias"]);
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
                "--name",
                "broken",
                "--ref",
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

    let divergent = fixture.add("divergent");
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
    fixture.ok(&["add", "project", "--name", "named"]);
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
        "reserve",
        "web",
        "first",
        "--reason",
        "Frontend dev server",
    ]);
    assert_eq!(web["env_var"], "SHOAL_PORT_WEB");
    assert_eq!(web["reason"], "Frontend dev server");
    assert_eq!(fixture.ok(&["port", "reserve", "web", "first"]), web);
    let port = web["port"].to_string();
    assert!(
        !fixture
            .run(&["port", "reserve", "web", "second", "--port", &port])
            .status
            .success()
    );
    let api = fixture.ok(&["port", "reserve", "api", "first", "--env", "API_PORT"]);
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
        fixture
            .ok(&["port", "list", "first"])
            .as_array()
            .unwrap()
            .len(),
        2
    );
    fixture.ok(&["rm", "first", "--yes", "--keep-branch"]);
    assert_eq!(
        fixture.ok(&["port", "list", "--all"]),
        serde_json::json!([])
    );
    fixture.ok(&["port", "reserve", "web", "second", "--port", &port]);
    fixture.ok(&["port", "release", "web", "second"]);
    assert_eq!(
        fixture.ok(&["port", "list", "second"]),
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
            .run(&["port", "reserve", "occupied", "ports", "--port", &occupied])
            .status
            .success()
    );
    let mut children = Vec::new();
    for i in 0..8 {
        children.push(
            fixture
                .command()
                .args(["--json", "port", "reserve", &format!("server{i}"), "ports"])
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
    let before = fixture.ok(&["port", "list", "ports"]);
    assert!(fixture.run(&["daemon", "stop"]).status.success());
    fixture.daemon.wait().unwrap();
    fixture.daemon = fixture
        .command()
        .args(["daemon", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fixture.wait_ready();
    assert_eq!(fixture.ok(&["port", "list", "ports"]), before);
    fixture.ok(&["rm", "ports"]);
}

#[test]
fn diff_excludes_new_main_commits_before_and_after_rebase_and_uses_git_configuration() {
    let fixture = Fixture::new();
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
    fixture.ok(&["rm", "changes", "--yes", "--delete-branch"]);
}

#[test]
fn configured_port_range_exhaustion_and_release() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let number = listener.local_addr().unwrap().port();
    let fixture = Fixture::with_config(Some(&format!("[ports]\nstart={number}\nend={number}\n")));
    fixture.add("limited");
    assert!(
        !fixture
            .run(&["port", "reserve", "web", "limited"])
            .status
            .success()
    );
    drop(listener);
    let lease = fixture.ok(&["port", "reserve", "web", "limited"]);
    assert_eq!(lease["port"], number);
    assert!(
        !fixture
            .run(&["port", "reserve", "api", "limited"])
            .status
            .success()
    );
    fixture.ok(&["port", "release", "web", "limited"]);
    assert_eq!(
        fixture.ok(&["port", "reserve", "api", "limited"])["port"],
        number
    );
    fixture.ok(&["rm", "limited"]);
}

#[test]
fn concurrent_adds_cannot_claim_the_same_name() {
    let fixture = Fixture::new();
    let first = fixture
        .command()
        .args(["add", fixture.repo.to_str().unwrap(), "--name", "shared"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second = fixture.run(&["add", fixture.repo.to_str().unwrap(), "--name", "shared"]);
    let first = first.wait_with_output().unwrap();
    assert_ne!(first.status.success(), second.status.success());
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
    fixture.ok(&["rm", "shared"]);
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
            command.arg("cli");
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
    fixture.daemon.wait().unwrap();
    fixture.daemon = fixture
        .command()
        .args(["daemon", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fixture.wait_ready();
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
shoal add "$REPO" --name navigate
test "${PWD##*/}" = navigate
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
test "$PWD" = "$REPO"
if shoal cd -; then
  exit 1
fi
test "$PWD" = "$REPO"
printf 'navigation-ok\n'
"#;
    for shell in ["bash", "zsh"] {
        let output = Command::new(shell)
            .arg("-c")
            .arg(script)
            .env("INTEGRATION", &integration)
            .env("REPO", &fixture.repo)
            .env("SHOAL_STATE_DIR", fixture.root.path().join("state"))
            .env("HOME", fixture.root.path())
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
    let path = Path::new(workspace["path"].as_str().unwrap());
    let overview = fixture
        .command()
        .current_dir(path)
        .args(["--json", "ports"])
        .output()
        .unwrap();
    assert!(overview.status.success());
    let overview: Value = serde_json::from_slice(&overview.stdout).unwrap();
    assert_eq!(overview["configured"]["web"]["port"], preferred);
    assert_eq!(overview["reserved"], serde_json::json!([]));
    let proposal = fixture.run(&["--json", "port", "reserve", "web", "configured"]);
    assert_eq!(proposal.status.code(), Some(2));
    let proposal: Value = serde_json::from_slice(&proposal.stdout).unwrap();
    assert_eq!(proposal["reserved"], false);
    assert_eq!(
        fixture.ok(&["port", "list", "configured"]),
        serde_json::json!([])
    );
    let accepted = fixture.ok(&[
        "port",
        "reserve",
        "web",
        "configured",
        "--port",
        &proposal["suggested_port"].to_string(),
    ]);
    assert_eq!(accepted["env_var"], "PORT");
    assert_eq!(
        fixture.ok(&["port", "reserve", "web", "configured"]),
        accepted
    );
    fixture.ok(&["port", "release", "web", "configured"]);
    let automatic = fixture.ok(&[
        "port",
        "reserve",
        "web",
        "configured",
        "--on-conflict",
        "auto",
    ]);
    assert_ne!(automatic["port"], preferred);
    assert_eq!(
        fixture.ok(&["port", "reserve", "web", "configured"]),
        automatic
    );
    fs::create_dir(path.join(".shoal")).unwrap();
    fs::rename(path.join(".shoal.toml"), path.join(".shoal/config.toml")).unwrap();
    fixture.ok(&["ports", "configured"]);
    fs::write(path.join(".shoal.toml"), "").unwrap();
    assert!(!fixture.run(&["ports", "configured"]).status.success());
}

#[test]
fn execution_scope_limits_management_and_expires() {
    let fixture = Fixture::new();
    fixture.add("worker");
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
    assert!(scoped(&["port", "reserve", "web"]).status.success());
    assert!(
        scoped(&["exec", "worker", "--", binary, "ports"])
            .status
            .success()
    );
    for args in [
        vec!["rm", "worker", "--yes", "--delete-branch"],
        vec!["stop", "worker"],
        vec!["inspect", "other"],
        vec!["port", "reserve", "web", "other"],
        vec!["repo", "rename", fixture.repo.to_str().unwrap(), "changed"],
        vec!["daemon", "stop"],
        vec!["setup", "--dry-run"],
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
    let first = fixture.ok(&["sim", "acquire", "first"]);
    assert_eq!(first["state"], "leased");
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
        "list",
        "--all",
    ]);
    assert_eq!(
        serde_json::from_slice::<Value>(&listed.stdout).unwrap(),
        serde_json::json!([])
    );
    fixture.ok(&["rm", "first"]);
    assert_eq!(
        fixture
            .ok(&["sim", "list", "--all"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.ok(&["rm", "second"]);
    assert_eq!(fixture.ok(&["sim", "list", "--all"]), serde_json::json!([]));
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
    assert_eq!(fixture.ok(&["sim", "list", "first"])[0]["state"], "failed");
    fs::remove_file(fixture.root.path().join("sim-fail")).unwrap();
    fixture.ok(&["sim", "release", "default", "first"]);
    let lease = fixture.ok(&["sim", "acquire", "first"]);
    fixture.daemon.kill().unwrap();
    fixture.daemon.wait().unwrap();
    fixture.daemon = fixture
        .command()
        .args(["daemon", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fixture.wait_ready();
    assert_eq!(fixture.ok(&["sim", "acquire", "first"]), lease);
    assert_eq!(
        fixture.run(&["sim", "acquire", "second"]).status.code(),
        Some(2)
    );
    fs::write(fixture.root.path().join("sim-fail"), "delete").unwrap();
    assert!(!fixture.run(&["rm", "first"]).status.success());
    assert_eq!(
        fixture.ok(&["sim", "list", "first"])[0]["udid"],
        lease["udid"]
    );
    fs::remove_file(fixture.root.path().join("sim-fail")).unwrap();
    fixture.ok(&["rm", "first"]);
    assert_eq!(fixture.ok(&["sim", "list", "--all"]), serde_json::json!([]));
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
    fs::write(
        fixture.root.path().join("sim-devices.json"),
        serde_json::to_string(&devices).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fixture.run(&["sim", "acquire", "worker"]).status.code(),
        Some(2)
    );
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
        fixture
            .ok(&["sim", "list", "--all"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.ok(&["sim", "release", "default", "worker"]);
    fs::write(fixture.root.path().join("sim-lost-create-response"), "1").unwrap();
    assert!(!fixture.run(&["sim", "acquire", "worker"]).status.success());
    let record = fixture.ok(&["sim", "list", "worker"]);
    assert_eq!(record[0]["state"], "failed");
    assert!(record[0]["udid"].is_null());
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
        let sims = fixture.ok(&["sim", "list", "--all"]);
        if sims.as_array().unwrap().len() == 1 {
            assert_eq!(sims[0]["udid"], second["udid"]);
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
    fixture.daemon.kill().unwrap();
    fixture.daemon.wait().unwrap();
    fixture.daemon = fixture
        .command()
        .args(["daemon", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fixture.wait_ready();
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
    assert_eq!(
        fixture.ok(&["resource", "list", "--all"]),
        serde_json::json!([])
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
    let overview = fixture.ok(&["resources", "third"]);
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
    let device_leases = fixture.ok(&["resource", "list", "--all"]);
    assert!(
        device_leases
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["pool"] != "devices")
    );
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
    let leases = fixture.ok(&["resource", "list", "--all"]);
    assert_eq!(leases.as_array().unwrap().len(), 3);
    fixture.daemon.kill().unwrap();
    fixture.daemon.wait().unwrap();
    fixture.daemon = fixture
        .command()
        .args(["daemon", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fixture.wait_ready();
    assert_eq!(fixture.ok(&["resource", "list", "--all"]), leases);
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
        fixture
            .ok(&["resource", "list", "--all"])
            .as_array()
            .unwrap()
            .len(),
        3
    );
    fixture.ok(&["rm", "worker", "--yes", "--keep-branch"]);
    assert_eq!(
        fixture.ok(&["resource", "list", "--all"]),
        serde_json::json!([])
    );
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
    let overview = fixture.ok(&["resources", "second"]);
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
        .args(["--json", "resources"])
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
    let second = fixture.ok(&["add", other.to_str().unwrap(), "--name", "second"]);
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
    assert!(!scoped(&["resources", "second"]).status.success());
    let listed: Value =
        serde_json::from_slice(&scoped(&["--json", "resource", "list", "--all"]).stdout).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 2);
    assert!(
        listed
            .as_array()
            .unwrap()
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
fn pull_remote(fixture: &Fixture) -> PathBuf {
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
        &["branch", "--set-upstream-to=origin/main", "main"],
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
    git(&author, &["push", "origin", "main"]);
    author
}

#[test]
fn scoped_pull_updates_only_main_and_denies_other_workspace_targets() {
    let fixture = Fixture::new();
    let workspace = fixture.add("worker");
    fixture.add("other");
    let before = git(&fixture.repo, &["rev-parse", "main"]);
    let author = pull_remote(&fixture);
    let expected = git(&author, &["rev-parse", "HEAD"]);
    let binary = env!("CARGO_BIN_EXE_shoal");
    let output = fixture.run(&["exec", "worker", "--", binary, "--json", "pull", "other"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot access another worktree"));
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
    let output = fixture.run(&["exec", "worker", "--", binary, "--json", "pull"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["updated"], true);
    assert_eq!(result["previous_commit"], before.trim());
    assert_eq!(result["commit"], expected.trim());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), expected);
    assert_eq!(
        git(
            Path::new(workspace["path"].as_str().unwrap()),
            &["rev-parse", "HEAD"]
        ),
        before
    );
    assert_eq!(fixture.ok(&["pull", "worker"])["updated"], false);
    assert_eq!(
        git(&fixture.repo, &["for-each-ref", "refs/shoal/pull/"]),
        ""
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
    let overview = fixture.ok(&["resources", "first"]);
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
    let status = fixture.ok(&["resources", "second"]);
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
    let overview = fixture.ok(&["resources", "owner"]);
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
    fixture.daemon.kill().unwrap();
    fixture.daemon.wait().unwrap();
    fixture.daemon = fixture
        .command()
        .args(["daemon", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fixture.wait_ready();
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
    assert_eq!(fixture.ok(&["resource", "list", "reader"])[0], lease);
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
        fixture.ok(&["resources", "second"])["pools"][0]["configuration_matches"],
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
fn cd_always_picks_even_inside_a_workspace_and_cancel_does_not_navigate() {
    use std::os::fd::FromRawFd;
    let fixture = Fixture::new();
    let first = fixture.add("first");
    let second = fixture.add("second");
    let mut broken = fixture.command();
    let failed = broken
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "--name",
            "missing",
            "--ref",
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
        let (mut master, mut slave) = (-1, -1);
        // Give the CLI real terminal handles so its normal interactive path runs.
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
        let _master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
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

fn recovery_report(fixture: &Fixture, args: &[&str]) -> Value {
    let output = fixture.command().arg("--json").args(args).output().unwrap();
    assert!(
        matches!(output.status.code(), Some(0 | 2)),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn reconcile_repairs_interrupted_state_and_preserves_work_and_leases() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
    let workspace = fixture.add("interrupted");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("uncommitted"), "preserve me").unwrap();
    let port = fixture.ok(&["port", "reserve", "web", "interrupted"]);
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
    let preview = recovery_report(&fixture, &["reconcile", "interrupted"]);
    assert_eq!(preview[0]["directory"], "valid");
    assert!(!preview[0]["issues"].as_array().unwrap().is_empty());
    assert_eq!(
        fixture.ok(&["inspect", "interrupted"])["workspace"]["state"],
        "failed"
    );
    let repaired = fixture.ok(&["reconcile", "interrupted", "--repair"]);
    assert_eq!(repaired[0]["workspace"]["state"], "ready");
    assert_eq!(
        fs::read_to_string(path.join("uncommitted")).unwrap(),
        "preserve me"
    );
    assert_eq!(fixture.ok(&["port", "list", "interrupted"])[0], port);
    assert_eq!(
        fixture.ok(&["resource", "list", "interrupted"])[0],
        resource
    );
    assert!(
        fixture.ok(&["reconcile", "interrupted", "--repair"])[0]["changes"]
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
                "reconcile",
                "--all",
                "--repair"
            ])
            .status
            .success()
    );
}

#[test]
fn reconcile_detects_moved_and_replaced_worktrees_without_deleting_data() {
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
    let report = recovery_report(&fixture, &["reconcile", "original", "--repair"]);
    assert_eq!(report[0]["directory"], "moved");
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
    fixture.ok(&["reconcile", "original", "--repair"]);
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
    let report = recovery_report(&fixture, &["reconcile", "original", "--repair"]);
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
fn reconcile_missing_worktrees_allows_explicit_cleanup_and_retains_branches() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
    for (name, prune_git) in [("directory-only", false), ("git-removed", true)] {
        let workspace = fixture.add(name);
        fixture.ok(&["port", "reserve", "web", name]);
        fixture.ok(&["resource", "acquire", "lock", name]);
        let path = Path::new(workspace["path"].as_str().unwrap());
        if prune_git {
            git(
                &fixture.repo,
                &["worktree", "remove", path.to_str().unwrap()],
            );
        } else {
            fs::remove_dir_all(path).unwrap();
        }
        fixture.restart();
        assert_eq!(
            fixture.ok(&["inspect", name])["workspace"]["state"],
            "failed"
        );
        let report = recovery_report(&fixture, &["reconcile", name, "--repair"]);
        assert_eq!(report[0]["directory"], "missing");
        assert_eq!(
            fixture
                .ok(&["port", "list", name])
                .as_array()
                .unwrap()
                .len(),
            1
        );
        fixture.ok(&["rm", name]);
        assert_eq!(
            fixture.ok(&["port", "list", "--all"]),
            serde_json::json!([])
        );
        assert_eq!(
            fixture.ok(&["resource", "list", "--all"]),
            serde_json::json!([])
        );
        assert!(!git(&fixture.repo, &["rev-parse", &format!("refs/heads/{name}")]).is_empty());
        assert!(
            !git(&fixture.repo, &["worktree", "list", "--porcelain"])
                .contains(path.to_str().unwrap())
        );
    }
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
fn reconcile_stops_identity_verified_orphans_after_wrapper_death() {
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
    let report = recovery_report(&fixture, &["reconcile", "orphan", "--repair"]);
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
    fixture.ok(&[
        "reconcile",
        "orphan",
        "--repair",
        "--stop",
        "--acknowledge-stopped",
    ]);
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
fn reconcile_recovers_daemon_crash_and_requires_acknowledgement_for_legacy_records() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("crash");
    let port = fixture.ok(&["port", "reserve", "web", "crash"]);
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
    let report = recovery_report(&fixture, &["reconcile", "crash"]);
    assert_eq!(report[0]["executions"][0]["state"], "unknown");
    fixture.ok(&["reconcile", "crash", "--repair", "--acknowledge-stopped"]);
    assert_eq!(fixture.ok(&["port", "list", "crash"])[0], port);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO executions(id,workspace_id,state) VALUES ('legacy',?1,'unknown')",
        [workspace["id"].as_str().unwrap()],
    )
    .unwrap();
    let report = recovery_report(&fixture, &["reconcile", "crash", "--repair"]);
    assert!(!report[0]["executions"][0]["cleared"].as_bool().unwrap());
    fixture.ok(&["reconcile", "crash", "--repair", "--acknowledge-stopped"]);
    assert_eq!(
        fixture.ok(&["inspect", "crash"])["executions"],
        serde_json::json!([])
    );
}

#[test]
fn reconcile_finds_detached_tagged_children_even_after_the_command_exits() {
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
    let report = recovery_report(&fixture, &["reconcile", "detached"]);
    assert!(
        !report[0]["executions"][0]["processes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    fixture.ok(&[
        "reconcile",
        "detached",
        "--repair",
        "--stop",
        "--acknowledge-stopped",
    ]);
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
fn reconcile_all_reports_each_workspace_independently() {
    let fixture = Fixture::new();
    fixture.add("healthy");
    let missing = fixture.add("missing");
    fs::remove_dir_all(missing["path"].as_str().unwrap()).unwrap();
    let reports = recovery_report(&fixture, &["reconcile", "--all", "--repair"]);
    assert_eq!(reports.as_array().unwrap().len(), 2);
    assert_eq!(reports[0]["workspace"]["name"], "healthy");
    assert_eq!(reports[0]["workspace"]["state"], "ready");
    assert_eq!(reports[1]["workspace"]["state"], "failed");
}

#[test]
fn reconcile_preserves_connected_commands_until_stop_is_explicit() {
    let fixture = Fixture::new();
    fixture.add("connected");
    let port = fixture.ok(&["port", "reserve", "web", "connected"]);
    let mut wrapper = fixture
        .command()
        .args(["exec", "connected", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "connected");
    let report = fixture.ok(&["reconcile", "connected", "--repair"]);
    assert_eq!(report[0]["executions"][0]["connected"], true);
    assert_eq!(report[0]["executions"][0]["cleared"], false);
    assert!(wrapper.try_wait().unwrap().is_none());
    fixture.ok(&["reconcile", "connected", "--repair", "--stop"]);
    wrapper.wait().unwrap();
    assert_eq!(
        fixture.ok(&["inspect", "connected"])["executions"],
        serde_json::json!([])
    );
    assert_eq!(fixture.ok(&["port", "list", "connected"])[0], port);
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
                command.arg("app");
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
    for args in [vec!["codex", "app", "other"], vec!["t3", "other"]] {
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
fn merge_fetches_remote_only_branch_and_refreshes_qualified_sources() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    let author = pull_remote(&fixture);
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
}

#[test]
fn merge_remote_ambiguity_local_precedence_and_explicit_remote() {
    let fixture = Fixture::new();
    fixture.add("worker");
    let author = pull_remote(&fixture);
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
