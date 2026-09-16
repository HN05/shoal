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
        assert!(
            Command::new("wt").arg("--version").output().is_ok(),
            "workspace integration tests require Worktrunk (wt)"
        );
        let root = tempfile::tempdir_in("/tmp").unwrap();
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
fn named_workspace_uses_committed_history_and_preserves_branch_on_removal() {
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
    git(
        &fixture.repo,
        &[
            "rev-parse",
            "--verify",
            workspace["branch"].as_str().unwrap(),
        ],
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
    git(&fixture.repo, &["rev-parse", "--verify", "my-branch"]);

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
    fixture.add("shortcut");
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
        let output = fixture
            .command()
            .args([agent, "shortcut", "--", "--version", "hello with spaces"])
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
            "shortcut\n--version\nhello with spaces\n"
        );
    }
}

#[test]
fn stop_requests_terminate_the_connected_execution() {
    let fixture = Fixture::new();
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
    fixture.ok(&["stop", "running"]);
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(Instant::now() < deadline);
    fixture.ok(&["rm", "running"]);
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
shoal add "$REPO" --name navigate
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
