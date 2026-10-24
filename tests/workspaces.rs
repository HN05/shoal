#[path = "support/prompt.rs"]
mod prompt;
#[path = "support/pty.rs"]
mod pty;

#[path = "support/daemon.rs"]
mod daemon_fixture;
use daemon_fixture::DaemonGuard;

#[path = "support/git.rs"]
mod git_fixture;
use git_fixture::{git, init_repo};

#[path = "support/ports.rs"]
mod ports;

mod support;

use support::{
    cli, install_test_script, install_test_script_with_mode,
    wait::{try_wait_for, wait_for, wait_until},
};

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

/// Global config without a cleanup grace period, at the root so the rest of a
/// test's config follows unchanged.
const NO_GRACE: &str = "cleanup.grace_minutes = 0\n";
/// The scripted forges serve Forgejo's API without answering which one they
/// serve; `tests/forge.rs` covers asking.
const FORGEJO_HOST: &str = "forges.\"forge.example\".type = \"forgejo\"\n";

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
        // Remove eligible workspaces on their first sweep unless a test sets
        // its own grace period.
        fs::create_dir_all(root.path().join(".config/shoal")).unwrap();
        fs::write(
            root.path().join(".config/shoal/config.toml"),
            format!("{NO_GRACE}{FORGEJO_HOST}{}", config.unwrap_or_default()),
        )
        .unwrap();
        if fake_sim {
            let script = root.path().join("bin/xcrun");
            install_test_script(&script, include_str!("fixtures/simctl.py"));
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

    // Exercise persisted/manual acknowledgement compatibility without retaining
    // the removed CLI spelling.
    fn acknowledge(&self, workspace: &str) -> Value {
        self.request(serde_json::json!({"set_pr":{"workspace":workspace,"clear":false}}))
    }

    fn request(&self, method: Value) -> Value {
        self.request_with_id(1, method)
    }

    fn request_with_id(&self, id: u64, method: Value) -> Value {
        let protocol = self.protocol();
        self.raw_request(protocol, id, method)
    }

    fn protocol(&self) -> u64 {
        support::daemon_client::protocol(self.root.path())
    }

    fn raw_request(&self, protocol: u64, id: u64, method: Value) -> Value {
        support::daemon_client::exchange(
            self.root.path(),
            &serde_json::json!({"protocol": protocol, "id": id, "method": method}),
        )
    }

    fn rename(&self, workspace: &str, branch: &str) -> Value {
        self.request(
            serde_json::json!({"rename_workspace":{"workspace":workspace,"branch":branch}}),
        )
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
        self.interactive_command(self.command().args(args), answer)
    }

    fn interactive_command(&self, command: &mut Command, answer: &str) -> (Output, String) {
        self.terminal_command(command, answer, false)
    }

    /// Run `command` as `interactive_command` does, with stdout on the
    /// terminal too when `terminal_stdout` is set.
    fn terminal_command(
        &self,
        command: &mut Command,
        answer: &str,
        terminal_stdout: bool,
    ) -> (Output, String) {
        command.env("PATH", self.interactive_path());
        prompt::answer(
            command,
            answer,
            prompt::AnswerOptions {
                controlling_terminal: true,
                kill_on_timeout: true,
                terminal_stdout,
            },
        )
    }

    /// Run `args` on a terminal with a picker that records its rows and selects
    /// the row whose ID is `choice`, then type `answer` into later prompts.
    fn pick(&self, args: &[&str], choice: &str, answer: &str) -> (Output, String) {
        self.pick_in(self.root.path(), args, choice, answer)
    }

    /// `pick` run from `cwd`.
    fn pick_in(&self, cwd: &Path, args: &[&str], choice: &str, answer: &str) -> (Output, String) {
        let bin = self.root.path().join("bin");
        let fzf = bin.join("fzf");
        install_test_script(
            &fzf,
            "#!/bin/sh\ncat > \"$SHOAL_TEST_PICK_INPUT\"\nawk -F '\\t' -v id=\"$SHOAL_TEST_PICK_ID\" '$1 == id { print }' \"$SHOAL_TEST_PICK_INPUT\"\n",
        );
        let rows = self.root.path().join("picker-rows");
        let _ = fs::remove_file(&rows);
        let (output, _) = self.interactive_command(
            self.command()
                .current_dir(cwd)
                .env("SHOAL_TEST_PICK_INPUT", &rows)
                .env("SHOAL_TEST_PICK_ID", choice)
                .args(args),
            answer,
        );
        (output, fs::read_to_string(&rows).unwrap_or_default())
    }

    /// Save an interrupted command for `shoal resume`, as `shoal stop` does.
    fn save_stopped_command(&self, workspace: &Value) {
        let state = self
            .root
            .path()
            .join("state/workspaces")
            .join(workspace["id"].as_str().unwrap());
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("saved.command.json"), r#"{"argv":["true"]}"#).unwrap();
    }

    /// Interactive commands open pickers for whatever agents and fzf they find,
    /// so they see only the fixture's `bin`, the runner's Git and Worktrunk, and
    /// system directories.
    fn interactive_path(&self) -> String {
        let tools = self.root.path().join("runner-bin");
        if !tools.exists() {
            fs::create_dir(&tools).unwrap();
            for program in ["git", "wt"] {
                link_runner_program(&tools, program);
            }
            install_test_script(tools.join("zmx"), PASSTHROUGH_ZMX);
        }
        format!(
            "{}:{}:{}",
            self.root.path().join("bin").display(),
            tools.display(),
            self.system_path()
        )
    }

    /// `bin`, the runner's Worktrunk, which setup uses to copy ignored files,
    /// and system directories.
    fn restricted_path(&self, bin: &Path) -> String {
        let tools = self.root.path().join("worktrunk-bin");
        if !tools.exists() {
            fs::create_dir(&tools).unwrap();
            link_runner_program(&tools, "wt");
        }
        format!(
            "{}:{}:{}",
            bin.display(),
            tools.display(),
            self.system_path()
        )
    }

    /// `/usr/bin` and `/bin` without the programs fixtures stand in for.
    /// Distributions package some of them there, as Fedora does fzf, so a
    /// directory holding one is replaced by links to the rest of its programs.
    fn system_path(&self) -> String {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut seen = Vec::new();
        for dir in ["/usr/bin", "/bin"] {
            let dir = Path::new(dir);
            // Merged-/usr systems link /bin to /usr/bin.
            let canonical = fs::canonicalize(dir).unwrap();
            if seen.contains(&canonical) {
                continue;
            }
            seen.push(canonical);
            if !FIXTURE_PROGRAMS
                .iter()
                .any(|program| dir.join(program).exists())
            {
                dirs.push(dir.to_owned());
                continue;
            }
            let mirror = self
                .root
                .path()
                .join("system")
                .join(dir.strip_prefix("/").unwrap());
            if !mirror.exists() {
                link_programs_except_fixture_ones(dir, &mirror);
            }
            dirs.push(mirror);
        }
        std::env::join_paths(dirs).unwrap().into_string().unwrap()
    }
}

/// Fill `mirror` with links to the programs in `dir` except `FIXTURE_PROGRAMS`.
fn link_programs_except_fixture_ones(dir: &Path, mirror: &Path) {
    fs::create_dir_all(mirror).unwrap();
    for entry in fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name();
        if !FIXTURE_PROGRAMS.iter().any(|program| name == *program) {
            std::os::unix::fs::symlink(dir.join(&name), mirror.join(&name)).unwrap();
        }
    }
}

/// Programs tests install fakes for or expect to be missing: the picker, agents,
/// Happy and zmx. The runner's own must not answer in their place.
const FIXTURE_PROGRAMS: &[&str] = &[
    "fzf", "claude", "codex", "opencode", "pi", "grok", "happy", "zmx",
];

/// Link the runner's `program` into `tools`.
fn link_runner_program(tools: &Path, program: &str) {
    let path = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join(program))
        .find(|path| {
            path.metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .unwrap_or_else(|| panic!("{program} is not on PATH"));
    std::os::unix::fs::symlink(path, tools.join(program)).unwrap();
}

#[test]
fn workspace_rename_cli_infers_context_and_enforces_execution_scope() {
    let fixture = Fixture::new();
    let workspace = fixture.add("original");
    fixture.add("other");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let binary = env!("CARGO_BIN_EXE_shoal");
    let denied = fixture.run(&[
        "exec", "original", "--", binary, "rename", "other", "stolen",
    ]);
    assert!(!denied.status.success());
    assert_eq!(
        fixture.ok(&["inspect", "other"])["workspace"]["branch"],
        "other"
    );
    let renamed = fixture.ok(&[
        "exec",
        "original",
        "--",
        binary,
        "--json",
        "rename",
        "feature/topic",
    ]);
    assert_eq!(renamed["branch"], "feature/topic");
    assert_eq!(renamed["path"], workspace["path"]);
    let output = fixture
        .command()
        .current_dir(path)
        .args(["--json", "rename", "by-directory"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["name"],
        "by-directory"
    );
    let explicit = fixture.ok(&["rename", "by-directory", "explicit"]);
    assert_eq!(explicit["name"], "explicit");
    assert!(!fixture.run(&["rename"]).status.success());
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO executions(id,workspace_id,state) VALUES ('unknown',?1,'unknown')",
        [workspace["id"].as_str().unwrap()],
    )
    .unwrap();
    let denied = fixture.rename("explicit", "blocked");
    assert_eq!(denied["type"], "error");
    assert!(
        denied["data"]["message"]
            .as_str()
            .unwrap()
            .contains("other active or unknown executions")
    );
    assert_eq!(
        fixture.ok(&["inspect", "explicit"])["workspace"]["branch"],
        "explicit"
    );
    db.execute("DELETE FROM executions WHERE id='unknown'", [])
        .unwrap();
    let output = fixture.run(&["exec", "explicit", "--", "/usr/bin/env"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("SHOAL_WORKSPACE=explicit\n"));
}

#[test]
fn workspace_rename_preserves_work_ownership_and_diff_bases() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\n[done]\ncleanup = false\n"));
    let workspace = fixture.add("original");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let id = workspace["id"].as_str().unwrap();
    fixture.ok(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "dependent",
        "--base",
        "original",
    ]);
    fs::write(path.join("dirty"), "preserve\n").unwrap();
    git(path, &["config", "branch.original.remote", "."]);
    git(
        path,
        &["config", "branch.original.merge", "refs/heads/main"],
    );
    fixture.ok(&["port", "acquire", "web", "original"]);
    fixture.ok(&["resource", "acquire", "lock", "original"]);
    fixture.acknowledge("original");
    let blocked = fixture.rename("original", "blocked");
    assert_eq!(blocked["type"], "error");
    assert!(
        blocked["data"]["message"]
            .as_str()
            .unwrap()
            .contains("PR watch or acknowledgement")
    );
    fixture.request(serde_json::json!({"set_pr":{"workspace":"original","clear":true}}));
    fixture.ok(&["done", "original"]);
    let before = fixture.ok(&["inspect", "original"]);
    let response = fixture.rename("original", "feature/topic");
    assert_eq!(response["type"], "workspace", "{response}");
    let renamed = &response["data"];
    assert_eq!(renamed["name"], "feature-topic");
    assert_eq!(renamed["branch"], "feature/topic");
    assert_eq!(
        fixture.ok(&["status", "feature-topic"])["workspace"]["branch"],
        "feature/topic"
    );
    fs::write(path.join("after-rename"), "changed after rename\n").unwrap();
    git(path, &["add", "after-rename"]);
    let diff = fixture.run(&["diff", "feature-topic"]);
    assert!(diff.status.success());
    assert!(String::from_utf8_lossy(&diff.stdout).contains("changed after rename"));
    fixture.ok(&["done", "feature-topic"]);
    for field in [
        "id",
        "path",
        "git_dir",
        "git_dir_id",
        "base_ref",
        "base_commit",
    ] {
        assert_eq!(renamed[field], workspace[field]);
    }
    assert!(!fixture.run(&["inspect", "original"]).status.success());
    assert_eq!(
        git(path, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/feature/topic"
    );
    assert_eq!(
        git(path, &["rev-parse", "feature/topic@{upstream}"]),
        git(path, &["rev-parse", "main"])
    );
    assert!(
        git(path, &["reflog", "show", "feature/topic"]).contains("renamed refs/heads/original")
    );
    assert_eq!(
        fs::read_to_string(path.join("dirty")).unwrap(),
        "preserve\n"
    );
    assert_eq!(
        fixture.ok(&["inspect", "dependent"])["workspace"]["base_ref"],
        "refs/heads/feature/topic"
    );
    fixture.restart();
    let after = fixture.ok(&["inspect", id]);
    for field in ["ports", "resources"] {
        assert_eq!(after[field], before[field]);
    }
    assert_eq!(after["completion"]["head"], before["completion"]["head"]);
    assert_eq!(after["pr_cleanup"]["head"], before["pr_cleanup"]["head"]);
    assert_eq!(after["workspace"], *renamed);
}

#[test]
fn workspace_rename_issue_reopens_the_associated_workspace() {
    let fixture = Fixture::with_config(Some("[commands]\npi = ['sh', '-c', 'exit 0']\n"));
    fixture.add_github_origin();
    install_test_script(
        fixture.root.path().join("bin/gh"),
        "#!/bin/sh\necho '{\"state\":\"OPEN\",\"number\":34,\"title\":\"Fix API timeout\",\"body\":\"\"}'\n",
    );
    let repo = fixture.repo.to_str().unwrap();
    let issue = ["add", "34", "--repo", repo, "--agent", "pi"];
    let created = fixture.ok(&[&issue[..], &["--base", "HEAD"]].concat());
    let renamed = fixture.ok(&["rename", "34-fix-api-timeout", "renamed-issue"]);
    let reopened = fixture.ok(&issue);
    assert_eq!(reopened["id"], created["id"]);
    assert_eq!(reopened["path"], created["path"]);
    assert_eq!(reopened["branch"], renamed["branch"]);
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
}

#[test]
fn workspace_rename_refuses_collisions_and_recovers_git_failure() {
    let fixture = Fixture::new();
    let workspace = fixture.add("original");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fixture.add("other-name");
    git(&fixture.repo, &["branch", "taken"]);
    for branch in [
        "other/name",
        "taken",
        "bad name",
        "@{-1}",
        "HEAD",
        "@",
        "0123456789abcdef0123456789abcdef01234567",
        "taken/child",
    ] {
        let response = fixture.rename("original", branch);
        assert_eq!(response["type"], "error", "{branch}: {response}");
        assert_eq!(fixture.ok(&["inspect", "original"])["workspace"], workspace);
        assert_eq!(
            git(path, &["symbolic-ref", "HEAD"]).trim(),
            "refs/heads/original"
        );
    }
    git(path, &["switch", "--detach"]);
    assert_eq!(fixture.rename("original", "new-name")["type"], "error");
    git(path, &["switch", "original"]);
    // A different literal branch may have the same normalized workspace name.
    assert_eq!(fixture.rename("original", "original")["type"], "workspace");
    assert_eq!(fixture.rename("original", "new-name")["type"], "workspace");
    assert_eq!(fixture.rename("new-name", "new/name")["type"], "workspace");
    fixture.ok(&["rm", "new-name", "--yes"]);
    assert!(!path.exists());
    assert!(
        git(
            &fixture.repo,
            &["for-each-ref", "--format=%(refname)", "refs/heads/new"]
        )
        .is_empty()
    );
    let reused = fixture.add("original");
    assert_eq!(reused["branch"], "original");
    assert_ne!(reused["id"], workspace["id"]);
}

#[test]
fn interrupted_workspace_rename_requires_explicit_repair() {
    for branch in ["original", "new/topic", "unrelated"] {
        let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
        let workspace = fixture.add("original");
        let path = Path::new(workspace["path"].as_str().unwrap());
        let id = workspace["id"].as_str().unwrap();
        fs::write(path.join("dirty"), "preserve\n").unwrap();
        let port = fixture.ok(&["port", "acquire", "web", "original"]);
        let lease = fixture.ok(&["resource", "acquire", "lock", "original"]);
        let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
        db.execute("INSERT INTO workspace_renames(workspace_id,name,branch) VALUES (?1,'new-topic','new/topic')", [id]).unwrap();
        db.execute(
            "UPDATE workspaces SET state='reconciling' WHERE id=?1",
            [id],
        )
        .unwrap();
        if branch != "original" {
            git(path, &["branch", "-m", branch]);
        }
        if branch == "new/topic" {
            fs::remove_file(Path::new(workspace["git_dir"].as_str().unwrap()).join(OWNER_MARKER))
                .unwrap();
            db.execute("UPDATE workspaces SET git_dir_id='birth:1:2:3'", [])
                .unwrap();
        }
        fixture.restart();
        assert_eq!(fixture.ok(&["inspect", id])["workspace"]["state"], "failed");
        let rejected = fixture.run(&["add", fixture.repo.to_str().unwrap(), "new-topic"]);
        assert!(!rejected.status.success());
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains("reserved by an unfinished rename")
        );
        let report = recovery_report(&fixture, &["doctor", id]);
        assert!(
            report[0]["issues"]
                .to_string()
                .contains("unfinished rename")
        );
        if branch == "unrelated" {
            let report = recovery_report(&fixture, &["doctor", id, "--repair"]);
            assert!(
                report[0]["issues"]
                    .to_string()
                    .contains("restore one before repair")
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM workspace_renames", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            git(path, &["branch", "-m", "original"]);
        }
        let repair_args = if branch == "new/topic" {
            vec!["doctor", id, "--repair", "--reclaim"]
        } else {
            vec!["doctor", id, "--repair"]
        };
        repaired_workspaces(&fixture, &repair_args);
        let inspection = fixture.ok(&["inspect", id]);
        assert_eq!(inspection["workspace"]["state"], "ready");
        assert_eq!(
            inspection["workspace"]["branch"],
            if branch == "new/topic" {
                "new/topic"
            } else {
                "original"
            }
        );
        assert_eq!(
            inspection["workspace"]["name"],
            if branch == "new/topic" {
                "new-topic"
            } else {
                "original"
            }
        );
        assert_eq!(inspection["workspace"]["path"], workspace["path"]);
        assert_eq!(inspection["ports"][0], port);
        assert_eq!(inspection["resources"][0], lease);
        assert_eq!(
            fs::read_to_string(path.join("dirty")).unwrap(),
            "preserve\n"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM workspace_renames", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn deleted_worktree_with_interrupted_rename_can_be_removed() {
    let fixture = Fixture::with_config(Some("[resources.lock]\n"));
    let workspace = fixture.add("original");
    // A commit of its own keeps the branch from the kept-branch sweep.
    commit_file(
        Path::new(workspace["path"].as_str().unwrap()),
        "original",
        "work",
    );
    fixture.ok(&["port", "acquire", "web", "original"]);
    fixture.ok(&["resource", "acquire", "lock", "original"]);
    let id = workspace["id"].as_str().unwrap();
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO workspace_renames(workspace_id,name,branch) VALUES (?1,'new-topic','new/topic')",
        [id],
    )
    .unwrap();
    db.execute("UPDATE workspaces SET state='failed' WHERE id=?1", [id])
        .unwrap();
    fs::remove_dir_all(workspace["path"].as_str().unwrap()).unwrap();
    drop(db);
    let removed = fixture.ok(&["rm", "original"]);
    assert_eq!(removed["branch_deleted"], false);
    assert!(!fixture.run(&["inspect", "original"]).status.success());
    assert!(
        fixture
            .ok(&["port", "list", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .ok(&["resource", "list", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!git(&fixture.repo, &["rev-parse", "refs/heads/original"]).is_empty());
}

#[test]
fn cleanup_removes_idle_candidates_on_request() {
    let fixture = Fixture::new();
    let clean = fixture.add("clean");
    let dirty = fixture.add("dirty");
    fs::write(
        Path::new(dirty["path"].as_str().unwrap()).join("work"),
        "keep",
    )
    .unwrap();
    let binary = env!("CARGO_BIN_EXE_shoal");
    let denied = fixture.run(&["exec", "dirty", "--", binary, "cleanup"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("workspace processes can only"));
    let preview = fixture.ok(&["cleanup", "--dry-run"]);
    assert_eq!(preview["removed"], serde_json::json!(["clean"]));
    assert_eq!(
        preview["kept"],
        serde_json::json!([{"workspace": "dirty", "reason": {"reason": "dirty"}}])
    );
    let text = fixture.run(&["cleanup", "--dry-run"]);
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains("Would remove clean\nKept:\n  dirty  Uncommitted or untracked changes\n"),
        "{text:?}"
    );
    assert!(Path::new(clean["path"].as_str().unwrap()).exists());
    let report = fixture.ok(&["cleanup"]);
    assert_eq!(report["removed"], serde_json::json!(["clean"]));
    assert_eq!(report["retained"], serde_json::json!([]));
    assert_eq!(report["kept"], serde_json::json!([]));
    assert!(!Path::new(clean["path"].as_str().unwrap()).exists());
    let names: Vec<_> = fixture
        .ok(&["work"])
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].clone())
        .collect();
    assert_eq!(names, ["dirty"]);
}

#[test]
fn export_and_import_move_a_workspace_through_a_file_or_a_pipe() {
    let fixture = Fixture::with_config(Some(
        "[commands]\nclaude = ['sh', './agent.sh']\n[agent_resume]\nclaude = ['sh', './resume.sh']\n",
    ));
    let workspace = fixture.add("moving");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(path.join("notes"), "draft\n").unwrap();
    // When the agent starts again after the move, it records its arguments
    // and runs until the test releases it, or for at most two minutes.
    fs::write(
        path.join("agent.sh"),
        "if [ -e agent-started ]; then printf '%s\\n' \"$@\" > agent-args; \
         i=0; while [ ! -e release ] && [ $i -lt 120 ]; do sleep 1; i=$((i + 1)); done; \
         exit 0; fi\n\
         touch agent-started\ntrap 'exit 0' TERM\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::write(path.join("resume.sh"), "touch resumed\n").unwrap();
    let agent = fixture
        .command()
        .args(["claude", "moving"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("agent started", || path.join("agent-started").exists());

    let inside = path.join("moving.shoal");
    let refused = fixture.run(&["export", "moving", "-o", inside.to_str().unwrap()]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("inside the workspace"),
        "{refused:?}"
    );
    assert!(path.exists() && !inside.exists());
    let file = fixture.root.path().join("moving.shoal");
    let file = file.to_str().unwrap();
    let exported = fixture.ok(&["export", "moving", "-o", file]);
    assert!(agent.wait_with_output().unwrap().status.success());
    assert_eq!(exported["manifest"]["workspace"]["name"], "moving");
    assert!(exported["manifest"]["bundle"].as_bool().unwrap());
    let agents = exported["manifest"]["workspace"]["agents"]
        .as_array()
        .unwrap();
    assert_eq!(agents.len(), 1, "{exported}");
    assert_eq!(agents[0]["record"]["moved"], true);
    assert!(!path.exists());
    let again = fixture.run(&["export", "moving", "-o", file]);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("exists; choose another file"));
    // The repository has no origin to select the importing one by.
    let unselected = fixture.run(&["import", file]);
    assert!(!unselected.status.success());
    assert!(String::from_utf8_lossy(&unselected.stderr).contains("--repo"));

    let repo = fixture.repo.to_str().unwrap();
    let imported = fixture.ok(&["import", file, "--repo", repo]);
    assert_eq!(imported["workspace"]["name"], "moving");
    assert_eq!(imported["workspace"]["state"], "ready");
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
    assert_eq!(fixture.ok(&["restore"]), serde_json::json!([]));
    // The agent never reported a session, so it starts as a new agent
    // does, told the workspace moved.
    let record = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(imported["workspace"]["id"].as_str().unwrap())
        .join(format!(
            "{}.recovery.json",
            agents[0]["execution"].as_str().unwrap()
        ));
    assert!(record.exists());
    let resumed = fixture
        .command()
        .args(["resume", "moving"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("agent started again", || path.join("agent-args").exists());
    // Its wrapper consumes the moved record once the agent registers, while
    // the agent still runs.
    wait_until("moved record consumed", || !record.exists());
    fs::write(path.join("release"), "").unwrap();
    let resumed = resumed.wait_with_output().unwrap();
    assert!(resumed.status.success(), "{resumed:?}");
    let args = fs::read_to_string(path.join("agent-args")).unwrap();
    assert!(args.contains("--append-system-prompt\n"), "{args}");
    assert!(args.contains("\nShoal moved this workspace"), "{args}");
    assert!(!path.join("resumed").exists());

    let exported = fixture
        .command()
        .args(["export", "moving", "-o", "-"])
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let mut import = fixture
        .command()
        .args(["--json", "import", "-", "--repo", repo])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    import
        .stdin
        .take()
        .unwrap()
        .write_all(&exported.stdout)
        .unwrap();
    let imported = import.wait_with_output().unwrap();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let imported: Value = serde_json::from_slice(&imported.stdout).unwrap();
    assert_eq!(imported["workspace"]["name"], "moving");
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
}

#[test]
fn export_all_checks_every_workspace_first_and_import_takes_the_directory() {
    let fixture = Fixture::new();
    // Once the test arms it, the post-setup hook fails in the second workspace.
    let armed = fixture.root.path().join("fail-post-setup");
    install_test_script(
        fixture.repo.join("post-setup.sh"),
        format!(
            "#!/bin/sh\nif [ -e '{}' ]; then case \"$PWD\" in */second) exit 3;; esac; fi\n",
            armed.display()
        ),
    );
    commit_resource_config(&fixture.repo, "post_setup_cmd = 'post-setup.sh'\n");
    let paths: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|name| {
            let workspace = fixture.add(name);
            let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
            fs::write(path.join("notes"), format!("{name}\n")).unwrap();
            path
        })
        .collect();
    fixture.ok(&["hold", "acquire", "second", "--name", "review"]);
    let repo = fixture.repo.to_str().unwrap();
    let directory = fixture.root.path().join("exports");
    let export = |extra: &[&str]| {
        let mut args = vec!["export", "--repo", repo, "--all", "-o"];
        args.push(directory.to_str().unwrap());
        args.extend(extra);
        fixture.run(&args)
    };

    // The hold on one workspace keeps both.
    let refused = export(&[]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("pass --yes"),
        "{refused:?}"
    );
    assert!(paths.iter().all(|path| path.exists()));
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    // So does a file that one of them would overwrite.
    fs::write(directory.join("first.shoal"), "").unwrap();
    let refused = export(&["--yes"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("first: ") && stderr.contains("exists"),
        "{stderr}"
    );
    assert!(paths.iter().all(|path| path.exists()));
    fs::remove_file(directory.join("first.shoal")).unwrap();
    // So does one still in a lifecycle operation, which removal refuses.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let set_state = |state: &str| {
        db.execute(
            "UPDATE workspaces SET state=?1 WHERE name='second'",
            [state],
        )
        .unwrap();
    };
    set_state("preparing");
    let refused = export(&["--yes"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("second: workspace is preparing"),
        "{stderr}"
    );
    assert!(paths.iter().all(|path| path.exists()));
    set_state("ready");

    let exported = export(&["--yes", "--json"]);
    assert!(exported.status.success(), "{exported:?}");
    let exported: Value = serde_json::from_slice(&exported.stdout).unwrap();
    let names: Vec<_> = exported["exported"]
        .as_array()
        .unwrap()
        .iter()
        .map(|export| export["manifest"]["workspace"]["name"].clone())
        .collect();
    assert_eq!(names, ["first", "second"]);
    assert_eq!(exported["failed"], serde_json::json!([]));
    assert!(paths.iter().all(|path| !path.exists()));
    assert!(directory.join("first.shoal").exists() && directory.join("second.shoal").exists());

    // A file that is no export fails alone; the others import, and a failed
    // post-setup hook fails its file without removing the workspace.
    fs::write(directory.join("broken.shoal"), "not an export\n").unwrap();
    fs::write(&armed, "").unwrap();
    let imported = fixture
        .command()
        .args([
            "--json",
            "import",
            directory.to_str().unwrap(),
            "--repo",
            repo,
        ])
        .output()
        .unwrap();
    assert_eq!(imported.status.code(), Some(1), "{imported:?}");
    let imported: Value = serde_json::from_slice(&imported.stdout).unwrap();
    let names: Vec<_> = imported["imported"]
        .as_array()
        .unwrap()
        .iter()
        .map(|import| import["workspace"]["name"].clone())
        .collect();
    assert_eq!(names, ["first", "second"]);
    let failed: Vec<_> = imported["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|failure| {
            Path::new(failure["file"].as_str().unwrap())
                .file_name()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(failed, ["broken.shoal", "second.shoal"], "{imported}");
    assert!(
        imported["failed"][1]["error"]
            .as_str()
            .unwrap()
            .contains("imported second, but its post_setup_cmd failed"),
        "{imported}"
    );
    for (path, name) in paths.iter().zip(["first", "second"]) {
        assert_eq!(
            fs::read_to_string(path.join("notes")).unwrap(),
            format!("{name}\n")
        );
    }
}

#[test]
fn streaming_export_refuses_a_herdr_tab_that_closes_with_the_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.add("tabbed");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    // The tab's watcher records that the tab closes with the workspace.
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspace["id"].as_str().unwrap());
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("closing-tab-tab-7"), "").unwrap();
    let export = |tab: &str| {
        fixture
            .command()
            .args(["export", "tabbed", "-o", "-"])
            .env("HERDR_TAB_ID", tab)
            .output()
            .unwrap()
    };
    let refused = export("tab-7");
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("closes when the export removes"),
        "{refused:?}"
    );
    assert!(refused.stdout.is_empty());
    assert!(path.exists());
    // Another tab keeps its stream.
    let exported = export("tab-8");
    assert!(exported.status.success(), "{exported:?}");
    assert!(exported.stdout.starts_with(b"shoal workspace 1\n"));
    assert!(!path.exists());
}

#[test]
fn export_finishes_when_its_terminal_ends_after_the_removal() {
    // The removal hook holds the daemon between removal and the export, as
    // long as closing a workspace's Herdr tab would take.
    let fixture = Fixture::with_config(Some("post_remove_cmd = 'hold.sh'\n"));
    install_test_script(
        fixture.repo.join("hold.sh"),
        "#!/bin/sh\ntouch removed\ni=0\nwhile [ ! -e go ] && [ $i -lt 50 ]; do sleep 1; i=$((i + 1)); done\n",
    );
    let workspace = fixture.add("cut");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(path.join("notes"), "draft\n").unwrap();
    let file = fixture.root.path().join("cut.shoal");
    let mut export = fixture
        .command()
        .args(["export", "cut", "-o", file.to_str().unwrap()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("workspace removed", || {
        fixture.repo.join("removed").exists()
    });
    export.kill().unwrap();
    export.wait().unwrap();
    fs::write(fixture.repo.join("go"), "").unwrap();

    wait_until("export written", || file.exists());
    let repo = fixture.repo.to_str().unwrap();
    let imported = fixture.ok(&["import", file.to_str().unwrap(), "--repo", repo]);
    assert_eq!(imported["workspace"]["name"], "cut");
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
}

#[test]
fn restore_reopens_a_removed_workspace_with_its_unsaved_files() {
    let fixture = Fixture::new();
    let workspace = fixture.add("scratch");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(path.join("notes"), "draft\n").unwrap();
    fixture.ok(&["rm", "scratch", "--yes"]);
    assert!(!path.exists());
    let removed = fixture.ok(&["restore"]);
    assert_eq!(removed[0]["name"], "scratch");
    assert_eq!(removed[0]["cause"], "manual");
    let listing = String::from_utf8(fixture.run(&["restore"]).stdout).unwrap();
    assert!(
        listing.starts_with("scratch  scratch  removed "),
        "{listing:?}"
    );

    let restored = fixture.ok(&["restore", "scratch"]);
    assert_eq!(restored["workspace"]["name"], "scratch");
    assert_eq!(restored["workspace"]["state"], "ready");
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
    assert_eq!(fixture.ok(&["restore"]), serde_json::json!([]));
    let missing = fixture.run(&["restore", "missing"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no restorable workspace missing"));

    // The restored workspace keeps the removal's checkpoint, and returns to
    // any checkpoint after keeping the state it replaces.
    let checkpoints = fixture.ok(&["restore", "scratch"]);
    assert_eq!(checkpoints[0]["number"], 1);
    assert_eq!(checkpoints[0]["cause"], "removal");
    let checkpoint = fixture.ok(&["checkpoint", "scratch", "--name", "draft"]);
    assert_eq!(checkpoint["number"], 2);
    fs::write(path.join("notes"), "rewritten\n").unwrap();
    let restored = fixture.ok(&["restore", "scratch", "2"]);
    assert_eq!(restored["replaced"]["number"], 3);
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
    let listing = String::from_utf8(fixture.run(&["restore", "scratch"]).stdout).unwrap();
    let causes: Vec<_> = listing
        .lines()
        .map(|line| line.split("  ").nth(2).unwrap_or_default())
        .collect();
    assert_eq!(
        causes,
        ["removal", "manual", "before restore"],
        "{listing:?}"
    );
    assert!(
        listing.lines().nth(1).unwrap().ends_with("scratch  draft"),
        "{listing:?}"
    );
}

#[test]
fn restore_lists_removed_workspaces_beyond_one_protocol_frame() {
    let fixture = Fixture::new();
    fixture.add("scratch");
    fixture.ok(&["rm", "scratch", "--yes"]);
    // Copies of the record with long checkpoint names add up to more than
    // the 64 KiB frame.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "WITH RECURSIVE copies(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM copies WHERE n < 40)
         INSERT INTO removed_workspaces(workspace_id,repository_id,name,expires_at,record)
         SELECT 'copy-' || n, repository_id, 'copy-' || n, expires_at,
             json_set(record, '$.workspace_id', 'copy-' || n, '$.name', 'copy-' || n,
                 '$.checkpoint.name', replace(hex(zeroblob(1000)), '0', 'x'))
         FROM removed_workspaces, copies",
        [],
    )
    .unwrap();
    let removed = fixture.ok(&["restore"]);
    let names: Vec<_> = removed
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| workspace["name"].as_str().unwrap().to_owned())
        .collect();
    let expected: Vec<_> = (1..=40)
        .rev()
        .map(|n| format!("copy-{n}"))
        .chain(["scratch".to_owned()])
        .collect();
    assert_eq!(names, expected);
}

#[test]
fn restore_pickers_reopen_the_picked_workspace_and_return_to_the_picked_checkpoint() {
    let fixture = Fixture::new();
    let picked = fixture.add("picked");
    fixture.add("kept");
    let path = Path::new(picked["path"].as_str().unwrap()).to_owned();
    fs::write(path.join("notes"), "draft\n").unwrap();
    fixture.ok(&["rm", "picked", "--yes"]);
    fixture.ok(&["rm", "kept", "--yes"]);

    let (output, rows) = fixture.pick(&["restore"], picked["id"].as_str().unwrap(), "");
    assert!(output.status.success(), "{output:?}");
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[1].starts_with(&format!(
            "{}\tpicked  picked  removed ",
            picked["id"].as_str().unwrap()
        )),
        "{rows:?}"
    );
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
    let removed = fixture.ok(&["restore"]);
    let names: Vec<_> = removed
        .as_array()
        .unwrap()
        .iter()
        .map(|w| &w["name"])
        .collect();
    assert_eq!(names, ["kept"]);

    fixture.ok(&["checkpoint", "picked", "--name", "draft"]);
    fs::write(path.join("notes"), "rewritten\n").unwrap();
    let (output, rows) = fixture.pick(&["restore", "picked"], "2", "");
    assert!(output.status.success(), "{output:?}");
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[1].starts_with("2\t2  ") && rows[1].ends_with("picked  draft"),
        "{rows:?}"
    );
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
}

#[test]
fn resume_continues_the_agent_a_removal_stopped_once_restored() {
    let fixture = Fixture::with_config(Some(
        "[commands]\nclaude = ['sh', './agent.sh']\n[agent_resume]\nclaude = ['sh', './resume.sh']\n",
    ));
    let workspace = fixture.add("removed");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(
        path.join("agent.sh"),
        "touch agent-started\ntrap 'exit 0' TERM\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::write(path.join("resume.sh"), "touch resumed\n").unwrap();
    let spawn = |args: &[&str]| {
        fixture
            .command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let agent = spawn(&["claude", "removed"]);
    let command = spawn(&[
        "exec",
        "removed",
        "--",
        "sh",
        "-c",
        "touch command-started; while :; do sleep 1; done",
    ]);
    assert!(
        try_wait_for(|| (path.join("agent-started").exists()
            && path.join("command-started").exists())
        .then_some(()))
        .is_some(),
        "executions did not start"
    );

    fixture.ok(&["rm", "removed", "--yes"]);
    let stopped = String::from_utf8(agent.wait_with_output().unwrap().stderr).unwrap();
    assert!(
        stopped.contains("restore with shoal restore removed, then shoal resume removed"),
        "{stopped}"
    );
    let stopped = String::from_utf8(command.wait_with_output().unwrap().stderr).unwrap();
    assert!(!stopped.contains("shoal resume"), "{stopped}");
    assert!(!path.exists());
    let removed = fixture.ok(&["restore"]);
    let agents = removed[0]["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1, "{removed}");
    assert_eq!(agents[0]["record"]["agent"], "claude");

    let restored = fixture.ok(&["restore", "removed"]);
    assert_eq!(restored["warnings"], serde_json::json!([]));
    assert_eq!(restored["workspace"]["path"], workspace["path"]);
    let execution = agents[0]["execution"].as_str().unwrap();
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(restored["workspace"]["id"].as_str().unwrap());
    // The interrupted command is not saved, so only the agent resumes.
    let saved = fs::read_dir(&state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".recovery.json") || name.ends_with(".command.json"))
        .collect::<Vec<_>>();
    assert_eq!(saved, [format!("{execution}.recovery.json")]);
    let resumed = fixture.run(&["resume", "removed"]);
    assert!(resumed.status.success(), "{resumed:?}");
    assert!(path.join("resumed").exists());
    assert!(!state.join(format!("{execution}.recovery.json")).exists());
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
    assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
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
    let (ambiguous, prompt) = fixture.interactive(&["repo", "rm", "saldoir-server"], "y\n");
    assert!(!ambiguous.status.success());
    assert!(prompt.contains("repository name is ambiguous"), "{prompt}");
    assert!(!prompt.contains("Are you sure?"), "{prompt}");
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
    assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
    assert!(fixture.repo.exists());
}

#[test]
fn repository_selectors_preserve_names_and_reject_colliding_registered_paths() {
    let fixture = Fixture::new();
    let named = fixture.ok(&[
        "repo",
        "rename",
        fixture.repo.to_str().unwrap(),
        "saldoir-server",
    ]);
    let caller = fixture.root.path().join("caller");
    fs::create_dir(&caller).unwrap();
    let unrelated = init_repo(&caller, "saldoir-server", &[("precious", "keep me\n")]);
    git(
        &unrelated,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/unrelated",
        ],
    );
    let command = |args: &[&str]| {
        fixture
            .command()
            .current_dir(&caller)
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    };
    for args in [
        vec!["repo", "add", "saldoir-server"],
        vec!["repo", "config", "saldoir-server"],
        vec![
            "config",
            "set",
            "--repo",
            "saldoir-server",
            "done.cleanup",
            "false",
        ],
        vec!["add", "saldoir-server", "collision"],
    ] {
        let output = command(&args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result[if args[0] == "repo" && args[1] == "add" {
                "id"
            } else {
                "repository_id"
            }],
            named["id"]
        );
    }
    let other = fixture.ok(&[
        "repo",
        "add",
        unrelated.to_str().unwrap(),
        "--name",
        "other",
    ]);
    for args in [
        vec!["repo", "rm", "saldoir-server", "--yes"],
        vec!["repo", "rename", "saldoir-server", "renamed"],
    ] {
        let output = command(&args);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("ambiguous"), "{error}");
        assert!(error.contains(named["id"].as_str().unwrap()), "{error}");
        assert!(error.contains(other["id"].as_str().unwrap()), "{error}");
    }
    fixture.ok(&["repo", "rename", other["id"].as_str().unwrap(), "different"]);
    // A relative path remains usable when its spelling is not a repository name.
    let output = command(&["repo", "config", "./saldoir-server"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(config["repository_id"], other["id"]);
    // Replace the second registration with an unrelated directory to exercise
    // removal by the displayed name.
    fixture.ok(&["repo", "rm", other["id"].as_str().unwrap(), "--yes"]);
    fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("precious"), "keep me\n").unwrap();
    let output = command(&["repo", "rm", "saldoir-server", "--yes"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.repo.exists());
    assert_eq!(
        fs::read_to_string(unrelated.join("precious")).unwrap(),
        "keep me\n"
    );
}

#[test]
fn unregistered_repository_path_diagnostic_keeps_the_typed_argument() {
    let fixture = Fixture::new();
    let caller = fixture.root.path().join("caller");
    fs::create_dir(&caller).unwrap();
    let path = init_repo(&caller, "saldoir-server", &[("tracked", "unregistered\n")]);
    let output = fixture
        .command()
        .current_dir(&caller)
        .args(["repo", "rm", "saldoir-server", "--yes"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("repository is not registered: saldoir-server"),
        "{error}"
    );
    assert!(error.contains("read as a path:"), "{error}");
    assert!(error.contains(path.to_str().unwrap()), "{error}");
}

#[test]
fn setup_completion_exempts_only_the_callers_own_execution() {
    let fixture = Fixture::new();
    fixture.add("first");
    let shoal = env!("CARGO_BIN_EXE_shoal");
    // Completion for setup, run inside `first` through `depth` nested execs.
    let complete = |depth: usize| {
        let mut words = vec![];
        for _ in 0..depth {
            words.extend([shoal, "exec", "first", "--"]);
        }
        words.extend(["env", "SHOAL_COMPLETE=zsh", "_CLAP_COMPLETE_INDEX=2"]);
        words.extend([shoal, "--", "shoal", "setup", ""]);
        let output = fixture.command().args(&words[1..]).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let own = complete(1);
    assert!(own.lines().any(|line| line == "first"), "{own}");
    let nested = complete(2);
    assert!(
        nested.lines().any(|line| line == "first:running"),
        "{nested}"
    );
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
        vec!["add", "103", "--repo", "pr"],
    ] {
        assert!(
            complete(&args, fixture.root.path()).contains(&"project".into()),
            "{args:?}"
        );
    }
    for command in ["rm", "cd", "exec", "diff", "status", "inspect", "done"] {
        assert!(
            complete(&[command, "fi"], fixture.root.path()).contains(&"first".into()),
            "{command}"
        );
    }
    for words in [
        vec!["link", "pr", "42", "assigned", "--workspace", "fi"],
        vec!["unlink", "pr", "assigned", "--workspace", "fi"],
    ] {
        assert!(complete(&words, fixture.root.path()).contains(&"first".into()));
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
    // `--all` comes once, before a scope's workspaces, until it is typed.
    let choices = complete(&["doctor", ""], fixture.root.path());
    assert_eq!(choices[0], "--all", "{choices:?}");
    assert!(choices.contains(&"first".into()), "{choices:?}");
    assert_eq!(choices.iter().filter(|v| *v == "--all").count(), 1);
    // Resume describes each workspace by what it would restore.
    let resumable = || {
        let output = fixture
            .command()
            .args(["--", "shoal", "resume", ""])
            .env("SHOAL_COMPLETE", "zsh")
            .env("_CLAP_COMPLETE_INDEX", "2")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let choices = resumable();
    assert!(choices.contains("first:nothing to resume"), "{choices}");
    fixture.save_stopped_command(&first);
    let choices = resumable();
    assert!(choices.contains("first:stopped: 1 command"), "{choices}");
    assert!(choices.contains("second:nothing to resume"), "{choices}");
    // Other commands name why they would refuse a workspace.
    let described = |args: &[&str]| {
        let mut words = vec!["shoal"];
        words.extend_from_slice(args);
        let output = fixture
            .command()
            .arg("--")
            .args(&words)
            .env("SHOAL_COMPLETE", "zsh")
            .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    fs::write(
        fixture.root.path().join("zmx-sessions"),
        format!(
            "  name=first\tpid=1\tclients=0\tshoal.workspace={}\n",
            first["id"].as_str().unwrap()
        ),
    )
    .unwrap();
    // Attach leaves out workspaces without a session.
    let choices = described(&["attach", ""]);
    assert!(choices.contains(&"first".into()), "{choices:?}");
    assert!(
        !choices.iter().any(|choice| choice.starts_with("second")),
        "{choices:?}"
    );
    // Workspaces without what the command acts on are left out.
    for args in [
        vec!["stop", ""],
        vec!["undone", ""],
        vec!["unready", "--workspace", ""],
        vec!["hold", "release", ""],
        vec!["base", "clear", "--workspace", ""],
    ] {
        let choices = described(&args);
        assert!(
            !choices.iter().any(|choice| choice.starts_with("second")),
            "{args:?} {choices:?}"
        );
    }
    fixture.ok(&["hold", "acquire", "second", "--name", "review"]);
    assert!(described(&["hold", "release", ""]).contains(&"second".into()));
    // A workspace that is not ready shows its state.
    let failed = fixture
        .command()
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "broken",
            "--base",
            "not-a-ref",
        ])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let choices = described(&["exec", ""]);
    assert!(choices.contains(&"broken:failed".into()), "{choices:?}");
    assert!(choices.contains(&"second".into()), "{choices:?}");
    fixture.ok(&["hold", "release", "second", "--name", "review"]);
    assert!(described(&["exec", ""]).contains(&"second".into()));
    assert!(!complete(&["stop", "--all", ""], fixture.root.path()).contains(&"--all".into()));
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
    assert!(complete(&["acquire", "port", "w"], cwd).contains(&"web".into()));
    assert!(complete(&["release", "port", "w"], cwd).contains(&"web".into()));
    assert!(complete(&["acquire", "resource", "d"], cwd).contains(&"devices".into()));
    assert!(
        complete(
            &[
                "acquire",
                "resource",
                "devices",
                "--workspace",
                "first",
                "--member",
                "b"
            ],
            fixture.root.path()
        )
        .contains(&"beta".into())
    );
    assert!(
        complete(&["release", "resource", "devices", "--name", "t"], cwd).contains(&"tests".into())
    );
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
        for action in ["status", "inspect"] {
            let output = command(cwd, scoped, false)
                .args(["--json", action])
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["workspace"]["id"], own["id"]);
        }

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
        vec!["inspect", "context-other"],
        vec!["inspect", "missing"],
        vec!["stop"],
        vec!["stop", "context-other"],
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
    for args in [vec!["cd"], vec!["--json", "cd"]] {
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing argument"));
}

#[test]
fn status_summarizes_current_workspace_work_and_supports_json() {
    let config = format!("{RESOURCE_CONFIG}\n[cleanup.pr]\nenabled=false\n");
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
    wait_until("execution to start", || started.exists());

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
    assert_eq!(status["prs"][0]["state"], Value::Null);
    assert!(
        status["prs"][0]["errors"][0]
            .as_str()
            .unwrap()
            .contains("origin remote")
    );

    git(
        &fixture.repo,
        &["remote", "add", "origin", "https://forge.example/team/repo"],
    );
    let home = fixture.root.path();
    install_forgejo_curl(&home.join("bin"));
    let sha = "7".repeat(40);
    let mut pull = forgejo_pull(7, "open", "summary", &sha);
    pull["mergeable"] = false.into();
    forgejo_api(home, "repos/team/repo/pulls/7", &pull);
    forgejo_api(
        home,
        &format!("repos/team/repo/commits/{sha}/status?limit=50"),
        &serde_json::json!({"total_count": 2, "statuses": [
            {"context": "ci / rust", "status": "success"}, {"context": "review", "status": "pending"}
        ]}),
    );
    let status = fixture.ok(&["status", "summary"]);
    assert_eq!(
        status["prs"][0]["url"],
        "https://forge.example/team/repo/pulls/7"
    );
    assert_eq!(status["prs"][0]["state"], "open");
    assert_eq!(status["prs"][0]["merge_conflicts"], true);
    assert_eq!(
        status["prs"][0]["checks"],
        serde_json::json!([
            {"name": "ci / rust", "result": "success"},
            {"name": "review", "result": "pending"}
        ])
    );
    assert_eq!(status["prs"][0]["review"], Value::Null);
    assert_eq!(status["cleanup_reason"]["reason"], "pr_watch");
    assert!(
        status["prs"][0]["errors"][0]
            .as_str()
            .unwrap()
            .contains("reviews lookup failed")
    );

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
        "PR watch:      https://forge.example/team/repo/pulls/7\n  State:       open, merge conflicts\n  Checks:      2\n    ci / rust: success\n    review: pending\n  reviews lookup failed",
        "Cleanup:       Waiting for watched PRs to merge",
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

    for target in [
        vec!["pr", "7"],
        vec!["https://forge.example/team/repo/pulls/7/"],
        vec!["resource", "devices"],
        vec!["resource", "signing"],
    ] {
        let found = fixture.ok(&[&["status"], target.as_slice()].concat());
        let found = found.as_array().unwrap();
        assert_eq!(found.len(), 1, "{target:?}");
        assert_eq!(found[0]["workspace"]["name"], "summary");
        assert_eq!(found[0]["prs"][0]["state"], "open");
    }
    for (target, error) in [
        (vec!["pr", "8"], "no workspace links PR 8"),
        (vec!["issue", "7"], "no workspace links issue 7"),
        (
            vec!["https://forge.example/team/other/pulls/7"],
            "no workspace links PR",
        ),
        (
            vec!["resource", "missing"],
            "no workspace holds resource missing",
        ),
        (vec!["pr"], "pr needs a number or URL"),
        (vec!["summary", "7"], "put pr, issue or resource before"),
    ] {
        let output = fixture.run(&[&["status"], target.as_slice()].concat());
        assert!(!output.status.success(), "{target:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(error), "{target:?}: {stderr}");
    }
    let scoped = |workspace: &str| {
        fixture
            .command()
            .args(["exec", workspace, "--", env!("CARGO_BIN_EXE_shoal")])
            .args(["--json", "status", "resource", "devices"])
            .output()
            .unwrap()
    };
    let own = scoped("summary");
    assert!(own.status.success());
    let own: Value = serde_json::from_slice(&own.stdout).unwrap();
    assert_eq!(own[0]["workspace"]["name"], "summary");
    let other = scoped("waiter");
    assert!(!other.status.success());
    assert!(String::from_utf8_lossy(&other.stderr).contains("no workspace holds resource devices"));

    fs::write(finish, "done").unwrap();
    assert!(execution.wait().unwrap().success());
}

#[test]
fn status_keeps_shared_state_when_the_worktree_is_missing() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
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
fn root_and_clone_directories_the_daemon_cannot_write_are_refused() {
    let mut fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let blocked = fixture.root.path().join("file");
    fs::write(&blocked, "").unwrap();
    let clone = blocked.join("clone");
    let output = fixture.run(&["repo", "add", &url, "--path", clone.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains(&format!(
        "the Shoal daemon cannot write to {}",
        blocked.display()
    )));
    let config = fixture.root.path().join(".config/shoal/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "root_dir = \"~/file\"\n").unwrap();
    fixture.restart();
    let output = fixture.run(&["repo", "add", &url]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("the Shoal daemon cannot write to"),
        "{stderr}"
    );
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
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
fn migrate_moves_workspaces_into_a_saved_root() {
    let fixture = Fixture::new();
    let workspace = fixture.add("moving");
    let old = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    let root = fs::canonicalize(fixture.root.path()).unwrap().join("moved");

    // A process inside is refused before confirmation is asked for.
    let mut inside = Command::new("sleep")
        .arg("60")
        .current_dir(&old)
        .spawn()
        .unwrap();
    let output = fixture.run(&["repo", "migrate", "~/moved"]);
    inside.kill().unwrap();
    inside.wait().unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("processes are using"), "{error}");
    assert!(!error.contains("confirmation required"), "{error}");
    let output = fixture.run(&["repo", "migrate", "~/moved"]);
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("confirmation required"), "{error}");
    assert!(old.exists());

    let moved = fixture.ok(&["repo", "migrate", "~/moved", "--yes"]);
    let [moved] = moved.as_array().unwrap().as_slice() else {
        panic!("expected one move: {moved}");
    };
    let directory = Path::new(moved["repository"]["workspaces_dir"].as_str().unwrap());
    assert_eq!(directory.parent(), Some(root.as_path()));
    assert!(!old.exists());
    let config = fs::read_to_string(fixture.root.path().join(".config/shoal/config.toml")).unwrap();
    assert!(
        config.contains(&format!(
            "root_dir = {:?}",
            fixture.root.path().join("moved")
        )),
        "{config}"
    );

    // The moved workspace still runs commands, and new ones go to the new root.
    let path = directory.join(old.file_name().unwrap());
    let output = fixture.run(&["exec", "moving", "--", "git", "status", "--short"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(path.join("tracked").exists());
    let next = fixture.add("next");
    assert!(Path::new(next["path"].as_str().unwrap()).starts_with(directory));
    assert_eq!(
        fixture.ok(&["repo", "migrate", "--yes"]),
        serde_json::json!([])
    );
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
    fixture.ok(&["rm", "dirty", "--yes"]);
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
fn failed_creation_cannot_adopt_an_unrelated_worktree_at_its_reserved_path() {
    let fixture = Fixture::new();
    let failed = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "reserved",
        "--base",
        "does-not-exist",
    ]);
    assert!(!failed.status.success());
    let reserved = PathBuf::from(
        fixture.ok(&["inspect", "reserved"])["workspace"]["path"]
            .as_str()
            .unwrap(),
    );
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "unrelated",
            reserved.to_str().unwrap(),
            "main",
        ],
    );
    let report = recovery_report(&fixture, &["doctor", "reserved", "--repair"]);
    assert_eq!(report[0]["directory"], "unverified");
    assert!(report[0]["issues"].to_string().contains("ownership"));
    let removal = fixture.run(&["rm", "reserved", "--yes"]);
    assert!(!removal.status.success());
    assert!(reserved.exists());
}

#[test]
fn removal_keeps_an_unpushed_branch_without_asking_and_ignores_external_processes() {
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
    assert_eq!(fixture.ok(&["rm", "unpushed"])["branch_deleted"], false);
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
fn kept_branches_are_listed_and_deleted_on_request() {
    let fixture = Fixture::new();
    for name in ["first", "second", "third"] {
        let workspace = fixture.add(name);
        commit_file(Path::new(workspace["path"].as_str().unwrap()), name, name);
        assert_eq!(fixture.ok(&["rm", name])["branch_deleted"], false);
    }
    let names = |listed: &Value| -> Vec<String> {
        listed
            .as_array()
            .unwrap()
            .iter()
            .map(|branch| branch["branch"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        names(&fixture.ok(&["branch"])),
        ["first", "second", "third"]
    );
    let branch_exists = |name: &str| {
        !git(&fixture.repo, &["branch", "--list", name])
            .trim()
            .is_empty()
    };

    let piped = fixture.run(&["branch", "rm", "first"]);
    assert!(!piped.status.success());
    assert!(String::from_utf8_lossy(&piped.stderr).contains("pass --yes"));
    let missing = fixture.run(&["branch", "rm", "missing", "--yes"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("not a kept branch"));
    assert!(branch_exists("first"));

    // The picker selects several branches at once.
    let picker = fixture.root.path().join("bin/fzf");
    install_test_script(
        &picker,
        "#!/bin/sh\nawk -F '\\t' '$2 ~ /(first|second) /'\n",
    );
    let (output, transcript) = fixture.interactive(&["branch", "rm"], "y\n");
    assert!(output.status.success(), "{transcript}");
    assert!(transcript.contains("1 unpushed commit"), "{transcript}");
    assert!(!branch_exists("first") && !branch_exists("second"));
    assert!(branch_exists("third"));

    let deleted = fixture.ok(&["branch", "rm", "--all", "--yes"]);
    assert_eq!(names(&deleted), ["third"]);
    assert!(!branch_exists("third"));
    assert!(names(&fixture.ok(&["branch"])).is_empty());
}

#[test]
fn branch_removal_deletes_branches_main_or_a_remote_has() {
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
    // Automatic removal keeps branches while the default branch is unknown.
    git(
        &fixture.repo,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
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
    assert_eq!(fixture.ok(&["rm", "divergent"])["branch_deleted"], false);
    assert!(!path.exists());
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
    fixture.ok(&["rm", "first", "--yes"]);
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
    // Cross the small-list membership threshold while claims compete.
    for i in 0..40 {
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
    install_test_script(&external, "#!/bin/sh\nprintf 'configured-diff\\n'\n");
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
    fixture.ok(&["rm", "changes", "--yes"]);
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

/// An upstream remote served at ssh://git@forge.example/team/project.git;
/// returns the author's clone.
fn forge_origin(fixture: &Fixture) -> PathBuf {
    let author = upstream_remote(fixture);
    let remote = fixture.root.path().join("origin.git");
    let ssh = fixture.root.path().join("ssh");
    install_test_script(
        &ssh,
        format!(
            "#!/bin/sh\nfor last; do :; done\nexec sh -c \"$(printf '%s' \"$last\" | sed 's|/team/project.git|{}|')\"\n",
            remote.display()
        ),
    );
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
    author
}

#[test]
fn review_refines_an_issue_in_the_workspace_add_continues() {
    let fixture = Fixture::with_config(Some(
        "default_agent = 'reviewer'\n[commands]\nreviewer = ['printf', '%s', '{prompt}']\n",
    ));
    forge_origin(&fixture);
    let bin = fixture.root.path().join("issue-bin");
    install_forgejo_curl(&bin);
    forgejo_api(
        fixture.root.path(),
        "repos/team/project/issues/12",
        &serde_json::json!({"number": 12, "title": "Clarify retries", "state": "open",
            "body": "Retry failed uploads.", "labels": [], "pull_request": null}),
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let review = |target: &[&str]| {
        fixture
            .command()
            .arg("review")
            .args(target)
            .current_dir(&fixture.repo)
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    let name = "12-clarify-retries";
    let output = review(&["--issue", "12"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "Review issue #12: Clarify retries\nhttps://forge.example/team/project/issues/12",
        "Retry failed uploads.",
        "Refine the issue before implementation starts; this workspace is on branch 12-clarify-retries.",
        "Post this as one comment on issue #12 without editing the issue.",
    ] {
        assert!(stdout.contains(expected), "{stdout}");
    }
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains(&format!("Reviewing issue #12 in workspace {name}")),
        "{output:?}"
    );
    assert_eq!(
        fixture.ok(&["inspect", name])["issue"]["url"],
        "https://forge.example/team/project/issues/12"
    );

    let output = review(&["https://forge.example/team/project/issues/12", "--no-post"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("or comment on the forge unless asked"),
        "{output:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains(&format!("Reviewing issue #12 in workspace {name}")),
        "{output:?}"
    );
    let output = review(&["https://forge.example/team/project/issues/12", "--manual"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("an issue review needs an agent"),
        "{output:?}"
    );
    let added = fixture
        .command()
        .args([
            "--json",
            "add",
            "https://forge.example/team/project/issues/12",
            "--agent",
            "reviewer",
        ])
        .current_dir(&fixture.repo)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(added.status.success(), "{added:?}");
    assert_eq!(
        serde_json::from_str::<Value>(
            String::from_utf8_lossy(&added.stdout)
                .lines()
                .next()
                .unwrap()
        )
        .unwrap()["name"],
        name
    );
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
}

#[test]
fn review_opens_a_pr_head_against_its_base_and_reuses_the_owner() {
    let fixture = Fixture::with_config(Some(
        "default_agent = 'reviewer'\n[commands]\nreviewer = ['printf', '%s', '{prompt}']\n",
    ));
    let author = forge_origin(&fixture);
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

    // The daemon also queries the PR when linking it, through the fixture's bin.
    let bin = fixture.root.path().join("bin");
    install_forgejo_curl(&bin);
    let mut pull = forgejo_pull(7, "open", "stack/top", &"7".repeat(40));
    pull["title"] = "Top change".into();
    pull["base"]["ref"] = "stack/base".into();
    pull["head"]["repo"]["full_name"] = "team/project".into();
    forgejo_api(fixture.root.path(), "repos/team/project/pulls/7", &pull);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let review = |target: &[&str]| {
        fixture
            .command()
            .arg("review")
            .args(target)
            .args(["--agent", "reviewer"])
            .current_dir(&fixture.repo)
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    let output = review(&["--pr", "7"]);
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
        stdout.contains("Post them as one comment on pull request #7"),
        "{stdout}"
    );
    assert!(
        fs::read_to_string(fixture.root.path().join("curl-urls"))
            .unwrap()
            .contains("https://forge.example/api/v1/repos/team/project/pulls/7\n")
    );
    let url = "https://forge.example/team/project/pulls/7";
    let linked_prs = || fixture.ok(&["inspect", "stack-top"])["workspace"]["links"]["prs"].clone();
    assert_eq!(linked_prs(), serde_json::json!([url]));
    // Opening the PR again relinks it to its owner.
    fixture.ok(&["unlink", "pr", url, "assigned", "--workspace", "stack-top"]);
    assert_eq!(linked_prs(), serde_json::json!([]));
    let added = fixture
        .command()
        .args([
            "--json",
            "add",
            "https://forge.example/team/project/pulls/7",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(added.status.success(), "{added:?}");
    assert_eq!(linked_prs(), serde_json::json!([url]));
    let added: Value = serde_json::from_slice(&added.stdout).unwrap();
    assert_eq!(added["branch"], "stack/top");
    assert_eq!(added["links"]["prs"], serde_json::json!([url]));
    let branch = fixture.ok(&[
        "add",
        "https://forge.example/team/project/src/branch/stack%2Ftop",
    ]);
    assert_eq!(branch["id"], added["id"]);
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

    // Reviewing in the owner links the PR as well.
    fixture.ok(&["unlink", "pr", url, "assigned", "--workspace", "stack-top"]);
    commit("more");
    git(&author, &["push", "origin", "stack/top"]);
    let output = review(&["https://forge.example/team/project/pulls/7", "--no-post"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(linked_prs(), serde_json::json!([url]));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("or comment on the forge unless asked"),
        "{output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Updated workspace stack-top to refs/remotes/origin/stack/top"),
        "{output:?}"
    );
    assert!(
        stderr.contains("Reviewing PR #7 in workspace stack-top"),
        "{output:?}"
    );
    assert!(path_in.join("more").exists());
    // A diverged workspace is left for the reviewer to reconcile.
    git(path_in, &["reset", "--hard", "HEAD~1"]);
    fs::write(path_in.join("local"), "change\n").unwrap();
    git(path_in, &["add", "local"]);
    git(
        path_in,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "local",
        ],
    );
    let output = review(&["--pr", "7"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("has diverged from"),
        "{output:?}"
    );
    let output = review(&["https://forge.example/other/project/pulls/7"]);
    assert!(!output.status.success());
}

#[test]
fn repository_config_sets_the_automatic_port_range() {
    let fixture = Fixture::new();
    let workspace = fixture.add("ranged");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let [first, second] = [(); 2].map(|()| ports::hold()).map(|held| held.port);
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
         [cleanup.auto]\nenabled = false\n[ports]\nstart = 2000\nend = 6000\n",
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
            "cleanup.auto.enabled",
            serde_json::json!(false),
            "global_config",
        ),
        (
            "codex.default_mode",
            serde_json::json!("cli"),
            "built_in_default",
        ),
        (
            "cleanup.auto.idle_minutes",
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
    let held = ports::hold();
    let number = held.port;
    let fixture = Fixture::with_config(Some(&format!("[ports]\nstart={number}\nend={number}\n")));
    fixture.add("limited");
    assert!(
        !fixture
            .run(&["port", "acquire", "web", "limited"])
            .status
            .success()
    );
    drop(held);
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
        assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
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
        assert_eq!(fixture.ok(&["rm", name])["branch_deleted"], true);
    }
}

#[test]
fn split_refuses_a_detached_agent_without_zmx_before_creating_the_child() {
    let fixture = Fixture::with_config(Some("[ai.droid]\ncommand = ['fake-droid', '{args}']\n"));
    let bin = fixture.root.path().join("bin");
    install_test_script_with_mode(bin.join("fake-droid"), "#!/bin/sh\nexit 0\n", 0o700);
    fixture.add("parent");
    // Outside Herdr the daemon starts the agent in a detached zmx session.
    let output = fixture
        .command()
        .args([
            "split",
            "child",
            "--workspace",
            "parent",
            "--agent",
            "droid",
        ])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), fixture.system_path()),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("zmx is not installed"), "{stderr}");
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
}

#[test]
fn split_refuses_an_unavailable_agent_before_creating_the_child() {
    let fixture = Fixture::new();
    fixture.add("parent");
    let output = fixture.run(&[
        "split",
        "child",
        "--workspace",
        "parent",
        "--agent",
        "droid",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown agent \"droid\""), "{stderr}");
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
}

#[test]
fn split_rejects_an_invalid_branch_before_choosing_a_parent() {
    let fixture = Fixture::new();
    fixture.add("parent");
    // Without a current workspace a parent would need a picker or a target.
    let output = fixture.run(&["split", "bad..name"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid Git branch name"), "{stderr}");
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
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
        assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
    }
    git(&fixture.repo, &["branch", "-d", "previous"]);
    let existing = fixture.add("feature/topic");
    for branch in ["feature-topic", "feature.topic"] {
        let output = fixture.run(&["add", fixture.repo.to_str().unwrap(), branch]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("feature-topic"));
    }
    assert_eq!(fixture.ok(&["work"]), serde_json::json!([existing]));
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
fn execution_and_hooks_restore_terminal_settings_after_exit_and_stop() {
    use std::io::Read;

    let fixture = Fixture::new();
    let workspace = fixture.add("terminal");
    let agent = fixture.root.path().join("raw-agent.py");
    install_test_script(
        &agent,
        r#"#!/usr/bin/env python3
import os, pathlib, signal, sys, time, tty
mode = sys.argv[1] if len(sys.argv) > 1 else 'success'
tty.setraw(0)
# Emulator modes are independent of termios. Write to the terminal even when
# the command's output streams are pipes (as in the driver below).
with open('/dev/tty', 'wb', buffering=0) as terminal:
    for mode_number in (1000, 1002, 1003, 1005, 1006, 1015, 1016, 1004, 1007, 2004, 1):
        terminal.write(f'\x1b[?{mode_number}h'.encode())
    terminal.write(b'\x1b=\x1b[>4;2m\x1b[=15u')
pathlib.Path('raw-ready').touch()
if mode in ('success', 'exit'):
    sys.exit(0 if mode == 'success' else 7)
if mode == 'killed':
    os.kill(os.getpid(), signal.SIGKILL)
while True:
    time.sleep(0.01)
"#,
    );
    let setup = Path::new(workspace["path"].as_str().unwrap()).join("setup.sh");
    install_test_script(&setup, "#!/bin/sh\nexit 0\n");
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join(".shoal.toml"),
        format!(
            "setup_cmd = 'setup.sh'\npost_setup_cmd = '{}'\n",
            agent.display()
        ),
    )
    .unwrap();
    for (mode, term) in [
        ("success", Some("xterm-256color")),
        ("exit", Some("xterm-256color")),
        ("killed", Some("xterm-256color")),
        ("interrupt", Some("xterm-256color")),
        ("stop", Some("xterm-256color")),
        ("hook", Some("xterm-256color")),
        ("success", Some("dumb")),
        ("hook", Some("dumb")),
        ("success", Some("")),
        ("success", None),
    ] {
        let (mut master, slave) = pty::open();
        let mut driver = support::isolated(fixture.root.path(), "python3");
        match term {
            Some(term) => driver.env("TERM", term),
            None => driver.env_remove("TERM"),
        };
        let mut child = driver
            .args([
                "-c",
                r#"import fcntl, os, pathlib, signal, subprocess, sys, termios, time
binary, agent, mode = sys.argv[1:]
os.setsid()
fcntl.ioctl(0, termios.TIOCSCTTY, 0)
# Preserve deliberate caller customization too, rather than resetting to sane.
settings = termios.tcgetattr(0)
settings[6][termios.VEOF] = b'\x1d'
termios.tcsetattr(0, termios.TCSANOW, settings)
def terminal_settings():
    settings = termios.tcgetattr(0)
    # PENDIN describes pending input reprocessing, which the kernel can set
    # when canonical mode is restored; it is not a caller-controlled setting.
    settings[3] &= ~getattr(termios, 'PENDIN', 0)
    return settings
before = terminal_settings()
group = os.tcgetpgrp(0)
command = [binary, 'setup', 'terminal'] if mode == 'hook' else [
    binary, 'exec', 'terminal', '--', 'python3', agent, mode]
child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
try:
    if mode in ('interrupt', 'stop'):
        import json
        workspace = pathlib.Path(json.loads(subprocess.check_output(
            [binary, '--json', 'inspect', 'terminal'], text=True))['workspace']['path'])
        deadline = time.monotonic() + 10
        while not (workspace / 'raw-ready').exists():
            assert child.poll() is None, child.communicate()
            assert time.monotonic() < deadline, 'agent did not enter raw mode'
            time.sleep(0.01)
        if mode == 'interrupt':
            child.send_signal(signal.SIGTERM)
        else:
            subprocess.run([binary, 'stop', 'terminal'], check=True, capture_output=True, timeout=15)
    stdout, stderr = child.communicate(timeout=15)
    expected = {'success': 0, 'exit': 7, 'killed': 137, 'interrupt': 143, 'stop': 143, 'hook': 0}[mode]
    assert child.returncode == expected, (child.returncode, stdout, stderr)
    assert b'\x1b' not in stdout + stderr, (stdout, stderr)
    assert os.tcgetpgrp(0) == group, 'foreground group was not restored'
    assert terminal_settings() == before, (mode, before, terminal_settings())
finally:
    if child.poll() is None:
        child.kill()
        child.wait()
"#,
                env!("CARGO_BIN_EXE_shoal"),
                agent.to_str().unwrap(),
                mode,
            ])
            .stdin(slave.try_clone().unwrap())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut transcript = String::new();
        if try_wait_for(|| {
            // Drain while the driver runs: closing a PTY can wait for unread
            // output on macOS, including when the driver has been killed.
            let _ = master.read_to_string(&mut transcript);
            child.try_wait().unwrap()
        })
        .is_none()
        {
            let _ = child.kill();
            let _ = master.read_to_string(&mut transcript);
            let _ = child.wait();
            panic!("terminal driver hung: {mode} {term:?}: {transcript:?}");
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{mode} {term:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // The nonblocking master may report WouldBlock or EIO once drained.
        let _ = master.read_to_string(&mut transcript);
        let resets_input = term == Some("xterm-256color");
        for number in [
            1000, 1002, 1003, 1005, 1006, 1015, 1016, 1004, 1007, 2004, 1,
        ] {
            let enabled = transcript.rfind(&format!("\x1b[?{number}h")).unwrap();
            let disabled = transcript.rfind(&format!("\x1b[?{number}l"));
            if resets_input {
                assert!(
                    disabled.unwrap() > enabled,
                    "{mode}: input mode {number} leaked"
                );
            } else {
                assert!(disabled.is_none(), "{term:?}: unnecessary terminal escape");
            }
        }
        for (enabled, disabled) in [
            ("\x1b=", "\x1b>"),
            ("\x1b[>4;2m", "\x1b[>4;0m"),
            ("\x1b[=15u", "\x1b[=0u"),
        ] {
            if resets_input {
                assert!(
                    transcript.rfind(disabled).unwrap() > transcript.rfind(enabled).unwrap(),
                    "{mode}: keyboard mode leaked"
                );
            } else {
                assert!(
                    !transcript.contains(disabled),
                    "{term:?}: unnecessary terminal escape"
                );
            }
        }
        let workspace = fixture.ok(&["inspect", "terminal"]);
        fs::remove_file(
            Path::new(workspace["workspace"]["path"].as_str().unwrap()).join("raw-ready"),
        )
        .unwrap();
    }
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
fn configured_environment_renders_for_tracked_commands() {
    let fixture = Fixture::with_config(Some(
        "[env]\nSESSION = '{workspace}'\nPROFILE = '{workspace_path}/.profile'\nREPO = '{repo}'\nBRANCH = '{branch}'\nLITERAL = '{workspace}'\nAPI_PORT = 'wrong'\nURL = 'http://localhost:{port.api}'\nGOCACHE = '{cache.go}'\n[caches.go]\nscope = 'shared'\n",
    ));
    fixture.add("env-values");
    let port = fixture.ok(&["port", "acquire", "api", "env-values", "--env", "API_PORT"]);
    let output = fixture
        .command()
        .args([
            "exec",
            "env-values",
            "--",
            "sh",
            "-c",
            "printf '%s\\n%s\\n%s\\n%s\\n%s' \"$SESSION\" \"$PROFILE\" \"$REPO\" \"$BRANCH\" \"$LITERAL\"",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let lines: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(lines[0], "env-values");
    assert!(lines[1].ends_with("/env-values/.profile"));
    assert_eq!(lines[2], "repo with ' quotes & $literal");
    assert_eq!(lines[3], "env-values");
    assert_eq!(lines[4], "env-values");
    let output = fixture
        .command()
        .args([
            "exec",
            "env-values",
            "--",
            "sh",
            "-c",
            "printf '%s|%s|%s' \"$API_PORT\" \"$URL\" \"$GOCACHE\"; touch \"$GOCACHE/entry\"",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = String::from_utf8(output.stdout).unwrap();
    let (ports, cache) = output.rsplit_once('|').unwrap();
    assert_eq!(
        ports,
        format!("{}|http://localhost:{}", port["port"], port["port"])
    );
    assert!(cache.ends_with("/.caches/shared/go"), "{cache}");
    let exported = fixture.ok(&["env", "env-values"]);
    assert_eq!(exported["API_PORT"], port["port"].to_string());
    assert_eq!(exported["SESSION"], "env-values");
    assert_eq!(
        exported["URL"],
        format!("http://localhost:{}", port["port"])
    );
    assert_eq!(exported["GOCACHE"], cache);
    fixture.ok(&["rm", "env-values"]);
    // The repository keeps its caches until cleanup empties them.
    assert!(Path::new(cache).join("entry").exists());
    let report = fixture.ok(&["cleanup", "--caches"]);
    assert_eq!(
        report["caches"],
        serde_json::json!([{"repository": "repo with ' quotes & $literal", "outcome": "emptied", "caches": ["go"]}])
    );
    assert!(!Path::new(cache).exists());
}

#[test]
fn add_starts_agents_only_after_creation_and_preserves_workspace_on_exit() {
    let fixture = Fixture::with_config(Some("max_agents = 'none'"));
    let bin = fixture.root.path().join("add-agent-bin");
    fs::create_dir(&bin).unwrap();
    let inspection = fixture.root.path().join("agent-inspection.json");
    let directive = fixture.root.path().join("directive");
    for agent in ["codex", "claude"] {
        let stub = bin.join(agent);
        install_test_script_with_mode(
            &stub,
            r#"#!/bin/sh
"$SHOAL_TEST_BIN" --state-dir "$SHOAL_TEST_STATE" --json inspect "$SHOAL_TEST_NAME" > "$SHOAL_TEST_INSPECTION" || exit 99
test -f tracked || exit 98
test -z "$SHOAL_SHELL_DIRECTIVE" || exit 97
printf '%s\n' "$PWD" "$SHOAL_WORKSPACE" "$@"
exit 7
"#,
            0o700,
        );
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
            .env("PATH", fixture.restricted_path(&bin))
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
            format!("{name}\nliteral spaces; $(false)\n")
        } else {
            format!(
                "{name}\nliteral spaces; $(false)\n--sandbox\ndanger-full-access\n--ask-for-approval=never\n"
            )
        };
        assert!(
            strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n').ends_with(
                &format!("{}\n{args}", fs::canonicalize(path).unwrap().display())
            )
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
        .env("PATH", fixture.restricted_path(&bin))
        .env("SHOAL_TEST_BIN", env!("CARGO_BIN_EXE_shoal"))
        .env("SHOAL_TEST_INSPECTION", &inspection)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!inspection.exists());

    // A missing executable is refused before any worktree is created.
    fs::remove_file(bin.join("codex")).unwrap();
    fs::write(config_dir.join("config.toml"), "").unwrap();
    let add = |name: &str| {
        fixture
            .command()
            .args([
                "add",
                fixture.repo.to_str().unwrap(),
                name,
                "--agent",
                "codex",
            ])
            .env("PATH", fixture.restricted_path(&bin))
            .output()
            .unwrap()
    };
    let output = add("missing-agent");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cannot start codex: codex is not installed or not on PATH"),
        "{output:?}"
    );
    assert!(!fixture.run(&["inspect", "missing-agent"]).status.success());

    // A launch that fails only once it starts leaves the worktree for retry.
    fs::write(
        config_dir.join("config.toml"),
        "[commands]\ncodex = ['{path}/missing-codex']\n",
    )
    .unwrap();
    let output = add("failed-agent");
    assert!(!output.status.success());
    let workspace = fixture.ok(&["inspect", "failed-agent"]);
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
    let stub = bin.join("codex");
    install_test_script_with_mode(
        &stub,
        "#!/bin/sh\nprintf '%s\\n' \"$SHOAL_WORKSPACE\" \"$@\"\nexit 7\n",
        0o700,
    );

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
                .env("PATH", fixture.restricted_path(&bin))
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(7),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let app = mode == Some("--app") || (mode.is_none() && !config.is_empty());
            let stdout = String::from_utf8(output.stdout).unwrap();
            if app {
                assert_eq!(stdout, format!("\napp\n{path}\nliteral spaces; $(false)\n"));
            } else {
                let stdout = strip_shoal_arguments(&stdout, '\n');
                let lines: Vec<_> = stdout.lines().collect();
                assert_eq!(
                    lines[1..],
                    [
                        "literal spaces; $(false)",
                        "--sandbox",
                        "danger-full-access",
                        "--ask-for-approval=never"
                    ]
                );
                assert_eq!(lines[0], "default-mode");
            }
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
        .env("PATH", fixture.restricted_path(&bin))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("\napp\n{path}\nrepo-default\n")
    );
    // An explicit desktop handoff does not load terminal prompt templates.
    fs::write(config_dir.join("worker-template.md"), [0xff]).unwrap();
    for mode in ["--app", "--cli"] {
        let output = fixture
            .command()
            .args(["codex", mode, "default-mode", "--", "desktop prompt"])
            .env("PATH", fixture.restricted_path(&bin))
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
                String::from_utf8_lossy(&output.stderr).contains("worker-template.md"),
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
    for agent in ["claude", "codex"] {
        let path = bin.join(agent);
        install_test_script_with_mode(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$SHOAL_WORKSPACE\" \"$@\"\n",
            0o700,
        );
        let mut command = fixture.command();
        command.arg(agent);
        if agent == "codex" {
            command.arg("--cli");
        }
        let output = command
            .args(["shortcut", "--", "--version", "hello with spaces"])
            .env("PATH", fixture.restricted_path(&bin))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
            format!(
                "shortcut\n--version\nhello with spaces\n{}",
                if agent == "claude" {
                    ""
                } else {
                    "--sandbox\ndanger-full-access\n--ask-for-approval=never\n"
                }
            )
        );
    }
    // Current-directory resolution supplies an ID internally; Claude's
    // environment still names the human workspace, as after an fzf selection.
    for target in [Some(workspace["id"].as_str().unwrap()), None] {
        let mut command = fixture.command();
        command
            .current_dir(workspace["path"].as_str().unwrap())
            .arg("claude");
        if let Some(target) = target {
            command.arg(target);
        }
        let output = command
            .env("PATH", fixture.restricted_path(&bin))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
            "shortcut\n"
        );
    }
}

#[test]
fn worker_templates_resolve_per_launch_and_reach_native_instruction_options() {
    let fixture = Fixture::new();
    let workspace = fixture.add("instructions");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("worker-template.md"),
        "global {workspace} {branch}",
    )
    .unwrap();
    let bin = fixture.root.path().join("template-bin");
    for agent in ["codex", "claude"] {
        let stub = bin.join(agent);
        install_test_script_with_mode(&stub, "#!/bin/sh\nprintf '%s\\0' \"$@\"\n", 0o700);
    }
    for (index, expected) in [
        "global instructions instructions".to_owned(),
        format!("repo {}", path.display()),
        "saved \"quotes\"\n$(false) {unknown}".to_owned(),
        String::new(),
        String::new(),
    ]
    .into_iter()
    .enumerate()
    {
        if index == 1 {
            fs::write(path.join("worker-template.md"), "repo {path}").unwrap();
        }
        if index == 4 {
            fs::write(config_dir.join("worker-template.md"), "").unwrap();
        }
        if index >= 2 {
            let local = fixture.root.path().join("local.toml");
            fs::write(
                &local,
                format!(
                    "worker_template = {}",
                    toml::Value::String(expected.clone())
                ),
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
        let expected = if index == 4 {
            String::new()
        } else if index == 0 || expected.is_empty() {
            "global instructions instructions".to_owned()
        } else {
            format!("global instructions instructions\n\n{expected}")
        };
        for agent in ["codex", "claude"] {
            let mut command = fixture.command();
            command.arg(agent);
            if agent == "codex" {
                command.arg("--cli");
            }
            let output = command
                .args(["instructions", "--", "user prompt"])
                .env("PATH", fixture.restricted_path(&bin))
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            let args = without_message_hooks(&stdout.split('\0').collect::<Vec<_>>());
            let instructions = if agent == "claude" {
                assert_eq!(args[0], "--append-system-prompt");
                args[1].to_owned()
            } else {
                assert_eq!(args[0], "-c");
                let setting: toml::Value = toml::from_str(args[1]).unwrap();
                setting["developer_instructions"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            };
            assert_eq!(after_guidance(&instructions), expected);
            assert_eq!(args[2], "user prompt");
        }
    }
}

#[test]
fn stop_and_manual_removal_terminate_connected_executions() {
    let fixture = Fixture::new();
    for operation in ["stop", "rm"] {
        let workspace = fixture.add("running");
        let child = fixture
            .command()
            .args([
                "exec",
                "running",
                "--",
                "sh",
                "-c",
                "while :; do sleep 1; done",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_until("execution registration", || {
            !fixture.ok(&["inspect", "running"])["executions"]
                .as_array()
                .unwrap()
                .is_empty()
        });
        let output = fixture
            .command()
            .current_dir(workspace["path"].as_str().unwrap())
            .args(["--json", operation])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        if operation == "stop" {
            fixture.ok(&["rm", "running"]);
        }
    }
}

#[test]
fn stop_all_stops_running_workspaces_and_marks_them_stopped() {
    let fixture = Fixture::new();
    let mut children = Vec::new();
    for name in ["first", "second"] {
        fixture.add(name);
        children.push(
            fixture
                .command()
                .args(["exec", name, "--", "sh", "-c", "while :; do sleep 1; done"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    fixture.add("unused");
    for name in ["first", "second"] {
        wait_until("execution registration", || {
            !fixture.ok(&["inspect", name])["executions"]
                .as_array()
                .unwrap()
                .is_empty()
        });
    }
    let stopped = fixture.ok(&["stop", "--all"]);
    let mut names = stopped["stopped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["first", "second"]);
    assert!(stopped["failed"].as_array().unwrap().is_empty());
    for mut child in children {
        assert!(!child.wait().unwrap().success());
    }
    let listed = fixture.run(&["work"]);
    let listed = String::from_utf8_lossy(&listed.stdout);
    for line in listed.lines() {
        let unused = line.contains("unused");
        assert_eq!(line.contains("stopped"), !unused, "{listed}");
        assert_eq!(line.contains("inactive"), unused, "{listed}");
    }
    // Without agents, every workspace's commands are reported once.
    let resumed = fixture.ok(&["resume", "--all"]);
    assert_eq!(
        resumed["commands"].as_array().unwrap().len(),
        2,
        "{resumed}"
    );
    assert!(resumed["agents"].as_array().unwrap().is_empty());
    let listed = fixture.run(&["work"]);
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("stopped"));
    assert!(
        fixture.ok(&["resume", "--all"])["commands"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn stop_saves_agents_and_commands_and_resumes_agent_sessions() {
    let fixture = Fixture::with_config(Some(
        "[commands]\nclaude = ['sh', './agent.sh']\n[agent_resume]\nclaude = ['sh', './resume.sh', '{prompt}']\n",
    ));
    let workspace = fixture.add("stopped");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join("agent.sh"),
        "touch started-$SHOAL_EXECUTION_ID\ntrap 'exit 0' TERM\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::write(path.join("resume.sh"), "printf '%s|' \"$1\" >> restored\n").unwrap();
    let spawn = |args: &[&str]| {
        fixture
            .command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let mut children = vec![
        spawn(&["claude", "stopped"]),
        spawn(&["claude", "stopped"]),
        spawn(&[
            "exec",
            "stopped",
            "--",
            "sh",
            "-c",
            "touch command-started; while :; do sleep 1; done",
        ]),
    ];
    let mut last_inspection = Value::Null;
    let ids = try_wait_for(|| {
        let inspection = fixture.ok(&["inspect", "stopped"]);
        last_inspection = inspection.clone();
        let ids = inspection["executions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|execution| execution["id"].as_str().unwrap().to_owned())
            .filter(|id| path.join(format!("started-{id}")).exists())
            .collect::<Vec<_>>();
        (ids.len() == 2 && path.join("command-started").exists()).then_some(ids)
    })
    .unwrap_or_else(|| panic!("executions did not start: {last_inspection}"));
    let output = fixture
        .command()
        .current_dir(path)
        .args(["--json", "stop"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        fixture.ok(&["inspect", "stopped"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for child in children.drain(..) {
        let output = child.wait_with_output().unwrap();
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("shoal resume"),
            "{output:?}"
        );
    }
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspace["id"].as_str().unwrap());
    for id in &ids {
        assert!(state.join(format!("{id}.recovery.json")).exists());
    }
    let commands = fs::read_dir(&state)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".command.json")
        })
        .count();
    assert_eq!(commands, 1);
    // Several agents without a terminal are listed for separate resumes.
    let all = fixture.ok(&["resume", "--all"]);
    let agents = all["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 2, "{all}");
    assert!(agents.iter().all(|agent| agent["background"] == false));
    assert!(all["commands"].as_array().unwrap().is_empty());
    let output = fixture.run(&["resume", "stopped"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--execution"));
    for id in &ids {
        assert!(
            fixture
                .run(&["resume", "stopped", "--execution", id])
                .status
                .success()
        );
        assert!(!state.join(format!("{id}.recovery.json")).exists());
    }
    // The stopped command reaches the first restored session only.
    let restored = fs::read_to_string(path.join("restored")).unwrap();
    let prompts = restored.split('|').collect::<Vec<_>>();
    assert_eq!(prompts.len(), 3, "{restored}");
    assert!(
        prompts[0].contains("- sh -c 'touch command-started; while :; do sleep 1; done'"),
        "{restored}"
    );
    for prompt in &prompts[..2] {
        assert!(
            prompt.contains("Continue working on the unfinished assignment"),
            "{restored}"
        );
    }
    assert!(!prompts[1].contains("command-started"), "{restored}");
    // The restored agents exited on their own, leaving only the last session.
    let left = fs::read_dir(&state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(left, ["last-session.json"]);
    let output = fixture.run(&["exec", "stopped", "--", env!("CARGO_BIN_EXE_shoal"), "stop"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("workspace processes can only"));
}

#[test]
fn resume_continues_the_session_of_an_agent_that_exited_on_its_own_after_a_restore() {
    let fixture = Fixture::with_config(Some("[commands]\nclaude = ['sh', './agent.sh']\n"));
    let workspace = fixture.add("quit");
    let path = Path::new(workspace["path"].as_str().unwrap());
    // The agent reports its session as Claude Code's hooks do, then quits.
    fs::write(
        path.join("agent.sh"),
        format!(
            "if [ \"$1\" = --resume ]; then printf '%s' \"$2\" > resumed; exit 0; fi\n\
             printf '{{\"session_id\":\"quit-session\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n",
            env!("CARGO_BIN_EXE_shoal")
        ),
    )
    .unwrap();
    let output = fixture
        .command()
        .args(["claude", "quit"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // The removal record keeps the last session for the restored workspace.
    fixture.ok(&["rm", "quit", "--yes"]);
    let removed = fixture.ok(&["restore"]);
    assert_eq!(
        removed[0]["last_session"],
        serde_json::json!({"agent": "claude", "session": "quit-session"}),
        "{removed}"
    );
    let restored = fixture.ok(&["restore", "quit"]);
    assert_eq!(restored["warnings"], serde_json::json!([]));
    let output = fixture
        .command()
        .current_dir(path)
        .arg("resume")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(path.join("resumed")).unwrap(),
        "quit-session"
    );
}

#[test]
fn import_on_the_exporting_machine_continues_the_session_unless_told_not_to() {
    let fixture = Fixture::with_config(Some("[commands]\nclaude = ['sh', './agent.sh']\n"));
    let workspace = fixture.add("carried");
    let path = Path::new(workspace["path"].as_str().unwrap());
    // The agent reports its session as Claude Code's hooks do, also when it
    // resumes one, then quits.
    fs::write(
        path.join("agent.sh"),
        format!(
            "session=carried-session\n\
             if [ \"$1\" = --resume ]; then printf '%s' \"$2\" > resumed; session=$2; fi\n\
             printf '{{\"session_id\":\"%s\"}}' \"$session\" | '{}' internal agent-state idle >/dev/null || exit 3\n",
            env!("CARGO_BIN_EXE_shoal")
        ),
    )
    .unwrap();
    let output = fixture
        .command()
        .args(["claude", "carried"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let file = fixture.root.path().join("carried.shoal");
    let file = file.to_str().unwrap();
    let repo = fixture.repo.to_str().unwrap();
    let resume = || {
        let output = fixture
            .command()
            .current_dir(path)
            .arg("resume")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    };

    let exported = fixture.ok(&["export", "carried", "-o", file]);
    // The import trusts the workspace's path for the agent it resumes there.
    let config = fixture.root.path().join(".claude.json");
    fs::remove_file(&config).unwrap();
    let imported = fixture.ok(&["import", file, "--repo", repo]);
    assert_eq!(imported["warnings"], serde_json::json!([]));
    // A machine without an ID cannot tell it exported the workspace.
    if exported["manifest"]["sessions"]["machine"].is_null() {
        resume();
        assert!(!path.join("resumed").exists());
        return;
    }
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(
        root["projects"][checkout.to_str().unwrap()]["hasTrustDialogAccepted"],
        true
    );
    resume();
    assert_eq!(
        fs::read_to_string(path.join("resumed")).unwrap(),
        "carried-session"
    );

    // --new-sessions starts the agent over on this machine too.
    fs::remove_file(path.join("resumed")).unwrap();
    fs::remove_file(file).unwrap();
    let exported = fixture.ok(&["export", "carried", "-o", file]);
    assert_eq!(
        exported["manifest"]["sessions"]["last_session"]["session"], "carried-session",
        "{exported}"
    );
    fixture.ok(&["import", file, "--repo", repo, "--new-sessions"]);
    resume();
    assert!(!path.join("resumed").exists());
}

#[test]
fn resume_reconciles_a_quarantined_execution_from_a_saved_handoff() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("quarantined");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let execution = crashed_execution(&mut fixture, "quarantined");
    let execution = execution["id"].as_str().unwrap();
    // The pause path clears the child identity before saving the handoff.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "UPDATE executions SET child=NULL,group_id=NULL WHERE id=?1",
        [execution],
    )
    .unwrap();
    fs::write(
        path.join(".shoal.toml"),
        "[agent_resume]\nclaude = ['sh', './resume.sh', '{prompt}']\n",
    )
    .unwrap();
    fs::write(path.join("resume.sh"), "printf restored > restored\n").unwrap();
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspace["id"].as_str().unwrap());
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join(format!("{execution}.recovery.json")),
        r#"{"agent":"claude"}"#,
    )
    .unwrap();

    let output = fixture.run(&["resume", "quarantined", "--execution", execution]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(path.join("restored")).unwrap(),
        "restored"
    );
    assert!(!state.join(format!("{execution}.recovery.json")).exists());
    assert!(
        fixture.ok(&["inspect", "quarantined"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn daemon_restarts_keep_tracked_agents_and_commands_running() {
    let mut fixture = Fixture::with_config(Some("[commands]\nclaude = ['sh', './agent.sh']\n"));
    let workspace = fixture.add("restarted");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(
        path.join("agent.sh"),
        "touch agent-started\ntrap 'exit 0' TERM\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    let spawn = |args: &[&str]| {
        fixture
            .command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut agent = spawn(&["claude", "restarted"]);
    let mut command = spawn(&[
        "exec",
        "restarted",
        "--",
        "sh",
        "-c",
        "touch command-started; while [ ! -f release ]; do sleep 0.1; done; touch command-exited; exit 3",
    ]);
    wait_until("executions to start", || {
        path.join("agent-started").exists() && path.join("command-started").exists()
    });
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspace["id"].as_str().unwrap());
    let saved = || {
        let mut saved = fs::read_dir(&state)
            .map(|entries| {
                entries
                    .map(|entry| {
                        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                        name[name.find('.').unwrap()..].to_owned()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        saved.sort();
        saved
    };
    let running = |fixture: &Fixture| {
        fixture.ok(&["inspect", "restarted"])["executions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|execution| execution["state"] == "running")
            .count()
    };
    // A planned stop detaches the wrappers, which keep their commands running.
    let output = fixture.run(&["daemon", "stop"]);
    assert!(output.status.success(), "{output:?}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("shoal resume"));
    fixture.daemon.child.wait().unwrap();
    assert!(agent.try_wait().unwrap().is_none());
    assert!(command.try_wait().unwrap().is_none());
    assert!(saved().is_empty(), "{:?}", saved());
    // A command that exits while the daemon is away reports to the next one.
    fs::write(path.join("release"), "").unwrap();
    wait_until("command to exit", || path.join("command-exited").exists());
    fixture.restart();
    assert_eq!(command.wait().unwrap().code(), Some(3));
    wait_until("agent to reattach", || running(&fixture) == 1);
    // A crash leaves the agent running too.
    fixture.restart();
    assert!(agent.try_wait().unwrap().is_none());
    wait_until("agent to reattach after a crash", || running(&fixture) == 1);
    // Stopping works again and the daemon still knows the agent.
    fixture.ok(&["stop", "restarted"]);
    assert!(agent.wait().unwrap().success());
    assert_eq!(saved(), [".recovery.json"]);
    assert!(
        fixture.ok(&["inspect", "restarted"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let notifications = fixture.ok(&["notifications"]);
    let messages = notifications
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["message"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        messages.contains(&"claude exited with code 0"),
        "{messages:?}"
    );
    assert!(fixture.ok(&["notifications", "clear"])["cleared"].as_u64() >= Some(1));
    assert_eq!(
        fixture.ok(&["notifications", "--all"]),
        Value::Array(vec![])
    );
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
fn workspace_targets_precede_directory_and_picker_fallback() {
    let fixture = Fixture::new();
    let own = fixture.add("selection-own");
    let other = fixture.add("selection-other");
    let nested = Path::new(own["path"].as_str().unwrap()).join("nested");
    fs::create_dir(&nested).unwrap();
    let bin = fixture.root.path().join("bin");
    let picked = fixture.root.path().join("picked");
    let fzf = bin.join("fzf");
    install_test_script(
        &fzf,
        "#!/bin/sh\ntouch \"$HOME/picked\"\nawk -F '\\t' -v id=\"$PICK_ID\" '$1 == id {print}'\n",
    );

    for action in ["inspect", "stop"] {
        // Stop leaves the idle workspaces out of its picker, offering all.
        let pick = match action {
            "stop" => "--all",
            _ => other["id"].as_str().unwrap(),
        };
        for (cwd, explicit, json, expected, picker) in [
            (nested.as_path(), None, false, &own, false),
            (nested.as_path(), None, true, &own, false),
            (
                nested.as_path(),
                Some("selection-other"),
                false,
                &other,
                false,
            ),
            (nested.as_path(), other["id"].as_str(), true, &other, false),
            (fixture.root.path(), None, false, &other, true),
        ] {
            let (_master, slave) = pty::open();
            let mut command = fixture.command();
            command
                .current_dir(cwd)
                .env("PICK_ID", pick)
                .stdin(slave.try_clone().unwrap())
                .stderr(slave);
            if json {
                command.arg("--json");
            }
            command.arg(action).args(explicit);
            let output = command.output().unwrap();
            assert!(output.status.success(), "{action}: {output:?}");
            assert_eq!(picked.exists(), picker);
            if picker {
                fs::remove_file(&picked).unwrap();
            }
            if action == "inspect" {
                let result: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(result["workspace"]["id"], expected["id"]);
            }
        }
        for (cwd, args) in [
            (nested.as_path(), vec![action, "missing"]),
            (fixture.root.path(), vec![action, "missing"]),
            (fixture.root.path(), vec!["--json", action]),
        ] {
            let (_master, slave) = pty::open();
            let output = fixture
                .command()
                .current_dir(cwd)
                .env("PICK_ID", other["id"].as_str().unwrap())
                .args(args)
                .stdin(slave.try_clone().unwrap())
                .stderr(slave)
                .output()
                .unwrap();
            assert!(!output.status.success(), "{output:?}");
            assert!(!picked.exists());
        }
    }
}

#[test]
fn workspace_commands_select_from_a_deleted_current_directory() {
    use std::{ffi::CString, os::unix::process::CommandExt};

    let fixture = Fixture::new();
    let workspace = fixture.add("available");
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let picked = fixture.root.path().join("picked");
    let fzf = bin.join("fzf");
    // Bash reports getcwd failures at startup if the picker inherits a deleted cwd.
    install_test_script(&fzf, "#!/bin/bash\npwd -P > \"$HOME/picked\"\ncat\n");

    for (args, interactive, expect_report) in [
        (vec!["doctor"], true, true),
        (vec!["--json", "doctor"], false, false),
        (vec!["--json", "doctor", "available"], false, true),
        (vec!["--json", "doctor", "--all"], false, true),
        (vec!["rm", "--yes"], true, true),
    ] {
        let deleted = fixture.root.path().join("deleted");
        fs::create_dir(&deleted).unwrap();
        let path = CString::new(deleted.as_os_str().as_encoded_bytes()).unwrap();
        let mut command = fixture.command();
        command.current_dir(&deleted).args(&args);
        // Delete only the child's cwd, after chdir and before exec; never change
        // the test process's directory or the fixture's managed worktree.
        unsafe {
            command.pre_exec(move || {
                if libc::rmdir(path.as_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let (mut master, slave) = pty::open();
        if interactive {
            command
                .stdin(slave.try_clone().unwrap())
                .stderr(slave.try_clone().unwrap());
        }
        let output = command.output().unwrap();
        drop(command);
        drop(slave);
        assert!(matches!(output.status.code(), Some(0 | 2)), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        if interactive {
            use std::io::Read;
            let mut transcript = String::new();
            let _ = master.read_to_string(&mut transcript);
            assert!(transcript.is_empty(), "{transcript}");
            if args[0] == "doctor" {
                assert!(stdout.contains("available: ready"), "{stdout}");
            } else {
                assert!(stdout.contains("Workspace removed"), "{stdout}");
                assert!(fixture.ok(&["work"]).as_array().unwrap().is_empty());
            }
            assert!(!stdout.contains("Could not check workspaces"), "{stdout}");
            assert_eq!(fs::read_to_string(&picked).unwrap().trim(), "/");
            fs::remove_file(&picked).unwrap();
        } else {
            let report: Value = serde_json::from_str(&stdout).unwrap();
            assert!(!picked.exists());
            if expect_report {
                assert_eq!(report["workspaces"][0]["workspace"]["id"], workspace["id"]);
            } else {
                assert_eq!(report["workspaces"], serde_json::json!([]));
                let check = report["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|check| check["name"] == "workspaces")
                    .unwrap();
                assert!(
                    check["message"]
                        .as_str()
                        .unwrap()
                        .contains("explicit target")
                );
            }
        }
    }
}

#[test]
fn noninteractive_missing_targets_do_not_open_pickers() {
    let fixture = Fixture::new();
    fixture.add("available");
    for args in [
        &["add"][..],
        &["exec", "--", "true"],
        &["rm"],
        &["inspect"],
        &["stop"],
        &["status"],
        &["diff"],
        &["done"],
    ] {
        for json in [false, true] {
            let mut command = fixture.command();
            if json {
                command.arg("--json");
            }
            let output = command.args(args).output().unwrap();
            assert!(!output.status.success(), "{args:?}: {output:?}");
            assert!(String::from_utf8_lossy(&output.stderr).contains("explicit target"));
        }
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
shoal add "$REPO" other
test "${PWD##*/}" = other
shoal cd navigate
shoal cd -
test "${PWD##*/}" = other
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
shoal cd -
test "${PWD##*/}" = other
shoal rm
test "$PWD" = "$shoal_repo_dir"
if shoal cd -; then
  exit 1
fi
test "$PWD" = "$shoal_repo_dir"
shoal add "$REPO" acknowledged
test "${PWD##*/}" = acknowledged
printf 'retained' > untracked
shoal done
test "${PWD##*/}" = acknowledged
shoal undone
rm untracked
shoal done
until ! shoal inspect acknowledged >/dev/null 2>&1; do sleep 0.1; done
_shoal_recover_directory
test "$PWD" = "$shoal_repo_dir"
shoal add "$REPO" held
shoal hold acquire held --name review
shoal done --cleanup
test "${PWD##*/}" = held
shoal hold release held --name review
until ! shoal inspect held >/dev/null 2>&1; do sleep 0.1; done
_shoal_recover_directory
test "$PWD" = "$shoal_repo_dir"
shoal add "$REPO" completed
printf 'keep me' > untracked
shoal done --cleanup
test "${PWD##*/}" = completed
shoal undone
rm untracked
shoal done --cleanup
until ! shoal inspect completed >/dev/null 2>&1; do sleep 0.1; done
_shoal_recover_directory
test "$PWD" = "$shoal_repo_dir"
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
    let workspaces = fixture.ok(&["work"]);
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
    install_test_script(&setup, "#!/bin/sh\nprintf setup >> setup-runs\n");
    let post_setup = fixture.repo.join("post-setup.sh");
    install_test_script(
        &post_setup,
        "#!/bin/sh\ntest -z \"$SHOAL_SCOPE_TOKEN\" || exit 81\ntest -z \"$SHOAL_EXECUTION_ID\" || exit 82\ntest -z \"$SHOAL_RESERVED_PORT_ENV\" || exit 83\ntest -z \"$SHOAL_PORT_WEB\" || exit 84\nprintf hook >> hook-runs\nsleep 30 < /dev/null > /dev/null 2>&1 &\n",
    );
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
    let output = scoped(&["--json", "work"]);
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
        vec!["rm", "worker", "--yes"],
        vec!["stop", "worker"],
        vec!["inspect", "other"],
        vec!["exec", "other", "--", "true"],
        vec!["setup", "other"],
        vec!["link", "pr", "42", "assigned", "--workspace", "other"],
        vec!["unlink", "pr", "assigned", "--workspace", "other"],
        vec!["undone", "other"],
        vec!["done", "other", "--cleanup"],
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
        .args(["work"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expired or unknown"));
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 2);
}

#[test]
fn setup_commands_cannot_recursively_run_setup() {
    let fixture = Fixture::new();
    let setup = fixture.repo.join("setup.sh");
    install_test_script(&setup, "#!/bin/sh\nexit 0\n");
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
fn simulator_omitted_runtime_uses_latest_ios_and_preserves_existing_lease() {
    let config = SIM_CONFIG.replace("runtime = \"iOS Test\"", "");
    let fixture = Fixture::with_tools(Some(&config), true);
    let set_runtimes = |newer_available| {
        fs::write(
            fixture.root.path().join("sim-runtimes.json"),
            serde_json::json!([
                {"name": "iOS 26.9", "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-9", "version": "26.9", "isAvailable": true},
                {"name": "iOS 26.10", "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-10", "version": "26.10", "isAvailable": true},
                {"name": "iOS 27", "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-27-0", "version": "27.0", "isAvailable": newer_available},
                {"name": "tvOS 28", "identifier": "com.apple.CoreSimulator.SimRuntime.tvOS-28-0", "version": "28.0", "isAvailable": true}
            ]).to_string(),
        ).unwrap();
    };
    set_runtimes(false);
    fixture.add("worker");
    let overview = fixture.run(&["sim", "worker"]);
    assert!(overview.status.success());
    assert!(String::from_utf8_lossy(&overview.stdout).contains("latest iOS"));
    let first = fixture.ok(&["sim", "acquire", "worker"]);
    assert_eq!(
        first["runtime"],
        "com.apple.CoreSimulator.SimRuntime.iOS-26-10"
    );
    set_runtimes(true);
    assert_eq!(fixture.ok(&["sim", "acquire", "worker"]), first);
    assert_eq!(
        fixture.ok(&["sim", "acquire", "worker", "--device", "Phone"]),
        first
    );
    assert_eq!(
        fixture.ok(&["sim", "acquire", "worker", "--profile", "phone"]),
        first
    );
    fixture.ok(&["sim", "release", "default", "worker"]);
    let next = fixture.ok(&["sim", "acquire", "worker", "--device", "Phone"]);
    assert_eq!(
        next["runtime"],
        "com.apple.CoreSimulator.SimRuntime.iOS-27-0"
    );
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    let creates: Vec<Value> = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event[0] == "create")
        .collect();
    assert_eq!(creates.len(), 2);
    assert_eq!(creates[0][3], first["runtime"]);
    assert_eq!(creates[1][3], next["runtime"]);
}

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
    fixture.ok(&["notifications"]);
    let mut waiting = fixture
        .command()
        .args(["--json", "sim", "acquire", "second", "--queue"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_busy(&fixture, "second");
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
    fixture.ok(&["rm", "worker", "--yes"]);
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
    let sims = wait_for("idle simulator deletion", || {
        let sims = fixture.ok(&["sim", "--all"]);
        (sims["simulators"].as_array().unwrap().len() == 1).then_some(sims)
    });
    assert_eq!(sims["simulators"][0]["udid"], second["udid"]);
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
            "--queue",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("busy simulator history entry", || {
        let entries = fixture.ok(&["sim", "history", "--all"]);
        entries.as_array().unwrap().len() == 1 && entries[0]["status"] == "busy"
    });
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
    let config = SIM_CONFIG.replace("max_devices = 2", "max_devices = 1");
    let fixture = Fixture::with_tools(Some(&config), true);
    let workspace = fixture.add("worker");
    let response = fixture.request_with_id(
        2,
        serde_json::json!({"sim_acquire":{"workspace":workspace["id"],"request":{"request_id":uuid::Uuid::new_v4().to_string(),"clean":true,"name":"default","profile":null,"device":null,"runtime":null,"reason":null}}}),
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
fn acquire_selects_the_lease_kind() {
    let fixture = Fixture::with_config(Some(RESOURCE_CONFIG));
    fixture.add("first");
    let port = fixture.ok(&["acquire", "port", "web", "--workspace", "first"]);
    assert_eq!(port["name"], "web");
    let lease = fixture.ok(&[
        "acquire",
        "resource",
        "devices",
        "--member",
        "beta",
        "--name",
        "tests",
        "--workspace",
        "first",
    ]);
    assert_eq!(lease["resource"], "beta");
    assert_eq!(lease["name"], "tests");
    // A repo acquisition never selects another kind of member.
    let repo = fixture.run(&["acquire", "repo", "signing", "--workspace", "first"]);
    assert!(!repo.status.success());
    assert!(String::from_utf8_lossy(&repo.stderr).contains("incompatible"));
    let leases = fixture.ok(&["leases", "--workspace", "first"]);
    assert_eq!(leases["ports"]["reserved"][0]["name"], "web");
    assert_eq!(leases["resources"]["leases"].as_array().unwrap().len(), 1);
    assert_eq!(leases["resources"]["leases"][0]["name"], "tests");
    let count = |args: &[&str], key: &str| {
        fixture.ok(&[&["leases"], args, &["--workspace", "first"]].concat())[key]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(count(&["resource"], "pools"), 2);
    assert_eq!(count(&["repo"], "pools"), 0);
    assert_eq!(count(&["repo"], "leases"), 0);
    assert_eq!(
        fixture.ok(&["leases", "--all"]).as_array().unwrap().len(),
        1
    );
    assert_eq!(
        fixture.ok(&["leases", "--all"])[0]["workspace"]["name"],
        "first"
    );
    assert_eq!(leases["workspace"]["name"], "first");
    let text = fixture.run(&["leases", "--workspace", "first"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Ports") && text.contains("web=") && text.contains("devices/tests"));
    assert!(!text.contains("Simulators"));
}

#[test]
fn release_narrows_from_every_lease_to_one_kind_or_item() {
    let fixture = Fixture::with_config(Some(RESOURCE_CONFIG));
    fixture.add("first");
    for args in [
        vec!["acquire", "port", "web"],
        vec!["acquire", "resource", "devices", "--name", "a"],
        vec!["acquire", "resource", "devices", "--name", "b"],
        vec!["acquire", "resource", "signing"],
    ] {
        fixture.ok(&[args, vec!["--workspace", "first"]].concat());
    }
    let released = |args: &[&str]| {
        fixture.ok(&[&["release", "--workspace", "first"], args].concat())["released"].clone()
    };
    // A named lease must match the requested kind.
    let mismatch = fixture.run(&[
        "release",
        "repo",
        "devices",
        "--name",
        "a",
        "--workspace",
        "first",
    ]);
    assert!(!mismatch.status.success());
    assert!(
        String::from_utf8_lossy(&mismatch.stderr).contains("no repo lease or request devices/a")
    );
    assert_eq!(
        released(&["resource", "devices", "--name", "a"]),
        serde_json::json!([{"kind": "resource", "pool": "devices", "name": "a"}])
    );
    assert_eq!(
        released(&["resource", "devices"]),
        serde_json::json!([{"kind": "resource", "pool": "devices", "name": "b"}])
    );
    assert_eq!(
        released(&["port"]),
        serde_json::json!([{"kind": "port", "name": "web"}])
    );
    assert_eq!(
        released(&[]),
        serde_json::json!([{"kind": "resource", "pool": "signing", "name": "default"}])
    );
    assert_eq!(released(&[]), serde_json::json!([]));
    let inspection = fixture.ok(&["inspect", "first"]);
    assert!(inspection["ports"].as_array().unwrap().is_empty());
    assert!(inspection["resources"].as_array().unwrap().is_empty());
}

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
    fixture.ok(&["notifications"]);
    let mut waiter = fixture
        .command()
        .args([
            "--json", "resource", "acquire", "workers", "worker", "--name", "waiter", "--queue",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_busy(&fixture, "worker");
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
    fixture.ok(&["rm", "worker", "--yes"]);
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
        ("cleanup.auto.enabled", "maybe"),
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
    // One invalid change rejects the whole set.
    assert!(
        !fixture
            .run(&[
                "config",
                "set",
                "cleanup.auto.idle_minutes",
                "30",
                "cleanup.auto.enabled",
                "maybe",
                "--repo",
                id,
            ])
            .status
            .success()
    );
    assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    fixture.ok(&[
        "config",
        "set",
        "cleanup.auto.idle_minutes",
        "30",
        "default_agent",
        "claude",
        "--repo",
        id,
    ]);
    fixture.ok(&["config", "unset", "cleanup", "default_agent", "--repo", id]);
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
    fixture.ok(&["rm", "second", "--yes"]);
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
    fs::copy(
        second_path.join(".shoal.toml"),
        first_path.join(".shoal.toml"),
    )
    .unwrap();
    let renewed = fixture.ok(&[
        "resource",
        "acquire",
        "signing",
        "first",
        "--reason",
        "still signing",
    ]);
    assert_eq!(renewed["id"], lease["id"]);
    assert_eq!(renewed["created_at"], lease["created_at"]);
    assert_eq!(renewed["reason"], "still signing");
    assert_eq!(fixture.ok(&["inspect", "first"])["resources"][0], renewed);
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
    // A remote branch base is fetched first, so an unreachable remote stops
    // creation instead of starting from a stale tracking ref.
    let output = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "from-remote",
        "--base",
        "origin/topic",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not fetch origin/topic"), "{stderr}");
    assert_eq!(
        fixture.ok(&["inspect", "from-remote"])["workspace"]["state"],
        "failed"
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), initial);
    assert_eq!(git(&fixture.repo, &["rev-parse", "feature/source"]), source);
}

#[test]
fn add_fetches_a_remote_branch_base_before_creating_the_worktree() {
    let fixture = Fixture::new();
    let author = upstream_remote(&fixture);
    let main = git(&fixture.repo, &["rev-parse", "main"]);
    git(&author, &["switch", "-c", "feature/api"]);
    for (index, contents) in ["first\n", "second\n"].into_iter().enumerate() {
        commit_file(&author, "api", contents);
        git(&author, &["push", "origin", "feature/api"]);
        let expected = git(&author, &["rev-parse", "HEAD"]);
        let name = format!("on-api-{index}");
        let workspace = fixture.ok(&[
            "add",
            fixture.repo.to_str().unwrap(),
            &name,
            "--base",
            "origin/feature/api",
        ]);
        let path = Path::new(workspace["path"].as_str().unwrap());
        assert_eq!(git(path, &["rev-parse", "HEAD"]), expected);
        assert_eq!(workspace["base_ref"], "refs/remotes/origin/feature/api");
        assert_eq!(workspace["base_commit"], expected.trim());
    }
    // Only the named branch is fetched; the local default stays as it was.
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), main);
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
    fixture.ok(&["notifications"]);
    let mut waiter = fixture
        .command()
        .args([
            "--json", "resource", "acquire", "cache", "writer", "--mode", "write", "--queue",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_busy(&fixture, "writer");
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
fn resource_renewals_reject_scope_drift_before_allocating() {
    let config = "[resources.cache]\nkind='rwlock'\n";
    let mut fixture = Fixture::with_config(Some(config));
    let workspace = fixture.add("owner");
    let args = ["resource", "acquire", "cache", "owner", "--mode", "read"];
    let original = fixture.ok(&args);
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join(".shoal.toml"),
        config,
    )
    .unwrap();
    fs::write(fixture.root.path().join(".config/shoal/config.toml"), "").unwrap();
    fixture.restart();
    let rejected = fixture.run(&args);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("different settings"));
    assert_eq!(
        fixture.ok(&["resource", "owner"])["leases"],
        serde_json::json!([original])
    );
    fixture.ok(&["resource", "release", "cache", "owner"]);
    let local = fixture.ok(&args);
    assert_ne!(local["scope"], original["scope"]);
    assert_eq!(fixture.ok(&args), local);
    fs::write(
        fixture.root.path().join(".config/shoal/config.toml"),
        config,
    )
    .unwrap();
    fixture.restart();
    assert!(!fixture.run(&args).status.success());
    assert_eq!(
        fixture.ok(&["resource", "owner"])["leases"],
        serde_json::json!([local])
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
fn cd_previous_returns_to_the_workspace_left_and_json_never_navigates() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    let second = fixture.add("second");
    let path = |workspace: &Value| {
        fs::canonicalize(workspace["path"].as_str().unwrap())
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    };
    let directive = fixture.root.path().join("cd-directive");
    fs::write(&directive, "").unwrap();
    let cd = |from: &Path, args: &[&str]| {
        fixture
            .command()
            .current_dir(from)
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .args(args)
            .output()
            .unwrap()
    };
    let none = cd(&fixture.repo, &["cd", "-"]);
    assert!(!none.status.success());
    assert!(String::from_utf8_lossy(&none.stderr).contains("no previous workspace"));
    let first_path = path(&first);
    let second_path = path(&second);
    assert!(
        cd(Path::new(&first_path), &["--json", "cd", "second"])
            .status
            .success()
    );
    for (from, to) in [(&second_path, &first_path), (&first_path, &second_path)] {
        let output = cd(Path::new(from), &["--json", "cd", "-"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["path"],
            to.as_str()
        );
    }
    assert_eq!(fs::read_to_string(&directive).unwrap(), "");
    fixture.ok(&["rm", "first"]);
    let removed = cd(Path::new(&second_path), &["cd", "-"]);
    assert!(!removed.status.success());
    assert!(String::from_utf8_lossy(&removed.stderr).contains("no previous workspace"));
    assert_eq!(fs::read_to_string(&directive).unwrap(), "");
}

#[test]
fn previous_workspace_is_remembered_per_command_and_not_for_scoped_callers() {
    let fixture = Fixture::new();
    fixture.add("first");
    fixture.add("second");
    fixture.ok(&["status", "first"]);
    assert!(fixture.run(&["diff", "second"]).status.success());
    assert_eq!(fixture.ok(&["status", "-"])["workspace"]["name"], "first");
    let unsupported = fixture.run(&["inspect", "-"]);
    assert!(!unsupported.status.success());
    assert!(String::from_utf8_lossy(&unsupported.stderr).contains("does not accept -"));
    let binary = env!("CARGO_BIN_EXE_shoal");
    let scoped = fixture
        .command()
        .args(["exec", "first", "--", binary, "--json", "status", "-"])
        .output()
        .unwrap();
    assert!(!scoped.status.success());
    assert!(String::from_utf8_lossy(&scoped.stderr).contains("no previous workspace"));
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
            install_test_script(
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
            );
            let args = if scoped {
                vec!["exec", "menu-worker", "--", env!("CARGO_BIN_EXE_shoal")]
            } else if key == "ctrl-o" {
                // `shoal work` opens the same picker when its output is a terminal.
                vec!["work"]
            } else {
                vec![]
            };
            fs::remove_file(fixture.root.path().join("picker-input")).ok();
            let listing = args == ["work"];
            let (output, transcript) =
                fixture.terminal_command(fixture.command().args(&args), "", listing);
            assert!(
                fixture.root.path().join("picker-input").exists(),
                "picker did not run: {transcript}"
            );
            let args = fs::read_to_string(fixture.root.path().join("picker-args")).unwrap();
            let input = fs::read_to_string(fixture.root.path().join("picker-input")).unwrap();
            assert!(input.contains(workspace["id"].as_str().unwrap()));
            assert_eq!(input.contains("Add workspace"), !scoped);
            // Every key is bound, and those the view leaves out unbound at start.
            let (unbound, header) = if scoped {
                (
                    Some("ctrl-t,ctrl-a,ctrl-s,ctrl-d"),
                    "enter: cd   ctrl-f: diff   ctrl-e: execute   ctrl-o: inspect",
                )
            } else {
                (
                    None,
                    "enter: cd   tab: next view   ctrl-t: attach   ctrl-f: diff   ctrl-a: add   ctrl-e: execute   ctrl-o: inspect   ctrl-s: stop   ctrl-d: delete",
                )
            };
            assert!(args.contains(&format!("--header\n{header}\n")), "{args}");
            let binds: String = [
                "enter", "ctrl-t", "ctrl-f", "ctrl-a", "ctrl-e", "ctrl-o", "ctrl-s", "ctrl-d",
            ]
            .iter()
            .map(|key| format!("--bind\n{key}:print({key})+accept\n"))
            .collect();
            assert!(args.contains(&binds), "{args}");
            assert_eq!(args.contains("start:unbind("), unbound.is_some(), "{args}");
            if let Some(keys) = unbound {
                assert!(args.contains(&format!("start:unbind({keys})")), "{args}");
            }
            // Workspace processes have no inbox to switch to.
            assert_eq!(args.contains("tab:transform("), !scoped, "{args}");
            match key {
                "cancel" => {
                    assert_eq!(output.status.code(), Some(130), "{transcript}");
                    assert!(!transcript.contains("canceled"), "{transcript}");
                }
                "ctrl-z" => {
                    assert!(!output.status.success());
                    assert!(transcript.contains("unknown picker action"), "{transcript}");
                }
                _ => {
                    assert!(output.status.success(), "{transcript}");
                    let shown = String::from_utf8_lossy(&output.stdout) + transcript.as_str();
                    assert!(shown.contains("menu-worker"), "{shown}");
                }
            }
        }
    }
    // `--print` lists on a terminal instead of opening the picker, as a
    // redirected listing does.
    fs::remove_file(fixture.root.path().join("picker-input")).ok();
    fixture.interactive(&["work"], "");
    assert!(!fixture.root.path().join("picker-input").exists());
    let (output, transcript) = fixture.interactive(&["work", "--print"], "");
    assert!(output.status.success(), "{transcript}");
    assert!(!fixture.root.path().join("picker-input").exists());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("menu-worker"),
        "{output:?}"
    );
}

#[test]
fn bare_shoal_opens_on_the_inbox_and_switches_views() {
    let fixture = Fixture::new();
    let asker = fixture.add("asker");
    fixture.add("quiet");
    // Records its arguments and rows, then chooses the first row whose ID
    // starts with $PICK, or cancels when it is empty. `=ID` chooses a row
    // that was not in its input, as one a Tab reload brought is.
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        r#"#!/bin/sh
printf '%s\n' "$@" > "$HOME/picker-args"
cat > "$HOME/picker-input"
[ -n "$PICK" ] || exit 130
printf '\n'
case "$PICK" in
=*) printf '%s\tloaded\n' "${PICK#=}" ;;
*) awk -F '\t' -v pick="$PICK" 'index($1, pick) == 1 { print; exit }' "$HOME/picker-input" ;;
esac
"#,
    );
    let read = |name: &str| fs::read_to_string(fixture.root.path().join(name)).unwrap();
    let open = |pick: &str, answer: &str| {
        let (output, transcript) =
            fixture.interactive_command(fixture.command().env("PICK", pick), answer);
        (
            output,
            transcript,
            read("picker-args"),
            read("picker-input"),
        )
    };

    // With nothing that needs the user, bare shoal shows the work going on.
    let (_, _, args, rows) = open("", "");
    assert!(args.contains("--prompt\nwork> \n"), "{args}");
    assert!(rows.contains(asker["id"].as_str().unwrap()), "{rows}");
    // An empty inbox still opens, so Tab can reach the other views.
    fixture.terminal_command(fixture.command().arg("inbox").env("PICK", ""), "", true);
    assert!(read("picker-args").contains("--prompt\ninbox> \n"));
    assert_eq!(read("picker-input"), "");

    fixture.ok(&[
        "ask",
        "Which database?",
        "--recommend",
        "SQLite",
        "--workspace",
        "asker",
    ]);
    let id = asker["id"].as_str().unwrap();
    let listed = fixture.run(&["internal", "menu-rows", "inbox"]);
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(listed.starts_with(&format!("question:{id}:")), "{listed}");
    // Tab names the next view by the prompt fzf shows.
    let next = |prompt: &str| {
        let output = fixture
            .command()
            .env("FZF_PROMPT", prompt)
            .args(["internal", "menu-next"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    let to_work = next("inbox> ");
    assert!(
        to_work.contains("change-prompt(work> )")
            && !to_work.contains("unbind(")
            && to_work.ends_with("internal menu-rows work\n"),
        "{to_work}"
    );
    let to_notifications = next("work> ");
    assert!(
        to_notifications.starts_with("unbind(ctrl-t,ctrl-f,ctrl-e,ctrl-s,ctrl-d)+")
            && to_notifications.ends_with("internal menu-rows notifications\n"),
        "{to_notifications}"
    );
    let to_inbox = next("notifications> ");
    assert!(
        to_inbox.starts_with("unbind(ctrl-e,ctrl-s,ctrl-d)+")
            && to_inbox.contains("change-header(enter: act   tab: next view   ")
            && to_inbox.ends_with("internal menu-rows inbox\n"),
        "{to_inbox}"
    );
    // Notifications name their workspace, and showing them marks them read.
    fixture.ok(&["notify", "build finished", "--workspace", "asker"]);
    let shown = fixture.run(&["internal", "menu-rows", "notifications"]);
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(
        shown.starts_with("notification:asker\t") && shown.contains("build finished"),
        "{shown}"
    );
    assert_eq!(fixture.ok(&["notifications"]), serde_json::json!([]));
    // A workspace process cannot see them, even having cleared its token.
    fixture.ok(&["notify", "agent only", "--workspace", "asker"]);
    let cleared = fixture
        .command()
        .args(["exec", "asker", "--", "sh", "-c"])
        // A second command keeps the scoped shell as the listing's parent.
        .arg(r#"env -u SHOAL_SCOPE_TOKEN "$0" internal menu-rows notifications; exit $?"#)
        .arg(env!("CARGO_BIN_EXE_shoal"))
        .output()
        .unwrap();
    assert!(!cleared.status.success(), "{cleared:?}");
    assert!(!String::from_utf8_lossy(&cleared.stdout).contains("agent only"));
    assert_eq!(
        fixture.ok(&["notifications"]).as_array().unwrap().len(),
        1,
        "the refused listing marked nothing read"
    );
    // Nor list the inbox without its token; with it, it gets its own items
    // printed instead of the picker it could not act from.
    let cleared = fixture
        .command()
        .args(["exec", "asker", "--", "sh", "-c"])
        .arg(r#"env -u SHOAL_SCOPE_TOKEN "$0" inbox --print; exit $?"#)
        .arg(env!("CARGO_BIN_EXE_shoal"))
        .output()
        .unwrap();
    assert!(!cleared.status.success(), "{cleared:?}");
    assert!(!String::from_utf8_lossy(&cleared.stdout).contains("Which database?"));
    fs::remove_file(fixture.root.path().join("picker-input")).unwrap();
    let mut scoped = fixture.command();
    scoped.args(["exec", "asker", "--", env!("CARGO_BIN_EXE_shoal"), "inbox"]);
    let (output, transcript) = fixture.terminal_command(&mut scoped, "", true);
    assert!(output.status.success(), "{transcript}");
    assert!(!fixture.root.path().join("picker-input").exists());
    assert!(transcript.contains("Which database?"), "{transcript}");

    // Inside a workspace with items, it starts searching for them.
    let path = Path::new(asker["path"].as_str().unwrap());
    fixture.interactive_command(fixture.command().current_dir(path).env("PICK", ""), "");
    assert!(read("picker-args").contains("--query\n'asker\n"));

    // Now bare shoal opens on the inbox, and Enter answers the question.
    let (output, transcript, args, rows) = open("question:", "Postgres\n");
    assert!(!args.contains("--query"), "{args}");
    assert!(output.status.success(), "{transcript}");
    assert!(args.contains("--prompt\ninbox> \n"), "{args}");
    assert!(
        args.contains(
            "--header\nenter: act   tab: next view   ctrl-t: attach   ctrl-f: diff   ctrl-a: add   ctrl-o: inspect\n"
        ),
        "{args}"
    );
    assert!(
        args.contains("--bind\nstart:unbind(ctrl-e,ctrl-s,ctrl-d)\n"),
        "{args}"
    );
    assert!(args.contains("internal menu-next"), "{args}");
    assert!(rows.contains("Which database?"), "{rows}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Answered question 1 from asker"),
        "{output:?}"
    );
    assert_eq!(fixture.ok(&["inbox"]), serde_json::json!([]));
    // A row Tab loaded is chosen like the first view's: Enter on a
    // notification enters its workspace.
    let (output, transcript, _, _) = open("=notification:quiet", "");
    assert!(output.status.success(), "{transcript}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("quiet"),
        "{output:?}"
    );
}

#[test]
fn inbox_decides_approvals_as_they_were_listed() {
    let fixture = Fixture::new();
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    let agent = fixture.add("agent");
    let pending = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    // Chooses the row $PICK names, `=ID` as given, and approves when asked.
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        r#"#!/bin/sh
cat > "$HOME/picker-input"
case "$*" in
*"Approval> "*) grep '^0' "$HOME/picker-input"; exit ;;
esac
printf '\n'
case "$PICK" in
=*) printf '%s\tloaded\n' "${PICK#=}" ;;
*) awk -F '\t' -v pick="$PICK" 'index($1, pick) == 1 { print; exit }' "$HOME/picker-input" ;;
esac
"#,
    );
    let decide = |pick: &str| {
        let mut command = fixture.command();
        command.arg("inbox").env("PICK", pick);
        fixture.terminal_command(&mut command, "", true)
    };
    let id = pending["id"].as_u64().unwrap();
    let workspace = agent["id"].as_str().unwrap();
    // A request listed with another creation time is not the one shown.
    let (output, transcript) = decide(&format!("=approval:{workspace}:{id}:1"));
    assert!(!output.status.success(), "{output:?} {transcript}");
    assert!(
        transcript.contains("changed since it was listed"),
        "{transcript}"
    );
    let (output, transcript) = decide("approval:");
    assert!(output.status.success(), "{transcript}");
    assert!(read_to_string_lossy(&fixture, "picker-input").contains("serve preview"));
    assert_eq!(fixture.ok(&["approvals"])[0]["status"], "approved");
}

fn read_to_string_lossy(fixture: &Fixture, name: &str) -> String {
    fs::read_to_string(fixture.root.path().join(name)).unwrap_or_default()
}

#[test]
fn workspace_pickers_offer_all_workspaces_after_the_workspaces() {
    let fixture = Fixture::new();
    fixture.add("first");
    let (output, rows) = fixture.pick(&["hold", "list"], "--all", "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains("first"), "{rows}");
    assert_eq!(rows.lines().last(), Some("--all\tAll workspaces"), "{rows}");
    // Stop leaves out workspaces with nothing running.
    let (output, rows) = fixture.pick(&["stop"], "--all", "");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rows, "--all\tAll workspaces\n", "{rows}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("No running agents or commands"),
        "{output:?}"
    );
}

#[test]
fn resume_picker_describes_what_each_workspace_would_restore() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    let second = fixture.add("second");
    let idle = fixture.add("idle");
    let idle_path = Path::new(idle["path"].as_str().unwrap());
    // Nothing to resume: no picker.
    let (output, rows) = fixture.pick(&["resume"], "--all", "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.is_empty(), "{rows}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("No stopped agents or commands"),
        "{output:?}"
    );
    // Every workspace is listed with what it would restore, stopped work first.
    fixture.save_stopped_command(&second);
    let (output, rows) = fixture.pick(&["resume"], second["id"].as_str().unwrap(), "");
    assert!(output.status.success(), "{output:?}");
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert!(
        rows[0].contains("second") && rows[0].contains("stopped: 1 command"),
        "{rows:?}"
    );
    for name in ["first", "idle"] {
        let row = rows.iter().find(|row| row.contains(name)).unwrap();
        assert!(row.contains("nothing to resume"), "{rows:?}");
    }
    assert_eq!(rows[3], "--all\tAll workspaces", "{rows:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Stopped commands in second"),
        "{output:?}"
    );
    // A current workspace without stopped work falls back to the picker.
    fixture.save_stopped_command(&first);
    let (output, rows) = fixture.pick_in(idle_path, &["resume"], first["id"].as_str().unwrap(), "");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rows.lines().count(), 4, "{rows}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Stopped commands in first"),
        "{output:?}"
    );
}

#[test]
fn repository_commands_pick_an_omitted_repository() {
    let fixture = Fixture::new();
    let id = fixture.ok(&["repo", "list"])[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let missing = fixture.run(&["repo", "rm", "--yes"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("missing argument"));

    let (output, rows) = fixture.pick(&["repo", "config"], &id, "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.starts_with(&format!("{id}\t")), "{rows:?}");
    let (output, _) = fixture.pick(&["repo", "rename"], &id, "renamed\n");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["repo", "list"])[0]["name"], "renamed");
    let (output, _) = fixture.pick(&["repo", "rm", "--yes"], &id, "");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["repo", "list"]), serde_json::json!([]));
}

#[test]
fn base_set_picks_a_workspace_of_the_same_repository() {
    let fixture = Fixture::new();
    let lower = fixture.add("lower");
    fixture.add("upper");
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "other\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let foreign = fixture.ok(&["add", other.to_str().unwrap(), "foreign"]);
    let lower = lower["id"].as_str().unwrap();
    let (output, rows) = fixture.pick(&["base", "set", "--workspace", "upper"], lower, "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains(&format!("{lower}\t")), "{rows:?}");
    assert_eq!(rows.lines().count(), 1, "{rows:?}");
    assert!(!rows.contains(foreign["id"].as_str().unwrap()));
    assert_eq!(
        fixture.ok(&["inspect", "upper"])["workspace"]["base_workspace"]["name"],
        "lower"
    );
}

#[test]
fn work_lists_only_the_selected_repository() {
    let fixture = Fixture::new();
    fixture.add("local");
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "other\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    fixture.ok(&["add", other.to_str().unwrap(), "foreign"]);
    let names = |repository: &str| -> Vec<_> {
        fixture
            .ok(&["work", "--repo", repository])
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].clone())
            .collect()
    };
    assert_eq!(names("other"), ["foreign"]);
    assert_eq!(names(fixture.repo.to_str().unwrap()), ["local"]);
    let unknown = fixture.run(&["work", "--repo", "missing"]);
    assert!(!unknown.status.success());
}

#[test]
fn work_lists_only_the_selected_statuses() {
    let fixture = Fixture::new();
    fixture.add("quiet");
    fixture.add("reviewed");
    fixture.ok(&["ready", "--workspace", "reviewed"]);
    let names = |statuses: &[&str]| -> Vec<_> {
        let mut args = vec!["work"];
        for status in statuses {
            args.extend(["--status", status]);
        }
        fixture
            .ok(&args)
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].clone())
            .collect()
    };
    assert_eq!(names(&["inactive"]), ["quiet"]);
    assert_eq!(names(&["ready"]), ["reviewed"]);
    assert_eq!(names(&["inactive,ready"]), ["quiet", "reviewed"]);
    assert_eq!(names(&["failed", "ready"]), ["reviewed"]);
    assert!(names(&["working"]).is_empty());
    let listed = String::from_utf8(fixture.run(&["work", "--status", "inactive"]).stdout).unwrap();
    assert!(
        listed.contains("quiet") && !listed.contains("reviewed"),
        "{listed}"
    );
    assert!(
        !fixture
            .run(&["work", "--status", "unknown"])
            .status
            .success()
    );
}

#[test]
fn inbox_lists_what_needs_the_user_by_kind() {
    let fixture = Fixture::new();
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    for name in ["reviewed", "waiter", "agent", "asker", "quiet"] {
        fixture.add(name);
    }
    assert_eq!(fixture.ok(&["inbox"]), serde_json::json!([]));
    assert_eq!(
        String::from_utf8(fixture.run(&["inbox"]).stdout).unwrap(),
        "Nothing needs you\n"
    );
    fixture.ok(&["ready", "--workspace", "reviewed"]);
    // The agent's state lasts while its execution runs, until `release`
    // exists; the loop's bound only keeps a failed test from leaving it behind.
    let release = fixture.root.path().join("release");
    let waiter_log = fixture.root.path().join("waiter.log");
    let mut waiter = fixture
        .command()
        .args(["exec", "waiter", "--", "sh", "-c"])
        .arg(concat!(
            r#""$0" internal agent-state waiting && i=0 && "#,
            r#"until [ -e "$1" ] || [ $i -ge 3000 ]; do sleep 0.1; i=$((i + 1)); done"#
        ))
        .arg(env!("CARGO_BIN_EXE_shoal"))
        .arg(&release)
        .stdout(Stdio::null())
        .stderr(fs::File::create(&waiter_log).unwrap())
        .spawn()
        .unwrap();
    let mut last = Value::Null;
    let waiting = try_wait_for(|| {
        last = fixture.ok(&["inbox"]);
        (last.as_array().unwrap().len() == 2).then_some(())
    });
    if waiting.is_none() {
        let status = waiter.try_wait();
        let log = fs::read_to_string(&waiter_log).unwrap_or_default();
        panic!("timed out waiting for the waiting agent: waiter {status:?} {log}, inbox {last}");
    }
    pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    fixture.ok(&[
        "ask",
        "Which database?",
        "--recommend",
        "SQLite",
        "--workspace",
        "asker",
    ]);

    let items = fixture.ok(&["inbox"]);
    let listed: Vec<_> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert_eq!(listed, ["question", "approval", "needs_input", "ready"]);
    assert_eq!(items[0]["question"]["workspace"], "asker");
    assert_eq!(items[1]["request"]["workspace"], "agent");
    assert_eq!(items[2]["workspace"]["name"], "waiter");
    let text = String::from_utf8(fixture.run(&["inbox"]).stdout).unwrap();
    let rows: Vec<_> = text.lines().collect();
    assert_eq!(rows.len(), 4, "{text}");
    assert!(
        rows[0].starts_with("question     asker ")
            && rows[0].contains("Which database?")
            && rows[0].ends_with("recommends SQLite"),
        "{text}"
    );
    assert!(rows[1].starts_with("approval     agent ") && rows[1].contains("serve preview"));
    assert_eq!(rows[2].trim_end(), "needs input  waiter");
    assert_eq!(rows[3].trim_end(), "ready        reviewed", "{text}");
    assert!(
        !fixture
            .run(&["inbox", "--repo", "missing"])
            .status
            .success()
    );
    fs::write(&release, "").unwrap();
    assert!(waiter.wait().unwrap().success());
}

#[test]
fn hold_release_picks_an_omitted_hold() {
    let fixture = Fixture::new();
    fixture.add("upper");
    let none = fixture.run(&["hold", "release", "upper"]);
    assert!(String::from_utf8_lossy(&none.stderr).contains("no holds on this workspace"));
    fixture.ok(&["hold", "acquire", "upper", "--name", "review"]);
    fixture.ok(&[
        "hold", "acquire", "upper", "--name", "demo", "--reason", "show",
    ]);
    let (output, rows) = fixture.pick(&["hold", "release", "upper"], "demo", "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains("demo\tdemo (show)") && rows.contains("review\t"));
    let holds = fixture.ok(&["hold", "list", "upper"]);
    assert_eq!(holds.as_array().unwrap().len(), 1);
    assert_eq!(holds[0]["name"], "review");
}

#[test]
fn config_show_picks_a_workspace_outside_any_checkout() {
    let fixture = Fixture::new();
    let workspace = fixture.add("configured");
    let id = workspace["id"].as_str().unwrap();
    let (output, rows) = fixture.pick(&["config", "show"], id, "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.starts_with(&format!("{id}\t")), "{rows:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("default_agent"));
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
    let fzf = bin.join("fzf");
    install_test_script(
        &fzf,
        "#!/bin/sh\ncat > \"$SHOAL_TEST_PICK_INPUT\"\n[ \"$SHOAL_TEST_PICK_ID\" != cancel ] || exit 130\nawk -F '\\t' -v id=\"$SHOAL_TEST_PICK_ID\" '$1 == id { print }' \"$SHOAL_TEST_PICK_INPUT\"\n",
    );
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
fn doctor_daemon_reports_a_bare_flag_on_a_checkout_with_a_working_tree() {
    let fixture = Fixture::new();
    fixture.add("owned");
    let checkout_findings = || {
        let response = fixture.request_with_id(2, serde_json::json!("diagnose"));
        response["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["name"].as_str().unwrap().starts_with("checkout:"))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(checkout_findings()[0]["status"], "ok");

    git(&fixture.repo, &["config", "core.bare", "true"]);
    let findings = checkout_findings();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["status"], "warning");
    let message = findings[0]["message"].as_str().unwrap();
    assert!(message.contains("core.bare=true"), "{message}");
    assert!(message.contains("config core.bare false"), "{message}");
}

#[test]
fn doctor_daemon_reports_untracked_worktrees_without_adopting_them() {
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
    let response = fixture.request_with_id(2, serde_json::json!("diagnose"));
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
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
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

const OWNER_MARKER: &str = "shoal-workspace";

/// Make a workspace look like one recorded before owner markers existed.
fn unmark_owner(workspace: &Value) {
    fs::remove_file(Path::new(workspace["git_dir"].as_str().unwrap()).join(OWNER_MARKER)).unwrap();
}

#[test]
fn owner_marker_keeps_workspaces_verified_when_filesystem_identity_changes() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
    let workspace = fixture.add("marked");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let marker = Path::new(workspace["git_dir"].as_str().unwrap()).join(OWNER_MARKER);
    assert_eq!(
        fs::read_to_string(&marker).unwrap().trim_end(),
        workspace["id"].as_str().unwrap()
    );
    fs::write(path.join("dirty"), "preserve me").unwrap();
    let lease = fixture.ok(&["resource", "acquire", "lock", "marked"]);
    // A reboot renumbering the device, or a restore from backup, changes the
    // stat identity without changing ownership.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute("UPDATE workspaces SET git_dir_id='birth:1:2:3'", [])
        .unwrap();
    fixture.restart();
    let saved = fixture.ok(&["inspect", "marked"])["workspace"].clone();
    assert_eq!(saved["state"], "ready", "{saved}");
    assert_eq!(saved["git_dir_id"], workspace["git_dir_id"]);
    assert!(
        fixture
            .run(&["exec", "marked", "--", "true"])
            .status
            .success()
    );
    assert_eq!(fixture.ok(&["resource", "marked"])["leases"][0], lease);
    // A marker naming another workspace is never accepted.
    fs::write(&marker, "another-workspace\n").unwrap();
    let report = recovery_report(&fixture, &["doctor", "marked", "--repair"]);
    assert_eq!(report[0]["directory"], "unverified");
    assert!(
        report[0]["issues"]
            .to_string()
            .contains("another Shoal workspace")
    );
    assert!(
        !fixture
            .run(&["exec", "marked", "--", "true"])
            .status
            .success()
    );
    assert_eq!(
        fs::read_to_string(path.join("dirty")).unwrap(),
        "preserve me"
    );
}

#[test]
fn doctor_reclaims_unverifiable_worktrees_only_on_explicit_request() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
    let workspace = fixture.add("reclaimed");
    let other = fixture.add("other");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let marker = Path::new(workspace["git_dir"].as_str().unwrap()).join(OWNER_MARKER);
    fs::write(path.join("dirty"), "preserve me").unwrap();
    let lease = fixture.ok(&["resource", "acquire", "lock", "reclaimed"]);
    // An unmarked record whose filesystem identity changed cannot be verified.
    unmark_owner(&workspace);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "UPDATE workspaces SET git_dir_id='0:0' WHERE name='reclaimed'",
        [],
    )
    .unwrap();
    fixture.restart();
    assert_eq!(
        fixture.ok(&["inspect", "reclaimed"])["workspace"]["state"],
        "failed"
    );
    let preview = recovery_report(&fixture, &["doctor", "reclaimed"]);
    assert!(preview[0]["issues"].to_string().contains("--reclaim"));
    let report = recovery_report(&fixture, &["doctor", "reclaimed", "--repair"]);
    assert_eq!(report[0]["directory"], "unverified");
    assert!(
        !fixture
            .run(&["doctor", "reclaimed", "--reclaim"])
            .status
            .success()
    );
    // Reclaim still requires the recorded branch and metadata no one else owns.
    git(path, &["switch", "-q", "-c", "elsewhere"]);
    let refused = recovery_report(&fixture, &["doctor", "reclaimed", "--repair", "--reclaim"]);
    assert!(refused[0]["issues"].to_string().contains("recorded branch"));
    assert_eq!(refused[0]["workspace"]["state"], "failed");
    git(path, &["switch", "-q", "reclaimed"]);
    fs::write(&marker, format!("{}\n", other["id"].as_str().unwrap())).unwrap();
    let refused = recovery_report(&fixture, &["doctor", "reclaimed", "--repair", "--reclaim"]);
    assert!(
        refused[0]["issues"]
            .to_string()
            .contains("already owned by other")
    );
    fs::remove_file(&marker).unwrap();
    assert!(
        !fixture
            .run(&[
                "exec",
                "other",
                "--",
                env!("CARGO_BIN_EXE_shoal"),
                "doctor",
                "reclaimed",
                "--repair",
                "--reclaim"
            ])
            .status
            .success()
    );
    let reclaimed =
        repaired_workspaces(&fixture, &["doctor", "reclaimed", "--repair", "--reclaim"]);
    assert_eq!(reclaimed[0]["workspace"]["state"], "ready");
    assert!(reclaimed[0]["changes"].to_string().contains("Reclaimed"));
    assert_eq!(
        fs::read_to_string(&marker).unwrap().trim_end(),
        workspace["id"].as_str().unwrap()
    );
    assert_eq!(
        reclaimed[0]["workspace"]["git_dir_id"],
        workspace["git_dir_id"]
    );
    assert!(
        fixture
            .run(&["exec", "reclaimed", "--", "true"])
            .status
            .success()
    );
    assert_eq!(fixture.ok(&["resource", "reclaimed"])["leases"][0], lease);
    assert_eq!(
        fs::read_to_string(path.join("dirty")).unwrap(),
        "preserve me"
    );
    fixture.restart();
    assert_eq!(
        fixture.ok(&["inspect", "reclaimed"])["workspace"]["state"],
        "ready"
    );
}

#[test]
fn deleted_worktrees_are_forgotten_even_when_their_metadata_is_unverifiable() {
    let fixture = Fixture::new();
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let mut paths = Vec::new();
    for name in ["deleted", "moved"] {
        let workspace = fixture.add(name);
        unmark_owner(&workspace);
        db.execute(
            "UPDATE workspaces SET git_dir_id='0:0' WHERE name=?1",
            [name],
        )
        .unwrap();
        paths.push(PathBuf::from(workspace["path"].as_str().unwrap()));
    }
    // Git still lists the deleted worktree, but nothing exists to protect.
    fs::remove_dir_all(&paths[0]).unwrap();
    let elsewhere = fixture.root.path().join("elsewhere");
    git(
        &fixture.repo,
        &[
            "worktree",
            "move",
            paths[1].to_str().unwrap(),
            elsewhere.to_str().unwrap(),
        ],
    );
    let report = recovery_report(&fixture, &["doctor", "moved", "--repair"]);
    assert!(
        report[0]["issues"]
            .to_string()
            .contains("cannot be verified")
    );
    assert!(!fixture.run(&["rm", "moved", "--yes"]).status.success());
    assert!(elsewhere.join("tracked").exists());
    assert!(fixture.run(&["rm", "deleted", "--yes"]).status.success());
    let names: Vec<_> = fixture
        .ok(&["work"])
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].clone())
        .collect();
    assert_eq!(names, ["moved"]);
}

#[test]
fn legacy_worktree_identity_upgrades_only_after_verification() {
    use std::os::unix::fs::MetadataExt;

    let mut fixture = Fixture::with_config(Some("[resources.lock]\ncapacity = 3\n"));
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let mut cases = Vec::new();
    for name in ["ready", "failed", "renumbered"] {
        let workspace = fixture.add(name);
        let metadata = fs::metadata(workspace["git_dir"].as_str().unwrap()).unwrap();
        let device = metadata.dev() + u64::from(name == "renumbered");
        let legacy = format!("{device}:{}", metadata.ino());
        unmark_owner(&workspace);
        fs::write(
            Path::new(workspace["path"].as_str().unwrap()).join("dirty"),
            "preserve me",
        )
        .unwrap();
        let lease = fixture.ok(&["resource", "acquire", "lock", name]);
        db.execute(
            "UPDATE workspaces SET git_dir_id=?1,state=?2 WHERE id=?3",
            rusqlite::params![
                legacy,
                if name == "failed" { "failed" } else { "ready" },
                workspace["id"].as_str().unwrap()
            ],
        )
        .unwrap();
        cases.push((name, workspace, legacy, lease));
    }
    fixture.restart();
    for (name, workspace, legacy, lease) in cases {
        let saved = fixture.ok(&["inspect", name])["workspace"].clone();
        if name == "ready" {
            assert_eq!(saved["state"], "ready");
            assert_eq!(saved["git_dir_id"], workspace["git_dir_id"]);
        } else {
            assert_eq!(saved["state"], "failed");
            assert_eq!(saved["git_dir_id"], legacy);
        }
        recovery_report(&fixture, &["doctor", name]);
        assert_eq!(fixture.ok(&["inspect", name])["workspace"], saved);
        let report = recovery_report(&fixture, &["doctor", name, "--repair"]);
        if name == "renumbered" {
            assert_eq!(report[0]["directory"], "unverified");
            assert_eq!(report[0]["workspace"]["git_dir_id"], legacy);
            assert!(report[0]["issues"].to_string().contains("identity changed"));
            assert!(!fixture.run(&["exec", name, "--", "true"]).status.success());
            assert!(!fixture.run(&["rm", name, "--yes"]).status.success());
        } else {
            assert_eq!(report[0]["workspace"]["state"], "ready");
            assert_eq!(
                report[0]["workspace"]["git_dir_id"],
                workspace["git_dir_id"]
            );
            assert!(fixture.run(&["exec", name, "--", "true"]).status.success());
            let repeated = recovery_report(&fixture, &["doctor", name, "--repair"]);
            assert!(repeated[0]["changes"].as_array().unwrap().is_empty());
        }
        assert_eq!(fixture.ok(&["resource", name])["leases"][0], lease);
        assert_eq!(
            fs::read_to_string(Path::new(workspace["path"].as_str().unwrap()).join("dirty"))
                .unwrap(),
            "preserve me"
        );
    }
}

#[test]
fn legacy_identity_still_detects_missing_worktrees_that_moved() {
    use std::os::unix::fs::MetadataExt;

    let fixture = Fixture::new();
    let workspace = fixture.add("legacy");
    let metadata = fs::metadata(workspace["git_dir"].as_str().unwrap()).unwrap();
    let legacy = format!("{}:{}", metadata.dev(), metadata.ino());
    unmark_owner(&workspace);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute("UPDATE workspaces SET git_dir_id=?1", [&legacy])
        .unwrap();
    let moved = fixture.root.path().join("moved");
    git(
        &fixture.repo,
        &[
            "worktree",
            "move",
            workspace["path"].as_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    let report = recovery_report(&fixture, &["doctor", "legacy", "--repair"]);
    assert_eq!(report[0]["directory"], "moved");
    assert_eq!(report[0]["workspace"]["git_dir_id"], legacy);
    assert!(moved.join("tracked").exists());
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
    // Replace the admin directory at its SAME path, proving pathname checks alone
    // are insufficient. Git re-creates it without Shoal's owner marker.
    let admin = Path::new(workspace["git_dir"].as_str().unwrap());
    let old = fixture.root.path().join("old-admin");
    fs::rename(admin, &old).unwrap();
    fs::create_dir(admin).unwrap();
    for entry in fs::read_dir(&old).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() && entry.file_name() != OWNER_MARKER {
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
    assert!(!fixture.run(&["rm", "original", "--yes"]).status.success());
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
    // Commits of their own keep the branches from the kept-branch sweep.
    for name in ["directory-only", "git-removed", "moved"] {
        let workspace = fixture.add(name);
        commit_file(Path::new(workspace["path"].as_str().unwrap()), name, name);
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
    let mut last_names = Vec::new();
    try_wait_for(|| {
        let names: Vec<String> = fixture
            .ok(&["work"])
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].as_str().unwrap().to_owned())
            .collect();
        last_names = names.clone();
        (names == ["moved"]).then_some(())
    })
    .unwrap_or_else(|| panic!("deleted worktrees remain: {last_names:?}"));
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
    commit_file(
        Path::new(workspace["path"].as_str().unwrap()),
        "explicit",
        "work",
    );
    fs::remove_dir_all(workspace["path"].as_str().unwrap()).unwrap();
    let result = fixture.ok(&["rm", "explicit"]);
    assert_eq!(result["branch_deleted"], false);
    assert!(!git(&fixture.repo, &["rev-parse", "refs/heads/explicit"]).is_empty());

    let missing_parent = fixture.add("missing-parent");
    let missing_path = PathBuf::from(missing_parent["path"].as_str().unwrap());
    commit_file(&missing_path, "missing-parent", "work");
    fixture.ok(&["port", "acquire", "web", "missing-parent"]);
    fixture.ok(&["resource", "acquire", "lock", "missing-parent"]);
    git(
        &fixture.repo,
        &["worktree", "remove", missing_path.to_str().unwrap()],
    );
    fs::remove_dir_all(missing_path.parent().unwrap()).unwrap();
    let result = fixture.ok(&["rm", "missing-parent"]);
    assert_eq!(result["branch_deleted"], false);
    assert!(!git(&fixture.repo, &["rev-parse", "refs/heads/missing-parent"]).is_empty());

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
}

fn wait_registered_execution(fixture: &Fixture, workspace: &str) -> Value {
    wait_for("registered execution with a child", || {
        let inspection = fixture.ok(&["inspect", workspace]);
        inspection["executions"]
            .as_array()
            .unwrap()
            .first()
            .filter(|execution| !execution["child"].is_null())
            .cloned()
    })
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
    wait_until("orphan execution to become unknown", || {
        fixture.ok(&["inspect", "orphan"])["executions"][0]["state"] == "unknown"
    });
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

/// Run a command in `workspace`, then crash the daemon, the wrapper and the
/// command together. That leaves nothing to reattach, only the record, which
/// the restarted daemon quarantines as unknown. Unlike a record without a
/// wrapper, its wrapper born during the test keeps the runner's older
/// processes with unreadable environments out of the evidence.
fn crashed_execution(fixture: &mut Fixture, workspace: &str) -> Value {
    let mut wrapper = fixture
        .command()
        .args(["exec", workspace, "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let execution = wait_registered_execution(fixture, workspace);
    fixture.daemon.child.kill().unwrap();
    fixture.daemon.child.wait().unwrap();
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    let group = execution["child"]["pid"].as_i64().unwrap() as i32;
    unsafe { libc::kill(-group, libc::SIGKILL) };
    fixture.restart();
    execution
}

#[test]
fn doctor_recovers_daemon_crash_and_requires_acknowledgement_without_launch_identity() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("crash");
    let port = fixture.ok(&["port", "acquire", "web", "crash"]);
    let execution = crashed_execution(&mut fixture, "crash");
    let report = recovery_report(&fixture, &["doctor", "crash"]);
    assert_eq!(report[0]["executions"][0]["state"], "unknown");
    repaired_workspaces(
        &fixture,
        &["doctor", "crash", "--repair", "--acknowledge-stopped"],
    );
    assert_eq!(fixture.ok(&["port", "crash"])["reserved"][0], port);
    // A wrapper that died before launching its command recorded no child or
    // group.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO executions(id,workspace_id,state,wrapper) VALUES ('unlaunched',?1,'unknown',?2)",
        rusqlite::params![
            workspace["id"].as_str().unwrap(),
            execution["wrapper"].to_string()
        ],
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
    wait_until("orphan execution to become unknown", || {
        fixture.ok(&["inspect", "orphan"])["executions"][0]["state"] == "unknown"
    });
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
    wait_until("orphan ready signal", || root.join("ready").exists());
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    wait_until("orphan execution to become unknown", || {
        fixture.ok(&["inspect", "orphan"])["executions"][0]["state"] == "unknown"
    });
    let mut stopping = fixture
        .command()
        .args(["stop", "orphan"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("orphan stopping signal", || root.join("stopping").exists());
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
    for program in ["codex", "t3"] {
        let stub = bin.join(program);
        install_test_script_with_mode(
            &stub,
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\"\nexit 7\n",
            0o700,
        );
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
                .env("PATH", fixture.restricted_path(&bin))
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
            .env("PATH", fixture.restricted_path(&bin))
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("their own workspace and those split from it")
        );
    }
    // Without a Codex on PATH, the shortcut fails instead of reaching the
    // runner's own installation.
    let output = fixture
        .command()
        .args(["codex", "desktop"])
        .env("PATH", fixture.system_path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
}

fn commit_file(path: &Path, file: &str, contents: &str) {
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
fn sync_fetches_the_remote_and_fast_forwards_only_the_default_branch() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    let head = git(path, &["rev-parse", "HEAD"]);
    let stale = git(&fixture.repo, &["rev-parse", "main"]);
    let author = upstream_remote(&fixture);
    let upstream = git(&author, &["rev-parse", "HEAD"]);
    git(&author, &["switch", "-c", "feature/api"]);
    commit_file(&author, "api", "feature\n");
    git(&author, &["push", "origin", "feature/api"]);
    let feature = git(&author, &["rev-parse", "HEAD"]);
    let binary = env!("CARGO_BIN_EXE_shoal");
    // A scoped agent syncs its own repository without moving its branch.
    let output = fixture.run(&["exec", "worker", "--", binary, "--json", "sync"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["remote"], "origin");
    assert_eq!(result["default_branch"]["updated"], true);
    assert_eq!(result["default_branch"]["previous_commit"], stale.trim());
    assert_eq!(result["default_branch"]["commit"], upstream.trim());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), upstream);
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "origin/feature/api"]),
        feature
    );
    assert_eq!(git(path, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&fixture.repo, &["for-each-ref", "refs/shoal/"]), "");
    // A diverged default branch is reported, not rewritten.
    commit_file(&fixture.repo, "local", "local\n");
    let local = git(&fixture.repo, &["rev-parse", "main"]);
    git(&author, &["switch", "main"]);
    commit_file(&author, "remote", "remote\n");
    git(&author, &["push", "origin", "main"]);
    let output = fixture.run(&["sync", fixture.repo.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("diverged"));
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), local);
    // Scoped callers cannot sync another repository.
    let other = fixture.root.path().join("other");
    git(fixture.root.path(), &["init", "-b", "main", "other"]);
    commit_file(&other, "file", "other\n");
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let output = fixture.run(&[
        "exec",
        "worker",
        "--",
        binary,
        "sync",
        other.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("own repository"));
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
    let second_path = Path::new(second["path"].as_str().unwrap());
    let repository = fixture.ok(&["repo", "list"])[0].clone();
    let workspaces_dir = Path::new(repository["workspaces_dir"].as_str().unwrap());
    let repo_key = fs::canonicalize(&fixture.repo).unwrap();
    let workspaces_key = fs::canonicalize(workspaces_dir).unwrap();
    let first_key = fs::canonicalize(first_path).unwrap();
    let second_key = fs::canonicalize(second_path).unwrap();
    fs::write(
        fixture.root.path().join(".claude.json"),
        serde_json::json!({
            "projects": {
                repo_key.to_str().unwrap(): {"hasTrustDialogAccepted": true},
                workspaces_key.to_str().unwrap(): {"hasTrustDialogAccepted": true},
                first_key.to_str().unwrap(): {"hasTrustDialogAccepted": true},
                second_key.to_str().unwrap(): {"hasTrustDialogAccepted": true},
                "/keep": {"hasTrustDialogAccepted": true}
            }
        })
        .to_string(),
    )
    .unwrap();
    fs::create_dir_all(fixture.root.path().join(".codex")).unwrap();
    fs::write(
        fixture.root.path().join(".codex/config.toml"),
        format!(
            "[projects]\n{} = {{ trust_level = 'trusted' }}\n{} = {{ trust_level = 'trusted' }}\n{} = {{ trust_level = 'trusted' }}\n{} = {{ trust_level = 'trusted' }}\n'/keep' = {{ trust_level = 'trusted' }}\n",
            toml_edit::Key::new(repo_key.to_str().unwrap()),
            toml_edit::Key::new(workspaces_key.to_str().unwrap()),
            toml_edit::Key::new(first_key.to_str().unwrap()),
            toml_edit::Key::new(second_key.to_str().unwrap()),
        ),
    )
    .unwrap();
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
    wait_until("execution registration", || {
        !fixture.ok(&["inspect", "first"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    let result = fixture.ok(&["repo", "rm", "doomed", "-y"]);
    assert_eq!(result["removed"], true);
    assert_eq!(result["repository_id"], repo["id"]);
    assert_eq!(result["workspaces_removed"], 2);
    assert!(!command.wait().unwrap().success());
    assert!(!fixture.repo.exists());
    assert!(!first_path.exists());
    assert!(!Path::new(second["path"].as_str().unwrap()).exists());
    assert!(fixture.ok(&["repo", "list"]).as_array().unwrap().is_empty());
    assert!(fixture.ok(&["work"]).as_array().unwrap().is_empty());
    let claude: Value = serde_json::from_str(
        &fs::read_to_string(fixture.root.path().join(".claude.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        claude["projects"],
        serde_json::json!({"/keep": {"hasTrustDialogAccepted": true}})
    );
    let codex: toml::Value = toml::from_str(
        &fs::read_to_string(fixture.root.path().join(".codex/config.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(codex["projects"].as_table().unwrap().len(), 1);
    assert_eq!(
        codex["projects"]["/keep"]["trust_level"].as_str(),
        Some("trusted")
    );
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
fn removal_confirmation_says_when_the_checkpoint_cannot_keep_the_files() {
    let fixture = Fixture::with_config(Some("[cleanup.restore]\narchive_max_mb = 0\n"));
    let workspace = fixture.add("large");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("uncommitted"), "too large to archive").unwrap();
    let (no, prompt) = fixture.interactive(&["rm", "large"], "n\n");
    assert!(!no.status.success());
    assert!(prompt.contains("so they cannot be restored"), "{prompt}");
    assert!(
        !prompt.contains("shoal restore brings them back"),
        "{prompt}"
    );
    assert!(path.join("uncommitted").exists());
}

#[test]
fn interactive_removal_confirms_and_defaults_to_no_without_affecting_scripts() {
    let fixture = Fixture::new();
    fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    let workspace = fixture.add("worker");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("uncommitted"), "keep until approved").unwrap();
    let piped = fixture.run(&["rm", "worker"]);
    assert!(!piped.status.success());
    assert!(String::from_utf8_lossy(&piped.stderr).contains("pass --yes"));
    let (no, prompt) = fixture.interactive(&["rm", "worker"], "n\n");
    assert!(!no.status.success());
    assert!(prompt.contains("Are you sure? [y/N]"), "{prompt}");
    assert!(
        prompt.contains("Deletes: uncommitted changes or untracked files"),
        "{prompt}"
    );
    assert!(
        prompt.contains("shoal restore brings them back"),
        "{prompt}"
    );
    assert!(prompt.contains("?? uncommitted"), "{prompt}");
    assert!(path.join("uncommitted").exists());
    let (yes, prompt) = fixture.interactive(&["rm", "worker"], "maybe\ny\n");
    assert!(yes.status.success(), "{prompt}");
    assert!(prompt.contains("Please enter y or n."), "{prompt}");
    assert!(!path.exists());
    // The branch had no commits of its own.
    let branch_exists = git(
        &fixture.repo,
        &["branch", "--list", workspace["branch"].as_str().unwrap()],
    );
    assert!(branch_exists.trim().is_empty());
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
fn repository_removal_confirmation_identifies_the_resolved_checkout() {
    let fixture = Fixture::new();
    let source = fixture.root.path().join("saldoir-server.git");
    git(
        &fixture.repo,
        &["clone", "--bare", ".", source.to_str().unwrap()],
    );
    let url = format!("file://{}", source.display());
    let checkout = fixture.root.path().join("saldera-server/.checkout");
    let repository = fixture.ok(&["repo", "add", &url, "--path", checkout.to_str().unwrap()]);
    let (output, prompt) = fixture.interactive(&["repo", "rm", "saldoir-server"], "n\n");
    assert!(!output.status.success());
    assert!(
        prompt.contains("Delete repository: saldoir-server"),
        "{prompt}"
    );
    assert!(prompt.contains(&format!("Source: {url}")), "{prompt}");
    assert!(
        prompt.contains(&format!(
            "Checkout: {}",
            repository["path"].as_str().unwrap()
        )),
        "{prompt}"
    );
    assert!(checkout.exists());
    assert!(fixture.repo.exists());
}

#[test]
fn repository_removal_keeps_the_confirmed_target_when_a_name_is_reassigned() {
    use std::io::Read;

    let fixture = Fixture::new();
    let selected = fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "keep\n")]);
    let replacement = fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let (mut master, slave) = pty::open();
    let mut child = fixture
        .command()
        .args(["repo", "rm", "project"])
        .stdin(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut transcript = String::new();
    try_wait_for(|| {
        let _ = master.read_to_string(&mut transcript);
        if transcript.contains("Are you sure? [y/N]") {
            return Some(());
        }
        assert!(child.try_wait().unwrap().is_none(), "{transcript}");
        None
    })
    .unwrap_or_else(|| panic!("hung confirmation: {transcript}"));
    fixture.ok(&[
        "repo",
        "rename",
        selected["id"].as_str().unwrap(),
        "renamed",
    ]);
    fixture.ok(&[
        "repo",
        "rename",
        replacement["id"].as_str().unwrap(),
        "project",
    ]);
    master.write_all(b"y\n").unwrap();
    try_wait_for(|| {
        let _ = master.read_to_string(&mut transcript);
        child.try_wait().unwrap()
    })
    .unwrap_or_else(|| panic!("hung removal: {transcript}"));
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{transcript}");
    assert!(!fixture.repo.exists());
    assert!(other.join("tracked").exists());
    let repositories = fixture.ok(&["repo", "list"]);
    assert!(
        repositories
            .as_array()
            .unwrap()
            .iter()
            .any(|repo| repo["id"] == replacement["id"])
    );
}

#[test]
fn removal_confirmation_lists_git_changes_before_asking() {
    let fixture = Fixture::new();
    let workspace = fixture.add("preview");
    let path = Path::new(workspace["path"].as_str().unwrap());
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

    let (no, prompt) = fixture.interactive(&["rm", "preview"], "n\n");
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
    let removed = fixture.run(&["rm", "preview", "--yes"]);
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
    let (no, prompt) = fixture.interactive(&["rm", "large-preview"], "n\n");
    assert!(!no.status.success(), "{prompt}");
    assert_eq!(prompt.matches("?? untracked/").count(), 50, "{prompt}");
    assert!(prompt.contains("450 more entries omitted"), "{prompt}");
    assert!(prompt.contains("Are you sure? [y/N]"), "{prompt}");
    assert_eq!(fs::read_dir(path.join("untracked")).unwrap().count(), 500);
    let piped = fixture.run(&["rm", "large-preview"]);
    assert!(!piped.status.success());
    let error = String::from_utf8_lossy(&piped.stderr);
    assert!(error.contains("pass --yes"), "{error}");
    assert!(!error.contains("?? untracked/"), "{error}");
    fixture.ok(&["rm", "large-preview", "--yes"]);
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
    // Removal refuses a workspace while setup runs, so this race skips setup.
    let fixture = Fixture::with_config(Some("copy_ignored = false\n"));
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
    let add = add.wait_with_output().unwrap();
    if !add.status.success() {
        assert!(
            String::from_utf8_lossy(&add.stderr).contains("repository"),
            "unexpected add failure: {add:?}"
        );
    }
    assert!(fixture.ok(&["repo", "list"]).as_array().unwrap().is_empty());
    assert!(fixture.ok(&["work"]).as_array().unwrap().is_empty());
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
    install_test_script(
        fixture.repo.join(script),
        r#"#!/bin/sh
printf 'setup output\n'
printf '%s' "$PWD" > setup-cwd
printf '%s' "$SHOAL_WORKSPACE" > setup-workspace
"$SHOAL_TEST_BIN" --json status > during-setup.json || exit 91
"#,
    );
    commit_resource_config(&fixture.repo, &format!("setup_cmd = {script:?}\n"));
    let bin = fixture.root.path().join("bin");
    install_test_script(
        bin.join("codex"),
        "#!/bin/sh\ntest -f setup-cwd || exit 92\nprintf agent > agent-started\n",
    );
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
fn ignored_files_are_copied_before_setup_unless_turned_off() {
    let fixture = Fixture::new();
    fs::write(fixture.repo.join(".gitignore"), ".env\nlocal/\n").unwrap();
    fs::write(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\ncat .env local/signing > setup-saw\n",
    )
    .unwrap();
    fs::set_permissions(
        fixture.repo.join("setup.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_resource_config(&fixture.repo, "setup_cmd = 'setup.sh'\n");
    fs::write(fixture.repo.join(".env"), "TOKEN=1\n").unwrap();
    fs::create_dir(fixture.repo.join("local")).unwrap();
    fs::write(fixture.repo.join("local/signing"), "team\n").unwrap();
    let workspace = fixture.ok(&["add", fixture.repo.to_str().unwrap(), "copied"]);
    assert_eq!(workspace["state"], "ready");
    let path = Path::new(workspace["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(path.join("setup-saw")).unwrap(),
        "TOKEN=1\nteam\n"
    );

    // Rerunning setup keeps files the workspace already has.
    fs::write(path.join(".env"), "TOKEN=2\n").unwrap();
    assert_eq!(fixture.ok(&["setup", "copied"])["state"], "ready");
    assert_eq!(fs::read_to_string(path.join(".env")).unwrap(), "TOKEN=2\n");

    commit_resource_config(&fixture.repo, "copy_ignored = false\n");
    let workspace = fixture.ok(&["add", fixture.repo.to_str().unwrap(), "uncopied"]);
    assert_eq!(workspace["state"], "ready");
    let path = Path::new(workspace["path"].as_str().unwrap());
    assert!(!path.join(".env").exists());
}

#[test]
fn setup_cmd_local_override_absolute_path_failure_and_retry() {
    let fixture = Fixture::new();
    commit_resource_config(&fixture.repo, "setup_cmd = 'must-not-run'\n");
    let script = fixture.root.path().join("system setup.sh");
    install_test_script(
        &script,
        "#!/bin/sh\nprintf partial > setup-result\nexit 17\n",
    );
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
    install_test_script(
        bin.join("claude"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    );
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
    install_test_script(
        bin.join("codex"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    );
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
fn setup_verification_failure_reports_survivor_and_keeps_work_without_prompting() {
    for json in [false, true] {
        let fixture = Fixture::new();
        let script = fixture.root.path().join("setup-survivor");
        install_test_script(
            &script,
            r#"#!/usr/bin/env python3
import subprocess, sys
from pathlib import Path
child = subprocess.Popen([sys.executable, '-c', 'import time; print("ready", flush=True); time.sleep(600)'], start_new_session=True,
    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
assert child.stdout.readline() == b'ready\n'
Path('survivor-pid').write_text(str(child.pid))
Path('setup-result').write_text('complete')
"#,
        );
        commit_resource_config(
            &fixture.repo,
            &format!(
                "setup_cmd = {:?}\n[commands]\npi = ['sh', '-c', 'touch agent-started']\n",
                script.to_str().unwrap()
            ),
        );
        let mut command = fixture.command();
        command.args([
            "add",
            fixture.repo.to_str().unwrap(),
            "survivor",
            "--agent",
            "pi",
        ]);
        if json {
            command.arg("--json");
        }
        let (output, transcript) = fixture.interactive_command(&mut command, "");
        let inspection = fixture.ok(&["inspect", "survivor"]);
        let path = Path::new(inspection["workspace"]["path"].as_str().unwrap());
        let pid = fs::read_to_string(path.join("survivor-pid")).unwrap();
        // Reap the test-owned survivor before making assertions about the output.
        repaired_workspaces(
            &fixture,
            &[
                "doctor",
                "survivor",
                "--repair",
                "--stop",
                "--acknowledge-stopped",
            ],
        );
        assert!(!output.status.success(), "{transcript}");
        assert_eq!(inspection["workspace"]["state"], "failed");
        assert_eq!(
            fs::read_to_string(path.join("setup-result")).unwrap(),
            "complete"
        );
        assert!(!path.join("agent-started").exists());
        assert!(
            transcript.contains("Setup verification incomplete"),
            "{transcript}"
        );
        assert!(transcript.contains("command exit 0"), "{transcript}");
        assert!(
            transcript.contains(&format!("owned processes still running (PID {pid})")),
            "{transcript}"
        );
        assert!(
            transcript.contains("shoal doctor survivor --repair"),
            "{transcript}"
        );
        assert!(!transcript.contains("[i]gnore"), "{transcript}");
    }
}

#[test]
fn setup_interruption_preserves_work_and_blocks_concurrent_execution() {
    let mut fixture = Fixture::new();
    install_test_script(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\nprintf partial > setup-started\nwhile :; do sleep 1; done\n",
    );
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
    wait_until("setup to start", || {
        if path.join("setup-started").exists() {
            return true;
        }
        assert!(add.try_wait().unwrap().is_none());
        false
    });
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
    let status = wait_for("setup to stop", || add.try_wait().unwrap());
    assert!(!status.success());
    let inspection = fixture.ok(&["inspect", "interrupted"]);
    assert_eq!(inspection["workspace"]["state"], "failed");
    assert_eq!(inspection["executions"], serde_json::json!([]));
    fixture.restart();
    assert_eq!(
        fixture.ok(&["inspect", "interrupted"])["workspace"]["state"],
        "failed"
    );
    assert!(path.join("setup-started").exists());
    fixture.ok(&["rm", "interrupted", "--yes"]);
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
        install_test_script(fixture.repo.join(name), body);
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
    install_test_script(
        bin.join("codex"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    );
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
    let output = fixture.run(&["rm", "hooked", "--yes"]);
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
    fixture.ok(&["rm", "hooked", "--yes"]);
    assert!(!path.exists());
    assert!(
        !fixture
            .ok(&["work"])
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["name"] == "hooked")
    );
}

#[test]
fn repository_removal_cleans_more_agent_configs_than_fit_in_a_frame() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let repository = fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "many",
    ]);
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    // Far more than one 64 KiB protocol frame of kept locations.
    let mut db = rusqlite::Connection::open(home.join("state/state.db")).unwrap();
    db.busy_timeout(Duration::from_secs(60)).unwrap();
    let tx = db.transaction().unwrap();
    for index in 0..2000 {
        let environment = serde_json::json!({
            "CLAUDE_CONFIG_DIR": home.join(format!("claude-{index:04}")),
        });
        tx.execute(
            "INSERT INTO repository_agent_environments(repository_id,environment) VALUES (?1,?2)",
            rusqlite::params![repository["id"].as_str().unwrap(), environment.to_string()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let config = home.join("claude-1999/.claude.json");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        serde_json::json!({"projects": {
            checkout.to_str().unwrap(): {"hasTrustDialogAccepted": true},
            "/keep": {"hasTrustDialogAccepted": true}
        }})
        .to_string(),
    )
    .unwrap();

    fixture.ok(&["repo", "rm", "many", "--yes"]);
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(
        root["projects"],
        serde_json::json!({"/keep": {"hasTrustDialogAccepted": true}})
    );
    // Cleaned, so the daemon forgets them.
    let kept: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM repository_agent_environments",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 0);
}

#[test]
fn trust_entries_follow_the_config_directories_env_gives_agents() {
    // A launch can fail after trust is written, here at its diff base.
    let fixture = Fixture::with_config(Some("[commands]\nclaude = ['claude', '{diff_base}']\n"));
    let home = fixture.root.path();
    // Each workspace's agents get their own directories, from the global
    // config and from the worktree's file.
    let config = home.join(".config/shoal/config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "[env]\nCODEX_HOME = '{}/codex-{{workspace}}'\n",
        home.display()
    ));
    fs::write(&config, text).unwrap();
    assert!(fixture.run(&["daemon", "reload"]).status.success());
    install_test_script(
        fixture.repo.join("pre-remove.sh"),
        "#!/bin/sh\ntest ! -f fail-removal || exit 3\n",
    );
    commit_resource_config(
        &fixture.repo,
        &format!(
            "pre_remove_cmd = 'pre-remove.sh'\n[env]\nCLAUDE_CONFIG_DIR = '{}/claude-{{workspace}}'\n",
            home.display()
        ),
    );
    fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "configured",
    ]);
    fixture.add("early");
    git(&fixture.repo, &["branch", "side"]);
    fixture.ok(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "late",
        "--base",
        "side",
    ]);
    git(&fixture.repo, &["branch", "-D", "side"]);
    let first = fixture.add("first");
    let first_path = Path::new(first["path"].as_str().unwrap()).to_owned();
    let second = fixture.add("second");
    let second_path = Path::new(second["path"].as_str().unwrap());
    let repository = fixture.ok(&["repo", "list"])[0].clone();
    let directory = fs::canonicalize(repository["workspaces_dir"].as_str().unwrap()).unwrap();
    let checkout = fs::canonicalize(repository["path"].as_str().unwrap()).unwrap();
    install_test_script(home.join("bin/claude"), "#!/bin/sh\nexit 0\n");

    for workspace in ["early", "first"] {
        assert!(fixture.run(&["claude", workspace]).status.success());
    }
    assert!(!fixture.run(&["claude", "late"]).status.success());
    assert!(home.join("claude-late/.claude.json").exists());
    let root: Value =
        serde_json::from_str(&fs::read_to_string(home.join("claude-first/.claude.json")).unwrap())
            .unwrap();
    for key in [&checkout, &directory] {
        assert_eq!(
            root["projects"][key.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
    }
    assert!(!home.join(".claude.json").exists());

    // Removing the repository clears them there too, though one workspace
    // went before and another in an earlier, failed attempt.
    for workspace in ["early", "first"] {
        let codex = home.join(format!("codex-{workspace}/config.toml"));
        fs::create_dir_all(codex.parent().unwrap()).unwrap();
        fs::write(
            &codex,
            format!(
                "[projects]\n{} = {{ trust_level = 'trusted' }}\n{} = {{ trust_level = 'trusted' }}\n'/keep' = {{ trust_level = 'trusted' }}\n",
                toml_edit::Key::new(checkout.to_str().unwrap()),
                toml_edit::Key::new(directory.to_str().unwrap()),
            ),
        )
        .unwrap();
    }
    // Removal cleans where the agents ran, not where `[env]` points now.
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("/codex-{workspace}", "/codex-changed-{workspace}"),
    )
    .unwrap();
    assert!(fixture.run(&["daemon", "reload"]).status.success());
    fixture.ok(&["rm", "early", "--yes"]);
    fs::write(second_path.join("fail-removal"), "busy").unwrap();
    assert!(
        !fixture
            .run(&["repo", "rm", "configured", "--yes"])
            .status
            .success()
    );
    assert!(!first_path.exists());
    fs::remove_file(second_path.join("fail-removal")).unwrap();
    fixture.ok(&["repo", "rm", "configured", "--yes"]);
    for workspace in ["early", "first", "late"] {
        let claude = home.join(format!("claude-{workspace}/.claude.json"));
        let root: Value = serde_json::from_str(&fs::read_to_string(&claude).unwrap()).unwrap();
        assert_eq!(root["projects"], serde_json::json!({}), "{workspace}");
    }
    for workspace in ["early", "first"] {
        let codex = home.join(format!("codex-{workspace}/config.toml"));
        let codex: toml::Value = toml::from_str(&fs::read_to_string(&codex).unwrap()).unwrap();
        assert_eq!(
            codex["projects"],
            toml::toml! { "/keep" = { trust_level = "trusted" } }.into(),
            "{workspace}"
        );
    }
}

#[test]
fn claude_launch_trusts_checkout_and_shared_repository_directory() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let workspace = fixture.add("trusted");
    let path = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
    let repository = fixture.ok(&["repo", "list"])[0].clone();
    let directory = fs::canonicalize(repository["workspaces_dir"].as_str().unwrap()).unwrap();
    // Claude Code keys a worktree's trust by the repository checkout.
    let checkout = fs::canonicalize(repository["path"].as_str().unwrap()).unwrap();
    let keys = [checkout.as_path(), directory.as_path()];
    let bin = home.join("bin");
    install_test_script(bin.join("claude"), "#!/bin/sh\nexit 0\n");
    let config = home.join(".claude.json");

    // First launch creates a trusted entry even before Claude has a config.
    assert!(fixture.run(&["claude", "trusted"]).status.success());
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    for path in keys {
        assert_eq!(
            root["projects"][path.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
    }
    assert!(root["projects"][path.to_str().unwrap()].is_null());
    assert!(root["projects"][fixture.shoal_dir().to_str().unwrap()].is_null());

    fs::write(&config, r#"{"numStartups": 1, "projects": {}}"#).unwrap();
    assert!(fixture.run(&["claude", "trusted"]).status.success());
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(root["numStartups"], 1);
    for path in keys {
        assert_eq!(
            root["projects"][path.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
    }
    assert!(root["projects"][path.to_str().unwrap()].is_null());
    assert!(root["projects"][fixture.shoal_dir().to_str().unwrap()].is_null());

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
    for path in keys {
        assert_eq!(
            overridden["projects"][path.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
    }
    assert!(overridden["projects"][path.to_str().unwrap()].is_null());
    let untouched: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(untouched["projects"], serde_json::json!({}));

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
fn codex_launches_trust_checkout_shared_repository_directory_and_external_workspace() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    // An explicit path must not make its arbitrary parent a trust root.
    let external = home.join("external/workspace");
    let workspace = fixture.ok(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "trusted",
        "--path",
        external.to_str().unwrap(),
    ]);
    let path = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
    let repository = fixture.ok(&["repo", "list"])[0].clone();
    let directory = fs::canonicalize(repository["workspaces_dir"].as_str().unwrap()).unwrap();
    let checkout = fs::canonicalize(repository["path"].as_str().unwrap()).unwrap();
    let keys = [checkout.as_path(), path.as_path(), directory.as_path()];
    let bin = home.join("bin");
    // Inspect the config from the child so trust must precede the launch.
    install_test_script(
        bin.join("codex"),
        "#!/bin/sh\ncat \"${CODEX_HOME:-$HOME/.codex}/config.toml\"\n",
    );
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
            for path in keys {
                assert_eq!(
                    root["projects"][path.to_str().unwrap()]["trust_level"].as_str(),
                    Some("trusted")
                );
            }
            assert!(
                root["projects"]
                    .get(path.parent().unwrap().to_str().unwrap())
                    .is_none()
            );
            assert!(
                root["projects"]
                    .get(fixture.shoal_dir().to_str().unwrap())
                    .is_none()
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
    install_test_script_with_mode(
        bin.join("gh"),
        "#!/bin/sh\n# The issue has no blocking issues.\n[ \"$1\" = api ] && { printf 'HTTP/2.0 200 OK\\n\\n[]'; exit; }\nprintf '%s\\0' \"$@\" > \"$ISSUE_ARGS\"\ncat \"$ISSUE_RESPONSE\"\n",
        0o700,
    );
    install_forgejo_curl(&bin);
    for tool in ["codex", "claude"] {
        let script = bin.join(tool);
        install_test_script_with_mode(
            &script,
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$AGENT_ARGS\"\n",
            0o700,
        );
    }
    let response = fixture.root.path().join("issue-response");
    let issue_args = fixture.root.path().join("issue-args");
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
        if host == "github.com" {
            let text =
                serde_json::json!({"state":"OPEN", "number": number, "title": title, "body": body});
            fs::write(&response, text.to_string()).unwrap();
        } else {
            forgejo_api(
                fixture.root.path(),
                &format!("repos/team/project/issues/{number}"),
                &serde_json::json!({"number": number, "title": title, "body": body,
                    "state": "open", "labels": [], "pull_request": null}),
            );
            forgejo_api(
                fixture.root.path(),
                &format!("repos/team/project/issues/{number}/dependencies?limit=50&page=1"),
                &serde_json::json!([]),
            );
        }
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
            .env("PATH", fixture.restricted_path(&bin))
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
        let name = format!("{number}-fix-api-timeout");
        assert_eq!(workspace["name"], name);
        assert_eq!(workspace["branch"], name);
        assert_eq!(fixture.ok(&["inspect", &name])["issue"]["url"], url);
        if host == "github.com" {
            let invocation = fs::read_to_string(&issue_args).unwrap();
            assert!(invocation.contains(&format!("issue\0view\0{number}\0")));
            assert!(invocation.contains("--repo\0github.com/team/project\0"));
        } else {
            let urls = fs::read_to_string(fixture.root.path().join("curl-urls")).unwrap();
            assert!(urls.contains(&format!(
                "https://forge.example/api/v1/repos/team/project/issues/{number}\n"
            )));
        }
        let invocation = strip_shoal_arguments(&fs::read_to_string(&agent_args).unwrap(), '\0');
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
        .env("PATH", fixture.restricted_path(&bin))
        .env("ISSUE_RESPONSE", &response)
        .env("ISSUE_ARGS", &issue_args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["name"],
        "custom-issue-name"
    );
}

#[test]
fn the_built_in_issue_prompt_ignores_retired_issue_templates() {
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
printf '%s' '{"state":"OPEN","number":44,"title":"Literal {body}","body":"$(false)"}'
"#,
        ),
        ("claude", "#!/bin/sh\nprintf '%s\\0' \"$@\"\n"),
    ] {
        let path = bin.join(tool);
        install_test_script_with_mode(&path, script, 0o700);
    }
    fixture.add_github_origin();
    let local = fixture.root.path().join("local.toml");
    fs::write(&local, "issue_template = 'saved {body}'").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        local.to_str().unwrap(),
    ]);
    let output = fixture
        .command()
        .args([
            "--json",
            "add",
            fixture.repo.to_str().unwrap(),
            "template",
            "--base",
            "HEAD",
            "--issue",
            "44",
            "--agent",
            "claude",
        ])
        .env("PATH", fixture.restricted_path(&bin))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let args = strip_shoal_arguments(stdout.split_once('\n').unwrap().1, '\0');
    let prompt = args.split('\0').next().unwrap();
    assert!(
        prompt.starts_with("Work on issue #44: Literal {body}\n"),
        "{prompt}"
    );
    assert!(
        prompt.contains("\nIssue description:\n$(false)\n"),
        "{prompt}"
    );
    assert!(
        !prompt.contains("global") && !prompt.contains("saved"),
        "{prompt}"
    );
    let output = fixture
        .command()
        .args(["--json", "config", "show", "template"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    for layer in ["global config", "saved repository config"] {
        assert!(
            stderr.contains(&format!("warning: {layer}: issue_template")),
            "{stderr}"
        );
    }
}

#[test]
fn add_item_finds_the_repository_and_starts_the_default_agent() {
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
    let gh = bin.join("gh");
    install_test_script_with_mode(
        &gh,
        "#!/bin/sh\n# The issue has no blocking issues.\n[ \"$1\" = api ] && { printf 'HTTP/2.0 200 OK\\n\\n[]'; exit; }\nprintf '%s\\0' \"$@\" > \"$ISSUE_ARGS\"\ncat \"$ISSUE_RESPONSE\"\n",
        0o700,
    );
    for tool in ["codex", "claude"] {
        let script = bin.join(tool);
        install_test_script_with_mode(
            &script,
            "#!/bin/sh\nprintf '%s\\0' \"$(basename \"$0\")\" \"$@\" > \"$AGENT_ARGS\"\n",
            0o700,
        );
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
            serde_json::json!({"state":"OPEN", "number": number, "title": "Paste an issue", "body": body})
                .to_string(),
        )
        .unwrap();
        let url = format!("https://github.com/team/project/issues/{number}");
        let number_input = number.to_string();
        let input = if number >= 45 { &number_input } else { &url };
        let mut args = vec!["--json", "add", input, "--base", "HEAD"];
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
                fixture.ok(&["inspect", "41-paste-issue"])["workspace"]["path"]
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
            .env("PATH", fixture.restricted_path(&bin))
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
        let name = format!("{number}-paste-issue");
        assert_eq!(workspace["name"], name);
        assert_eq!(workspace["branch"], name);
        assert!(fs::read_to_string(&issue_args).unwrap().contains(&format!(
            "issue\0view\0{number}\0--repo\0github.com/team/project\0"
        )));
        let invocation = strip_shoal_arguments(&fs::read_to_string(&agent_args).unwrap(), '\0');
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
            serde_json::json!({"state":"OPEN", "number": number, "title": "Create workspace", "body": body})
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
            .env("PATH", fixture.restricted_path(&bin))
            .env("ISSUE_RESPONSE", &response)
            .env("ISSUE_ARGS", &issue_args)
            .env("AGENT_ARGS", &agent_args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(workspace["name"], format!("{number}-create-workspace"));
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
        .env("PATH", fixture.restricted_path(&bin))
        .env("AGENT_ARGS", &agent_args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!agent_args.exists());
}

#[test]
fn agent_picker_can_skip_launch_select_agents_or_cancel() {
    let fixture = Fixture::with_config(Some("[ai.none]\ncommand = ['agent-probe']\n"));
    fixture.add_github_origin();
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    for (tool, script) in [
        (
            "gh",
            r#"#!/bin/sh
printf '{"state":"OPEN","number":298,"title":"Pick an agent","body":"Issue context"}'
"#,
        ),
        (
            "fzf",
            r#"#!/bin/sh
cat > "$HOME/picker-input"
choice=$(cat "$HOME/choice")
[ "$choice" != cancel ] || exit 130
awk -F '\t' -v choice="$choice" '$2 == choice {print}' "$HOME/picker-input"
"#,
        ),
        (
            "codex",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/agent-args\"\n",
        ),
        (
            "agent-probe",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/agent-args\"\n",
        ),
    ] {
        let path = bin.join(tool);
        install_test_script(&path, script);
    }
    let agent_args = fixture.root.path().join("agent-args");
    for choice in ["cancel", "No agent", "codex", "none"] {
        fs::write(fixture.root.path().join("choice"), choice).unwrap();
        let (output, transcript) = fixture.interactive(
            &[
                "add",
                "https://github.com/team/project/issues/298",
                "--base",
                "HEAD",
            ],
            "",
        );
        if choice == "cancel" {
            assert_eq!(output.status.code(), Some(130), "{output:?}");
            assert!(!transcript.contains("canceled"), "{transcript}");
            assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
            continue;
        }
        assert!(output.status.success(), "{output:?}\n{transcript}");
        let inspection = fixture.ok(&["inspect", "298-pick-agent"]);
        assert_eq!(inspection["workspace"]["state"], "ready");
        assert_eq!(inspection["executions"], serde_json::json!([]));
        if choice == "No agent" {
            assert!(!agent_args.exists());
            // The shared review picker also accepts the choice without a launch.
            let (output, transcript) = fixture.interactive(&["review", "298-pick-agent"], "");
            assert!(output.status.success(), "{output:?}\n{transcript}");
            assert!(!agent_args.exists());
        } else {
            let prompt = fs::read_to_string(&agent_args).unwrap();
            assert!(prompt.contains("Issue context"), "{prompt}");
            fs::remove_file(&agent_args).unwrap();
        }
        fixture.ok(&["rm", "298-pick-agent", "--yes"]);
    }

    // A regular add uses the same picker when no agent was named or configured.
    fs::write(fixture.root.path().join("choice"), "codex").unwrap();
    let (output, transcript) = fixture.interactive(
        &[
            "add",
            fixture.repo.to_str().unwrap(),
            "plain",
            "--base",
            "HEAD",
        ],
        "",
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert!(agent_args.exists());
}

#[test]
fn issue_number_picks_a_repository_before_lookup_interactively() {
    let fixture = Fixture::with_config(Some("default_agent = 'claude'\n"));
    fixture.add_github_origin();
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    install_fake_agents(&fixture, &["claude"]);
    for (tool, script) in [
        ("fzf", "#!/bin/sh\nhead -n 1\n"),
        (
            "gh",
            "#!/bin/sh\nprintf 'lookup with %s' \"$*\" >&2\nexit 1\n",
        ),
    ] {
        let path = bin.join(tool);
        install_test_script(&path, script);
    }
    let (output, transcript) = fixture.interactive(&["add", "103", "--base", "HEAD"], "");
    assert!(!output.status.success(), "{output:?}\n{transcript}");
    assert!(
        transcript.contains("--repo github.com/team/project"),
        "{transcript}"
    );
    assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
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
    // Agents are checked before the issue lookup these cases exercise.
    install_test_script_with_mode(bin.join("codex"), "#!/bin/sh\n", 0o700);
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
            install_test_script_with_mode(&tool, script, 0o700);
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
        assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
    }
    // A found issue still needs an agent before anything is created.
    install_test_script_with_mode(
        &tool,
        "#!/bin/sh\necho '{\"state\":\"OPEN\",\"number\":4,\"title\":\"Four\",\"body\":\"\"}'\n",
        0o700,
    );
    for (args, diagnostic) in [
        (
            vec!["add", "--issue", "https://github.com/team/other/issues/4"],
            "shoal repo add",
        ),
        (vec!["add", "--issue", "4"], "missing argument"),
        (
            vec!["add", "https://github.com/team/project/issues/4"],
            "no agent selected",
        ),
        (vec!["add", "4", "--agent", "codex"], "pass --repo"),
        (
            vec![
                "add",
                fixture.repo.to_str().unwrap(),
                "--issue",
                "not-an-issue",
                "--agent",
                "codex",
            ],
            "a positive number or an issue URL",
        ),
        (
            vec![
                "add",
                "0",
                "--repo",
                fixture.repo.to_str().unwrap(),
                "--agent",
                "codex",
            ],
            "must be positive",
        ),
        (
            vec!["add", "4", "--repo", "unknown", "--agent", "codex"],
            "not registered",
        ),
        (
            vec![
                "add",
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
                "add",
                "https://github.com/team/other/issues/4",
                "--agent",
                "codex",
            ],
            "shoal repo add",
        ),
        (
            vec![
                "add",
                fixture.repo.to_str().unwrap(),
                "--issue",
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
        assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
    }
}

#[test]
fn issues_reopen_their_branch_and_report_conflicts_before_picking_an_agent() {
    let fixture = Fixture::with_config(Some("[commands]\npi = ['sh', '-c', 'exit 0']\n"));
    fixture.add_github_origin();
    install_test_script(
        fixture.root.path().join("bin/gh"),
        "#!/bin/sh\necho '{\"state\":\"OPEN\",\"number\":34,\"title\":\"Fix API timeout\",\"body\":\"\"}'\n",
    );
    let repo = fixture.repo.to_str().unwrap();
    let issue = ["add", "34", "--repo", repo, "--agent", "pi"];
    let branch = "34-fix-api-timeout";
    let branches = |fixture: &Fixture| -> Vec<Value> {
        fixture
            .ok(&["work"])
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["branch"].clone())
            .collect()
    };
    let created = fixture.ok(&[&issue[..], &["--base", "HEAD"]].concat());
    let reopened = fixture.ok(&issue);
    assert_eq!(reopened["id"], created["id"]);
    assert_eq!(branches(&fixture), [branch]);

    // A commit of its own keeps the branch when the workspace goes.
    let path = Path::new(created["path"].as_str().unwrap());
    commit_file(path, "work", "work\n");
    fixture.ok(&["rm", branch, "--yes"]);
    let elsewhere = fixture.root.path().join("elsewhere");
    git(
        &fixture.repo,
        &["worktree", "add", "-q", elsewhere.to_str().unwrap(), branch],
    );
    // Without --agent the picker would fail next; the conflict is reported first.
    let output = fixture.run(&issue[..4]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("already checked out"),
        "{output:?}"
    );
    assert!(branches(&fixture).is_empty());

    git(
        &fixture.repo,
        &["worktree", "remove", elsewhere.to_str().unwrap()],
    );
    fixture.ok(&issue);
    assert_eq!(branches(&fixture), [branch]);
}

#[test]
fn add_existing_branch_runs_setup_once_and_denies_scoped_creation() {
    let fixture = Fixture::new();
    for hook in ["setup", "post"] {
        let path = fixture.repo.join(hook);
        install_test_script(&path, format!("#!/bin/sh\necho {hook} >> runs\n"));
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
    let picker = bin.join("fzf");
    install_test_script(
        &picker,
        "#!/bin/sh\nawk -F '\\t' '$2 == \"Use an existing branch\" || $1 == \"refs/heads/coworker/topic\" {print}'\n",
    );
    let directive = fixture.root.path().join("destination");
    for _ in 0..2 {
        let (output, transcript) = fixture.interactive_command(
            fixture
                .command()
                .env("SHOAL_SHELL_DIRECTIVE", &directive)
                .args(["add", fixture.repo.to_str().unwrap()]),
            "",
        );
        assert!(output.status.success(), "{transcript}");
        let workspace = fixture.ok(&["inspect", "coworker-topic"]);
        assert_eq!(
            fs::read_to_string(&directive).unwrap().trim(),
            workspace["workspace"]["path"].as_str().unwrap()
        );
    }
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
}

#[test]
fn interactive_add_picks_an_open_issue_or_pull_request() {
    let fixture = Fixture::new();
    fixture.add("topic");
    fixture.add_github_origin();
    // An existing branch avoids refreshing the default branch from GitHub.
    git(&fixture.repo, &["branch", "34-fix-api-timeout"]);
    install_test_script(
        fixture.root.path().join("bin/gh"),
        r#"#!/bin/sh
case "$1 $2" in
 'issue list') echo '[{"number":34,"title":"Fix API timeout","state":"OPEN","labels":[]},{"number":35,"title":"Other","state":"OPEN","labels":[]}]';;
 'issue view') echo '{"state":"OPEN","number":34,"title":"Fix API timeout","body":""}';;
 'pr list') echo '[{"number":7,"title":"Topic","state":"OPEN","labels":[],"headRefName":"topic","baseRefName":"main","isDraft":false}]';;
 'pr view') echo '{"number":7,"title":"Topic","headRefName":"topic","baseRefName":"main","isCrossRepository":false}';;
 *) exit 1;;
esac
"#,
    );
    let picker = fixture.root.path().join("bin/fzf");
    let directive = fixture.root.path().join("destination");
    let add = |choice: &str, item: &str| {
        install_test_script(
            &picker,
            format!("#!/bin/sh\nawk -F '\\t' '$2 == \"{choice}\" || $2 ~ /^#{item} / {{print}}'\n"),
        );
        fixture.interactive_command(
            fixture
                .command()
                .env("SHOAL_SHELL_DIRECTIVE", &directive)
                .args(["add", fixture.repo.to_str().unwrap()]),
            "",
        )
    };
    let (output, transcript) = add("Use an issue", "34");
    assert!(output.status.success(), "{output:?} {transcript}");
    let issue = fixture.ok(&["inspect", "34-fix-api-timeout"]);
    assert_eq!(
        issue["issue"]["url"],
        "https://github.com/team/project/issues/34"
    );
    // The PR's head branch belongs to a workspace, which reopens.
    let (output, transcript) = add("Use a pull request", "7");
    assert!(output.status.success(), "{output:?} {transcript}");
    assert_eq!(
        fs::read_to_string(&directive).unwrap().trim(),
        fixture.ok(&["inspect", "topic"])["workspace"]["path"]
            .as_str()
            .unwrap()
    );
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 2);

    install_test_script(fixture.root.path().join("bin/gh"), "#!/bin/sh\necho '[]'\n");
    let (output, transcript) = add("Use a pull request", "7");
    assert!(!output.status.success());
    assert!(transcript.contains("no open pull requests"), "{transcript}");
}

#[test]
fn link_picks_an_omitted_open_issue_or_pull_request() {
    let fixture = Fixture::new();
    fixture.add("topic");
    fixture.add_github_origin();
    install_test_script(
        fixture.root.path().join("bin/gh"),
        r#"#!/bin/sh
case "$1 $2" in
 'issue list') echo '[{"number":34,"title":"Fix API timeout","state":"OPEN","labels":[]}]';;
 'pr list') echo '[{"number":7,"title":"Topic","state":"OPEN","labels":[],"headRefName":"topic","baseRefName":"main","isDraft":false}]';;
 'issue view') echo '{"number":34,"state":"OPEN","title":"Fix API timeout","body":"","comments":[]}';;
 'pr view') echo '{"number":7,"state":"OPEN","headRefName":"topic","headRefOid":"head","title":"Topic","baseRefName":"main","isCrossRepository":false,"commits":[],"comments":[],"reviews":[],"statusCheckRollup":[],"mergeable":"MERGEABLE"}';;
 *) printf '[]';;
esac
"#,
    );
    let missing = fixture.run(&["link", "assigned", "--workspace", "topic"]);
    assert!(String::from_utf8_lossy(&missing.stderr).contains("link needs an issue or PR"));
    let picker = fixture.root.path().join("bin/fzf");
    for (args, kind, number) in [
        (
            vec!["link", "assigned", "--workspace", "topic"],
            "Pull request",
            "7",
        ),
        (
            vec!["link", "issue", "assigned", "--workspace", "topic"],
            "",
            "34",
        ),
    ] {
        install_test_script(
            &picker,
            format!("#!/bin/sh\nawk -F '\\t' '$2 == \"{kind}\" || $1 == \"{number}\" {{print}}'\n"),
        );
        let (output, transcript) = fixture.interactive_command(fixture.command().args(&args), "");
        assert!(output.status.success(), "{output:?} {transcript}");
    }
    let workspace = fixture.ok(&["inspect", "topic"]);
    assert_eq!(
        workspace["workspace"]["links"]["prs"],
        serde_json::json!(["https://github.com/team/project/pull/7"])
    );
    assert_eq!(
        workspace["workspace"]["links"]["issue"],
        "https://github.com/team/project/issues/34"
    );
    assert_eq!(
        workspace["workspace"]["links"]["issue_title"],
        "Fix API timeout"
    );
}

/// A `curl` for `bin` that answers Forgejo API requests from files that
/// [`forgejo_api`] writes, logging each URL to `$HOME/curl-urls`.
fn install_forgejo_curl(bin: &Path) {
    install_test_script(
        bin.join("curl"),
        r#"#!/bin/sh
cat >/dev/null
for arg; do url=$arg; done
printf '%s\n' "$url" >> "$HOME/curl-urls"
key=$(printf %s "${url#*/api/v1/}" | tr '/?&=' '____')
if [ -f "$HOME/api/$key" ]; then
  cat "$HOME/api/$key"
  printf '\n200'
else
  printf '{"message":"no fixture for %s"}\n404' "$key"
fi
"#,
    );
}

/// Answer `GET /api/v1/<endpoint>` with `body` through the fake curl.
fn forgejo_api(home: &Path, endpoint: &str, body: &Value) {
    let key: String = endpoint
        .chars()
        .map(|c| if "/?&=".contains(c) { '_' } else { c })
        .collect();
    fs::create_dir_all(home.join("api")).unwrap();
    fs::write(home.join("api").join(key), body.to_string()).unwrap();
}

/// A Forgejo PR of `team/repo` from its `head` branch at commit `sha`.
fn forgejo_pull(number: u64, state: &str, head: &str, sha: &str) -> Value {
    let merged = state == "merged";
    serde_json::json!({"number": number, "title": format!("PR {number}"),
        "state": if merged { "closed" } else { state }, "merged": merged, "body": "", "labels": [],
        "head": {"ref": head, "sha": sha, "repo": {"full_name": "team/repo"}},
        "base": {"ref": "main"}, "mergeable": true})
}

fn wait_removed(fixture: &Fixture, name: &str) {
    try_wait_for(|| {
        (!fixture
            .ok(&["work"])
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["name"] == name))
        .then_some(())
    })
    .unwrap_or_else(|| {
        panic!(
            "PR cleanup did not remove workspace: {}",
            fixture.ok(&["inspect", name])
        )
    });
}

fn wait_pr_error(fixture: &Fixture, name: &str, message: &str) {
    let mut inspection = Value::Null;
    try_wait_for(|| {
        inspection = fixture.ok(&["inspect", name]);
        inspection["pr_cleanup"]["error"]
            .as_str()
            .is_some_and(|s| s.contains(message))
            .then_some(())
    })
    .unwrap_or_else(|| panic!("expected {message}: {inspection}"));
}

#[test]
fn merged_stops_agent_and_releases_resources_without_idle_delay() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n[resources.device]\n"));
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
    assert_eq!(fixture.acknowledge("merged")["type"], "ok");
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
    assert_eq!(fixture.acknowledge("retain")["type"], "ok");
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
    fixture.ok(&["unlink", "pr", "assigned", "--workspace", "retain"]);
    assert!(fixture.ok(&["inspect", "retain"])["pr_cleanup"].is_null());
}

#[test]
fn pr_cleanup_can_be_disabled_independently() {
    let fixture = Fixture::with_config(Some("[cleanup.pr]\nenabled=false\n"));
    fixture.add("keep");
    // Linking does not depend on PR cleanup.
    fixture.add_github_origin();
    install_test_script(
        fixture.root.path().join("bin/gh"),
        "#!/bin/sh\necho '{\"number\":7,\"state\":\"OPEN\",\"headRefName\":\"keep\",\"commits\":[]}'\n",
    );
    fixture.ok(&["link", "pr", "7", "assigned", "--workspace", "keep"]);
    assert_eq!(
        fixture.ok(&["inspect", "keep"])["workspace"]["links"]["prs"],
        serde_json::json!(["https://github.com/team/project/pull/7"])
    );
    fixture.ok(&["unlink", "pr", "assigned", "--workspace", "keep"]);
    let output = fixture.acknowledge("keep");
    assert_eq!(output["type"], "error");
    assert!(
        output["data"]["message"]
            .as_str()
            .unwrap()
            .contains("PR cleanup is disabled")
    );
    assert!(fixture.ok(&["inspect", "keep"])["pr_cleanup"].is_null());
}

#[test]
fn legacy_pr_registrations_survive_disabled_cleanup_and_clear_after_restart() {
    let mut fixture = Fixture::with_config(Some(
        "[cleanup.pr]\nenabled=false\n[cleanup.auto]\nenabled=false\n",
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
        assert_eq!(fixture.acknowledge(name)["type"], "error");
        assert_eq!(fixture.ok(&["inspect", name])["pr_cleanup"], record);
        // Clear remains allowed while disabled, including for a scoped caller.
        assert_eq!(
            fixture.ok(&[
                "exec",
                name,
                "--",
                env!("CARGO_BIN_EXE_shoal"),
                "--json",
                "unlink",
                "pr",
                "assigned",
            ]),
            serde_json::json!({"registered": false})
        );
        fixture.restart();
        assert!(fixture.ok(&["inspect", name])["pr_cleanup"].is_null());
        assert_eq!(
            fixture.ok(&["unlink", "pr", "assigned", "--workspace", name]),
            serde_json::json!({"registered": false})
        );
        assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    }
}

#[test]
fn invalid_persisted_pr_registration_retains_workspace_and_can_be_cleared() {
    let mut fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
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
        fixture.ok(&["unlink", "pr", "assigned", "--workspace", "invalid"]);
        assert!(fixture.ok(&["inspect", "invalid"])["pr_cleanup"].is_null());
    }
}

#[test]
fn repository_config_sets_pr_cleanup_over_the_global_default() {
    let fixture = Fixture::with_config(Some("[cleanup.pr]\nenabled=false\n"));
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "[cleanup.pr]\nenabled=true\n",
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
    assert_eq!(fixture.acknowledge("merged")["type"], "ok");
    wait_removed(&fixture, "merged");
    // The saved config is the top layer.
    let saved = fixture.root.path().join("saved.toml");
    fs::write(&saved, "[cleanup.pr]\nenabled=false\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    fixture.add("kept");
    let output = fixture.acknowledge("kept");
    assert_eq!(output["type"], "error");
    assert!(
        output["data"]["message"]
            .as_str()
            .unwrap()
            .contains("PR cleanup is disabled")
    );
}

#[test]
fn pr_watch_checks_github_state_and_commit_and_survives_restart() {
    let mut fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    let workspace = fixture.add("watch");
    let failed = fixture.run(&["link", "pr", "56", "assigned", "--workspace", "watch"]);
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
    install_test_script(bin.join("gh"), "#!/bin/sh\ncat \"$HOME/pr.json\"\n");
    let failed = fixture.run(&[
        "link",
        "pr",
        "https://github.com/team/repo/pull/56",
        "assigned",
        "--workspace",
        "watch",
    ]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("shoal done"));
    assert!(fixture.ok(&["inspect", "watch"])["pr_cleanup"].is_null());
    assert!(
        !fixture
            .run(&[
                "link",
                "pr",
                "https://github.com/other/repo/pull/56",
                "assigned",
                "--workspace",
                "watch"
            ])
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
    fixture.ok(&[
        "link",
        "pr",
        "https://github.com/team/repo/pull/56",
        "assigned",
        "--workspace",
        "watch",
    ]);
    fixture.ok(&["unlink", "pr", "assigned", "--workspace", "watch"]);
    let events = fixture
        .command()
        .args(["--json", "internal", "events"])
        .output()
        .unwrap();
    assert!(events.status.success());
    let events: Vec<Value> = String::from_utf8(events.stdout)
        .unwrap()
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .unwrap();
    for kind in ["linked", "unlinked"] {
        let event = events
            .iter()
            .find(|event| event["kind"] == kind)
            .unwrap_or_else(|| panic!("missing {kind} event"));
        assert_eq!(event["link"]["kind"], "pr");
        assert_eq!(event["link"]["relation"], "assigned");
        assert_eq!(event["link"]["url"], "https://github.com/team/repo/pull/56");
    }
    fixture.ok(&["link", "pr", "56", "assigned", "--workspace", "watch"]);
    fixture.ok(&["unlink", "pr", "assigned", "--workspace", "watch"]);
    // Scoped callers can omit the workspace and register by number too.
    fixture.ok(&[
        "exec",
        "watch",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "--json",
        "link",
        "pr",
        "56",
        "assigned",
    ]);
    fixture.restart();
    assert_eq!(
        fixture.ok(&["inspect", "watch"])["pr_cleanup"]["url"],
        "https://github.com/team/repo/pull/56"
    );
    // Watched PRs are looked up once the assignment is done.
    fixture.ok(&["done", "watch"]);
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
    // Checking the default branch for HEAD's changes must not reach GitHub.
    git(&fixture.repo, &["config", "protocol.https.allow", "never"]);
    fixture.restart();
    wait_pr_error(&fixture, "watch", "do not contain HEAD");
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
fn pr_watch_checks_forgejo_merge_and_commits_through_the_api() {
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
    let home = fixture.root.path();
    install_forgejo_curl(&home.join("bin"));
    let head = git(
        Path::new(workspace["path"].as_str().unwrap()),
        &["rev-parse", "HEAD"],
    );
    let head = head.trim();
    forgejo_api(
        home,
        "repos/team/repo/pulls/56",
        &forgejo_pull(56, "merged", "fj-watch", head),
    );
    forgejo_api(
        home,
        "repos/team/repo/pulls/56/commits?limit=50&page=1",
        &serde_json::json!([{"sha": head}]),
    );
    fixture.ok(&["link", "pr", "56", "assigned", "--workspace", "fj-watch"]);
    // A merge alone waits for the agent's explicit done.
    assert!(fixture.ok(&["inspect", "fj-watch"])["completion"].is_null());
    fixture.ok(&["done", "fj-watch"]);
    wait_removed(&fixture, "fj-watch");
}

#[test]
fn pr_wait_wakes_for_individual_ci_reviews_and_conflicts_with_own_scope() {
    let mut fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    let workspace = fixture.add("watch");
    fixture.add("other");
    fixture.add_github_origin();
    let bin = fixture.root.path().join("bin");
    install_test_script(
        bin.join("gh"),
        "#!/bin/sh\nif [ \"$1\" = api ]; then cat \"$HOME/inline.json\"; else cat \"$HOME/pr.json\"; fi\n",
    );
    let mut pull = serde_json::json!({"number":7,"state":"OPEN","headRefName":"watch","headRefOid":"head",
    "commits":[],"comments":[],"reviews":[],"mergeable":"MERGEABLE",
    "statusCheckRollup":[
        {"__typename":"CheckRun","name":"rust","status":"IN_PROGRESS","conclusion":"","detailsUrl":"rust/1"},
        {"__typename":"CheckRun","name":"review","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"review/1"}
    ]});
    let response = fixture.root.path().join("pr.json");
    fs::write(&response, pull.to_string()).unwrap();
    fs::write(
        fixture.root.path().join("inline.json"),
        "[[{\"id\":1,\"body\":\"review finding\"}]]",
    )
    .unwrap();
    fixture.ok(&["link", "pr", "7", "assigned", "--workspace", "watch"]);
    let binary = env!("CARGO_BIN_EXE_shoal");
    let first = fixture.ok(&["exec", "watch", "--", binary, "--json", "watch", "pr"]);
    assert_eq!(first["timed_out"], false);
    let kinds: Vec<_> = first["updates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|update| update["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["comment", "ci_completed"]);
    assert_eq!(
        first["updates"][0]["message"],
        "Comments or reviews changed; read them with shoal view https://github.com/team/project/pull/7"
    );
    assert_eq!(first["updates"][1]["message"], "review: SUCCESS");
    assert_eq!(
        first["updates"][0]["url"],
        "https://github.com/team/project/pull/7"
    );
    let denied = fixture.run(&[
        "exec",
        "watch",
        "--",
        binary,
        "watch",
        "pr",
        "--workspace",
        "other",
    ]);
    assert!(!denied.status.success());
    assert!(
        String::from_utf8_lossy(&denied.stderr)
            .contains("their own workspace and those split from it")
    );
    fixture.restart();
    // Completion being disabled does not disable activity waits.
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join(".shoal.toml"), "[cleanup.pr]\nenabled=false\n").unwrap();
    pull["statusCheckRollup"][0]["status"] = serde_json::json!("COMPLETED");
    pull["statusCheckRollup"][0]["conclusion"] = serde_json::json!("FAILURE");
    pull["mergeable"] = serde_json::json!("CONFLICTING");
    fs::write(&response, pull.to_string()).unwrap();
    let result = fixture
        .command()
        .current_dir(path)
        .args(["--json", "watch", "pr"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let second: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(second["updates"].as_array().unwrap().len(), 1);
    // This gh answers no log lookup; tests/forge.rs covers what it adds.
    let message = second["updates"][0]["message"].as_str().unwrap();
    assert!(message.starts_with("rust: FAILURE; "), "{message}");
    // A conflict is reported once a second poll sees it at the same commits.
    let third = fixture.ok(&["watch", "pr", "--workspace", "watch"]);
    assert_eq!(third["updates"].as_array().unwrap().len(), 1, "{third}");
    assert_eq!(third["updates"][0]["kind"], "merge_conflict");
    assert!(fixture.ok(&["inspect", "watch"])["completion"].is_null());
    fixture.ok(&["unlink", "pr", "assigned", "--workspace", "watch"]);
    let missing = fixture.run(&["watch", "pr", "--workspace", "watch"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no linked items"));
}

#[test]
fn a_newer_pr_wait_supersedes_the_older_one_without_taking_its_updates() {
    let fixture = Fixture::with_config(Some("[auto_cleanup]\nenabled=false\n"));
    fixture.add("waits");
    fixture.add_github_origin();
    let bin = fixture.root.path().join("bin");
    // Lookups after linking block until released, so the first wait is still polling when
    // the second starts.
    install_test_script(
        bin.join("gh"),
        r#"#!/bin/sh
if [ -f "$HOME/block" ]; then
  touch "$HOME/polled"
  while test ! -f "$HOME/release"; do sleep 0.02; done
fi
if [ "$1" = api ]; then printf '[]'; else cat "$HOME/pr.json"; fi
"#,
    );
    fs::write(fixture.root.path().join("pr.json"), serde_json::json!({"number":7,"state":"OPEN","headRefName":"waits","headRefOid":"head",
        "commits":[],"comments":[{"id":1,"body":"review"}],"reviews":[],"statusCheckRollup":[],"mergeable":"MERGEABLE"}).to_string()).unwrap();
    fixture.ok(&["link", "pr", "7", "assigned", "--workspace", "waits"]);
    fs::write(fixture.root.path().join("block"), "").unwrap();
    let wait = || {
        fixture
            .command()
            .args(["--json", "watch", "pr", "--workspace", "waits"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let mut older = wait();
    let polled = fixture.root.path().join("polled");
    wait_until("the first wait's lookup", || polled.exists());
    let newer = wait();
    wait_until("the superseded wait", || {
        older.try_wait().unwrap().is_some()
    });
    let output = older.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let superseded: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(superseded["superseded"], true);
    assert!(superseded["updates"].as_array().unwrap().is_empty());
    fs::write(fixture.root.path().join("release"), "").unwrap();
    let output = newer.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let received: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(received["superseded"], false);
    assert_eq!(received["updates"][0]["kind"], "comment");
}

#[test]
fn unified_items_link_filter_and_watch_explicit_targets_without_completion() {
    let mut fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("items");
    fixture.add("other");
    fixture.add_github_origin();
    let bin = fixture.root.path().join("bin");
    install_test_script(
        bin.join("gh"),
        r#"#!/bin/sh
case "$1" in
 api) printf '[]';;
 issue) cat "$HOME/issue.json";;
 *) cat "$HOME/pr.json";;
esac
"#,
    );
    let issue_file = fixture.root.path().join("issue.json");
    let write_issue = |number, body: &str| {
        fs::write(
            &issue_file,
            serde_json::json!({"number":number,"state":"OPEN","comments":[{"id":1,"body":body}]})
                .to_string(),
        )
        .unwrap()
    };
    fs::write(fixture.root.path().join("pr.json"), serde_json::json!({"number":7,"state":"OPEN","headRefName":"items","headRefOid":"head","title":"PR","baseRefName":"main","isCrossRepository":false,
        "commits":[],"comments":[{"id":2,"body":"PR comment"}],"reviews":[],"statusCheckRollup":[],"mergeable":"MERGEABLE"}).to_string()).unwrap();
    write_issue(1, "first");
    fixture.ok(&["link", "pr", "7", "assigned", "--workspace", "items"]);
    fixture.ok(&[
        "link",
        "https://github.com/team/project/issues/1",
        "assigned",
        "--workspace",
        "items",
    ]);
    let binary = env!("CARGO_BIN_EXE_shoal");
    let filtered = fixture.ok(&["exec", "items", "--", binary, "--json", "watch", "pr"]);
    assert_eq!(filtered["updates"].as_array().unwrap().len(), 1);
    assert_eq!(
        filtered["updates"][0]["url"],
        "https://github.com/team/project/pull/7"
    );
    let issue = fixture.ok(&["watch", "issue", "--workspace", "items"]);
    assert_eq!(
        issue["updates"][0]["url"],
        "https://github.com/team/project/issues/1"
    );
    write_issue(1, "changed");
    let all = fixture.ok(&["watch", "--workspace", "items"]);
    assert_eq!(all["updates"].as_array().unwrap().len(), 1);
    fixture.restart();
    write_issue(1, "changed after restart");
    let direct = fixture.ok(&[
        "watch",
        "https://github.com/team/project/issues/1",
        "--workspace",
        "items",
    ]);
    assert_eq!(direct["updates"].as_array().unwrap().len(), 1);
    assert!(
        !fixture
            .run(&["unlink", "issue", "2", "assigned", "--workspace", "items"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&[
                "exec",
                "items",
                "--",
                binary,
                "watch",
                "--workspace",
                "other"
            ])
            .status
            .success()
    );
    fixture.ok(&[
        "exec", "items", "--", binary, "--json", "unlink", "issue", "1", "assigned",
    ]);
    let inspection = fixture.ok(&["inspect", "items"]);
    assert!(inspection["issue"].is_null());
    assert_eq!(
        inspection["pr_cleanup"]["url"],
        "https://github.com/team/project/pull/7"
    );
    write_issue(2, "unlinked issue");
    let explicit = fixture.ok(&["watch", "issue", "2", "--workspace", "items"]);
    assert_eq!(
        explicit["updates"][0]["url"],
        "https://github.com/team/project/issues/2"
    );
    let inspection = fixture.ok(&["inspect", "items"]);
    assert!(inspection["issue"].is_null() && inspection["completion"].is_null());
    write_issue(1, "relink");
    fixture.ok(&["link", "issue", "1", "assigned", "--workspace", "items"]);
    fixture.ok(&["unlink", "assigned", "--workspace", "items"]);
    assert!(fixture.ok(&["inspect", "items"])["issue"].is_null());
    assert!(fixture.ok(&["inspect", "items"])["pr_cleanup"].is_null());
    // With nothing linked, a watch waits for branch conflicts alone.
    let items = PathBuf::from(
        fixture.ok(&["inspect", "items"])["workspace"]["path"]
            .as_str()
            .unwrap(),
    );
    commit_file(&items, "conflict", "workspace\n");
    commit_file(&fixture.repo, "conflict", "main\n");
    let branch = fixture.ok(&["watch", "--workspace", "items"]);
    assert_eq!(branch["updates"][0]["kind"], "merge_conflict");
    assert_eq!(branch["updates"][0]["url"], "items");
    assert_eq!(
        branch["updates"][0]["message"],
        "Conflicts with main: conflict"
    );
    let pr_file = fixture.root.path().join("pr.json");
    let mut fork: Value = serde_json::from_str(&fs::read_to_string(&pr_file).unwrap()).unwrap();
    fork["isCrossRepository"] = serde_json::json!(true);
    fork["headRefName"] = serde_json::json!("fork-topic");
    fs::write(pr_file, fork.to_string()).unwrap();
    let explicit = fixture.ok(&["watch", "pr", "7", "--workspace", "items"]);
    assert_eq!(explicit["updates"].as_array().unwrap().len(), 1);
    assert!(fixture.ok(&["inspect", "items"])["pr_cleanup"].is_null());
    assert!(
        !fixture
            .run(&[
                "watch",
                "https://github.com/other/project/issues/2",
                "--workspace",
                "items"
            ])
            .status
            .success()
    );
}

#[test]
fn watch_explicit_forgejo_fork_pr_without_linking_or_checkout() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("items");
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://forge.example/team/repo.git",
        ],
    );
    let home = fixture.root.path();
    install_forgejo_curl(&home.join("bin"));
    let sha = "a".repeat(40);
    let mut pull = forgejo_pull(7, "open", "fork-topic", &sha);
    pull["head"]["repo"]["full_name"] = "other/repo".into();
    forgejo_api(home, "repos/team/repo/pulls/7", &pull);
    forgejo_api(
        home,
        "repos/team/repo/issues/7/comments?limit=50&page=1",
        &serde_json::json!([{"id": 1, "body": "please test forks"}]),
    );
    forgejo_api(
        home,
        "repos/team/repo/pulls/7/reviews?limit=50&page=1",
        &serde_json::json!([]),
    );
    forgejo_api(
        home,
        &format!("repos/team/repo/commits/{sha}/status?limit=50"),
        &serde_json::json!({"total_count": 1, "statuses": [{"context": "rust", "status": "success"}]}),
    );
    let watched = fixture.ok(&["watch", "pr", "7", "--workspace", "items"]);
    let updates = watched["updates"].as_array().unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0]["kind"], "comment");
    assert_eq!(updates[1]["message"], "rust: Success");
    assert_eq!(updates[0]["url"], "https://forge.example/team/repo/pulls/7");
    let inspection = fixture.ok(&["inspect", "items"]);
    assert!(inspection["pr_cleanup"].is_null());
    assert!(inspection["completion"].is_null());
    assert_eq!(inspection["workspace"]["branch"], "items");
}

#[test]
fn watch_forgejo_issue_reports_comments_closure_and_reopening() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("issue-watch");
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://forge.example/team/repo.git",
        ],
    );
    // The unfiltered watch checks branch conflicts against the default branch.
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let home = fixture.root.path();
    install_forgejo_curl(&home.join("bin"));
    let state = |state: &str| {
        forgejo_api(
            home,
            "repos/team/repo/issues/8",
            &serde_json::json!({"number": 8, "title": "Issue", "state": state, "body": "",
                "labels": [], "pull_request": null}),
        )
    };
    state("open");
    forgejo_api(
        home,
        "repos/team/repo/issues/8/comments?limit=50&page=1",
        &serde_json::json!([{"id": 1, "body": "first comment"}]),
    );
    fixture.ok(&[
        "link",
        "issue",
        "8",
        "assigned",
        "--workspace",
        "issue-watch",
    ]);
    let first = fixture.ok(&["watch", "issue", "--workspace", "issue-watch"]);
    assert_eq!(first["updates"][0]["kind"], "comment");
    state("closed");
    let closed = fixture.ok(&["watch", "--workspace", "issue-watch"]);
    assert_eq!(closed["updates"].as_array().unwrap().len(), 1);
    assert_eq!(closed["updates"][0]["kind"], "closed");
    state("open");
    let reopened = fixture.ok(&["watch", "issue", "8", "--workspace", "issue-watch"]);
    assert_eq!(reopened["updates"][0]["kind"], "reopened");
    assert!(fixture.ok(&["inspect", "issue-watch"])["completion"].is_null());
}

#[test]
fn pr_wait_reads_forgejo_ci_and_conflicts_from_the_api() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("api");
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://forge.example/team/repo.git",
        ],
    );
    let home = fixture.root.path();
    install_forgejo_curl(&home.join("bin"));
    let sha = "2".repeat(40);
    let mut pull = forgejo_pull(7, "open", "api", &sha);
    pull["mergeable"] = false.into();
    forgejo_api(home, "repos/team/repo/pulls/7", &pull);
    forgejo_api(
        home,
        "repos/team/repo/issues/7/comments?limit=50&page=1",
        &serde_json::json!([]),
    );
    forgejo_api(
        home,
        "repos/team/repo/pulls/7/reviews?limit=50&page=1",
        &serde_json::json!([]),
    );
    forgejo_api(
        home,
        &format!("repos/team/repo/commits/{sha}/status?limit=50"),
        &serde_json::json!({"total_count": 2, "statuses": [
            {"context": "rust", "status": "failure"}, {"context": "gate", "status": "pending"}
        ]}),
    );
    fixture.ok(&["link", "pr", "7", "assigned", "--workspace", "api"]);
    let watched = fixture.ok(&["watch", "pr", "--workspace", "api"]);
    let updates = watched["updates"].as_array().unwrap();
    assert_eq!(
        updates
            .iter()
            .map(|update| update["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ci_completed"],
        "{watched}"
    );
    assert!(
        updates[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("rust: Failure; log unavailable:")
    );
    // Forgejo also shows a PR it is still checking as not mergeable, so the
    // conflict waits for a second poll.
    let confirmed = fixture.ok(&["watch", "pr", "--workspace", "api"]);
    assert_eq!(
        confirmed["updates"].as_array().unwrap().len(),
        1,
        "{confirmed}"
    );
    assert_eq!(confirmed["updates"][0]["message"], "PR has merge conflicts");
    assert!(
        fs::read_to_string(home.join("curl-urls"))
            .unwrap()
            .starts_with("https://forge.example/api/v1/repos/team/repo/pulls/7\n")
    );
}

#[test]
fn pr_wait_reports_forgejo_reviews_when_the_ci_lookup_fails() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("watch");
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://forge.example/team/repo.git",
        ],
    );
    let home = fixture.root.path();
    install_forgejo_curl(&home.join("bin"));
    let sha = "1".repeat(40);
    let status = format!("repos/team/repo/commits/{sha}/status?limit=50");
    let checks = |rust: &str| {
        serde_json::json!({"total_count": 2, "statuses": [
            {"context": "review/default", "status": "success"}, {"context": "rust", "status": rust}
        ]})
    };
    let review = |body: &str| serde_json::json!([{"id": 1, "state": "COMMENT", "body": body, "user": {"login": "review-bot"}}]);
    let reviews = "repos/team/repo/pulls/7/reviews?limit=50&page=1";
    forgejo_api(
        home,
        "repos/team/repo/pulls/7",
        &forgejo_pull(7, "open", "watch", &sha),
    );
    forgejo_api(
        home,
        "repos/team/repo/issues/7/comments?limit=50&page=1",
        &serde_json::json!([]),
    );
    forgejo_api(home, &status, &checks("pending"));
    forgejo_api(home, reviews, &review("A finding"));
    fixture.ok(&["link", "pr", "7", "assigned", "--workspace", "watch"]);
    let first = fixture.ok(&["watch", "pr", "--workspace", "watch"]);
    assert_eq!(first["updates"].as_array().unwrap().len(), 2, "{first}");
    assert_eq!(first["updates"][1]["message"], "review/default: Success");
    // A failed CI lookup reports itself without hiding a new review.
    fs::remove_file(
        home.join("api")
            .join(status.replace(['/', '?', '&', '='], "_")),
    )
    .unwrap();
    forgejo_api(home, reviews, &review("Another finding"));
    let second = fixture.ok(&["watch", "pr", "--workspace", "watch"]);
    assert_eq!(second["updates"][0]["kind"], "comment", "{second}");
    assert_eq!(second["updates"][1]["kind"], "lookup_failed");
    let mut conflicting = forgejo_pull(7, "open", "watch", &sha);
    conflicting["mergeable"] = false.into();
    forgejo_api(home, "repos/team/repo/pulls/7", &conflicting);
    forgejo_api(home, &status, &checks("failure"));
    let third = fixture.ok(&["watch", "pr", "--workspace", "watch"]);
    assert_eq!(third["updates"].as_array().unwrap().len(), 1, "{third}");
    assert!(
        third["updates"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("rust: Failure; log unavailable:")
    );
    let fourth = fixture.ok(&["watch", "pr", "--workspace", "watch"]);
    assert_eq!(fourth["updates"].as_array().unwrap().len(), 1, "{fourth}");
    assert_eq!(fourth["updates"][0]["kind"], "merge_conflict");
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
        install_test_script(
            fixture.repo.join("hook.sh"),
            "#!/bin/sh\ngit -c user.name=Test -c user.email=test@example.invalid commit --allow-empty -m 'hook work'\n",
        );
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
        assert_eq!(fixture.acknowledge("hook")["type"], "ok");
        wait_pr_error(&fixture, "hook", "HEAD changed during removal hooks");
        assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    }
}

#[test]
fn manual_removal_keeps_a_branch_its_hooks_committed_to() {
    for key in ["pre_remove_cmd", "pre_resource_release_cmd"] {
        let fixture = Fixture::new();
        fs::write(
            fixture.repo.join(".shoal.toml"),
            format!("{key} = 'hook.sh'\n[resources.signing]\n"),
        )
        .unwrap();
        install_test_script(
            fixture.repo.join("hook.sh"),
            concat!(
                "#!/bin/sh\n",
                "echo work > hook-work && git add hook-work\n",
                "git -c user.name=Test -c user.email=test@example.invalid ",
                "commit -m 'hook work'\n",
            ),
        );
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
        // The final inspection sees the hook's commit, which only the branch has.
        let removal = fixture.ok(&["rm", "hook", "--yes"]);
        assert_eq!(removal["branch_deleted"], false);
        assert!(!Path::new(workspace["path"].as_str().unwrap()).exists());
        let branch = workspace["branch"].as_str().unwrap();
        assert_eq!(
            git(&fixture.repo, &["log", "-1", "--format=%s", branch]).trim(),
            "hook work"
        );
    }
}

#[test]
fn land_merges_into_main_without_a_remote_and_needs_approval_in_scoped_processes() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    commit_file(path, "landed", "from the workspace\n");
    let expected = git(path, &["rev-parse", "HEAD"]);
    let before = git(&fixture.repo, &["rev-parse", "main"]);
    let binary = env!("CARGO_BIN_EXE_shoal");
    let pending = fixture.run(&["exec", "worker", "--", binary, "--json", "land"]);
    assert_eq!(pending.status.code(), Some(2));
    let pending: serde_json::Value = serde_json::from_slice(&pending.stdout).unwrap();
    assert_eq!(pending["code"], "approval_pending");
    assert_eq!(pending["request"]["target"], "land");
    assert_eq!(pending["request"]["specification"]["head"], expected.trim());
    let denied = fixture.run(&["exec", "worker", "--", binary, "internal", "land", "{}"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("only an authorized landing"));
    let repo = fixture.ok(&["repo", "list"])[0]["id"].clone();
    fixture.ok(&[
        "config",
        "set",
        "land.push",
        "true",
        "--repo",
        repo.as_str().unwrap(),
    ]);
    let unpushable = fixture.run(&["land", "worker"]);
    assert!(!unpushable.status.success());
    assert!(
        String::from_utf8_lossy(&unpushable.stderr).contains("main has no upstream to push to")
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
    fixture.ok(&["approve", &pending["request"]["id"].to_string()]);
    let landed = fixture.run(&[
        "exec",
        "worker",
        "--",
        binary,
        "--json",
        "land",
        "--no-push",
    ]);
    assert!(landed.status.success(), "{landed:?}");
    let result: serde_json::Value = serde_json::from_slice(&landed.stdout).unwrap();
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
    assert_eq!(
        fixture.ok(&["land", "--no-push", "worker"])["updated"],
        false
    );
    // Landed work is on the default branch, so its branch goes.
    assert_eq!(fixture.ok(&["rm", "worker"])["branch_deleted"], true);
}

#[test]
fn view_shows_linked_items_with_status_and_discussion() {
    let fixture = Fixture::with_config(Some("[auto_cleanup]\nenabled=false\n"));
    fixture.add("view");
    fixture.add_github_origin();
    let bin = fixture.root.path().join("bin");
    install_test_script(
        bin.join("gh"),
        "#!/bin/sh\ncase \"$1 $2\" in\n'issue view') [ \"$3\" = 3 ] && cat \"$HOME/issue.json\" || echo '{\"number\":3}';;\n'pr view') cat \"$HOME/pr.json\";;\n*) case \"$4 $6\" in */reviews\\ *) cat \"$HOME/reviews.json\";; '-X graphql') printf 'HTTP/2.0 200 OK\\n\\n'; cat \"$HOME/threads.json\";; *) cat \"$HOME/inline.json\";; esac;;\nesac\n",
    );
    let root = fixture.root.path();
    // Longer than a protocol frame: lookups run in the CLI.
    let body = "Steps ".repeat(20_000);
    fs::write(
        root.join("issue.json"),
        serde_json::json!({"number":3,"title":"Crash","body":body,"state":"OPEN",
            "author":{"login":"ann"},"labels":[{"name":"bug"}],"createdAt":"t0",
            "comments":[{"author":{"login":"bob"},"createdAt":"t1","body":"Same here"}]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        root.join("pr.json"),
        serde_json::json!({"number":7,"title":"Fix crash","body":"Closes #3","state":"OPEN",
            "author":{"login":"agent"},"labels":[],"createdAt":"t2","headRefName":"view",
            "baseRefName":"main","isDraft":false,"headRefOid":"head","commits":[],
            "comments":[],"reviews":[{"id":"r1","author":{"login":"bot"},"state":"CHANGES_REQUESTED"}],
            "mergeable":"CONFLICTING","statusCheckRollup":[{"__typename":"CheckRun","name":"rust",
            "status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"rust/1"}]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        root.join("reviews.json"),
        r#"[[{"id":11,"user":{"login":"bot"},"state":"CHANGES_REQUESTED","body":"One blocker","submitted_at":"t3"}]]"#,
    )
    .unwrap();
    fs::write(
        root.join("inline.json"),
        r#"[[{"id":21,"pull_request_review_id":11,"user":{"login":"bot"},"path":"src/a.rs","line":3,"body":"Handle None","created_at":"t3"}]]"#,
    )
    .unwrap();
    fs::write(
        root.join("threads.json"),
        serde_json::json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
            "pageInfo":{"hasNextPage":false,"endCursor":null},
            "nodes":[{"id":"T_1","isResolved":false,"comments":{
                "pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"databaseId":21}]}}]}}}}})
        .to_string(),
    )
    .unwrap();
    fixture.ok(&["link", "issue", "3", "assigned", "--workspace", "view"]);
    fixture.ok(&["link", "pr", "7", "assigned", "--workspace", "view"]);

    let views = fixture.ok(&["view", "--workspace", "view"]);
    let views = views.as_array().unwrap();
    assert_eq!(views.len(), 2);
    let issue = views.iter().find(|view| view["kind"] == "issue").unwrap();
    assert_eq!(issue["url"], "https://github.com/team/project/issues/3");
    assert_eq!(issue["state"], "open");
    assert_eq!(issue["body"], body);
    assert_eq!(issue["comments"][0]["body"], "Same here");
    let pr = views.iter().find(|view| view["kind"] == "pr").unwrap();
    assert_eq!(pr["pr"]["merge_conflicts"], true);
    assert_eq!(pr["pr"]["checks"][0]["result"], "failure");
    assert_eq!(pr["pr"]["review"], "changes_requested");
    let finding = &pr["reviews"][0]["comments"][0];
    assert_eq!(finding["path"], "src/a.rs");
    assert_eq!(
        (&finding["id"], &finding["thread"], &finding["resolved"]),
        (
            &serde_json::json!(21),
            &serde_json::json!(21),
            &serde_json::json!(false)
        )
    );
    // This gh answers no log lookup; tests/forge.rs covers failed logs.
    let errors = pr["errors"].as_array().unwrap();
    assert!(
        errors.iter().all(|error| error
            .as_str()
            .unwrap()
            .starts_with("rust: log unavailable: ")),
        "{pr}"
    );

    let status = fixture.ok(&["status", "view"]);
    assert_eq!(status["issue_status"]["state"], "open");

    let brief = fixture.ok(&["view", "pr", "--no-comments", "--workspace", "view"]);
    assert_eq!(brief.as_array().unwrap().len(), 1);
    assert!(brief[0].get("reviews").is_none());

    let text = fixture.run(&["view", "pr", "7", "--workspace", "view"]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("PR #7: Fix crash"), "{text}");
    assert!(text.contains("Branch:   view → main"), "{text}");
    assert!(
        text.contains("src/a.rs:3 · bot · t3 · comment 21 · unresolved"),
        "{text}"
    );

    // An unreadable item reports its error and fails the command.
    let missing = fixture.run(&["--json", "view", "issue", "9", "--workspace", "view"]);
    assert_eq!(missing.status.code(), Some(1));
    let missing: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert!(
        missing[0]["errors"][0]
            .as_str()
            .unwrap()
            .contains("different item")
    );
}

#[test]
fn stop_preserves_the_child_exit_status_when_group_inventory_fails() {
    let fixture = Fixture::new();
    fixture.add("worker");
    let bin = fixture.root.path().join("wrapper-bin");
    let ps = bin.join("ps");
    install_test_script_with_mode(&ps, "#!/bin/sh\nexit 1\n", 0o700);
    let ready = fixture.root.path().join("ready");
    let wrapper = fixture
        .command()
        .args([
            "exec",
            "worker",
            "--",
            "sh",
            "-c",
            "trap 'exit 7' TERM; : > \"$READY\"; while :; do sleep 30; done",
        ])
        // Fail only the wrapper's inventory; the daemon can still verify exit.
        .env("PATH", fixture.restricted_path(&bin))
        .env("READY", &ready)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("command signal handler", || ready.exists());
    fixture.ok(&["stop", "worker"]);
    let output = wrapper.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
}

#[test]
fn stop_allows_descendants_to_finish_signal_cleanup_after_the_leader_exits() {
    let fixture = Fixture::new();
    fixture.add("worker");
    let leader = fixture.root.path().join("leader.sh");
    let descendant = fixture.root.path().join("descendant.sh");
    let ready = fixture.root.path().join("ready");
    let cleaned = fixture.root.path().join("cleaned");
    fs::write(&leader, "trap 'exit 0' TERM\nsh \"$1\" &\nwait\n").unwrap();
    fs::write(
        &descendant,
        "trap 'sleep 0.2; echo done > \"$CLEANED\"; exit 0' TERM\n: > \"$READY\"\nwhile :; do sleep 30; done\n",
    )
    .unwrap();
    let wrapper = fixture
        .command()
        .args(["exec", "worker", "--", "sh"])
        .arg(&leader)
        .arg(&descendant)
        .env("READY", &ready)
        .env("CLEANED", &cleaned)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("descendant signal handler", || ready.exists());
    fixture.ok(&["stop", "worker"]);
    let output = wrapper.wait_with_output().unwrap();
    assert!(
        cleaned.exists(),
        "descendant cleanup was killed: {output:?}"
    );
    assert_eq!(
        fixture.ok(&["inspect", "worker"])["executions"],
        serde_json::json!([])
    );
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
        commit_file(&fixture.repo, ".gitattributes", "tracked merge=slow\n");
        let worker = fixture.add("worker");
        let path = Path::new(worker["path"].as_str().unwrap());
        commit_file(path, "tracked", "worker\n");
        commit_file(path, "a-added", "new file\n");
        commit_file(&fixture.repo, "tracked", "main\n");
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
                "echo $$ > .git/land-driver.pid.tmp; mv .git/land-driver.pid.tmp .git/land-driver.pid; exec sleep 60",
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
        wait_until("merge driver to start", || {
            if fixture.repo.join(".git/land-driver.pid").exists() {
                return true;
            }
            assert!(
                land.try_wait().unwrap().is_none(),
                "land exited before merge driver"
            );
            false
        });
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
        let status = wait_for("landing wrapper to stop", || land.try_wait().unwrap());
        assert!(!status.success());
        wait_until("merge driver to stop", || {
            let output = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "stat="])
                .output()
                .unwrap();
            let status = String::from_utf8_lossy(&output.stdout);
            status.trim().is_empty() || status.trim().starts_with('Z')
        });
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
    let script = bin.join("happy");
    install_test_script_with_mode(
        &script,
        r#"#!/bin/sh
{
  printf 'cwd=%s\n' "$PWD"
  printf 'args='; printf '%s\0' "$@"; printf '\n'
  printf 'scope=%s\nexecution=%s\nworkspace=%s\nport=%s\n' "$SHOAL_SCOPE_TOKEN" "$SHOAL_EXECUTION_ID" "$SHOAL_WORKSPACE" "$SHOAL_PORT_WEB"
  printf 'chat=%s\n' "$SHOAL_CHAT"
  printf 'reconnect=%s|%s|%s|%s|%s|%s\n' "$HAPPY_RECONNECT_SESSION_ID" "$HAPPY_RECONNECT_ENCRYPTION_KEY" "$HAPPY_RECONNECT_ENCRYPTION_VARIANT" "$HAPPY_RECONNECT_SEQ" "$HAPPY_RECONNECT_METADATA_VERSION" "$HAPPY_RECONNECT_AGENT_STATE_VERSION"
  if read -r _line; then printf 'stdin=data\n'; else printf 'stdin=eof\n'; fi
  test -t 1 && printf 'stdout=tty\n' || printf 'stdout=notty\n'
} > "$HAPPY_RECORD.tmp"
mv "$HAPPY_RECORD.tmp" "$HAPPY_RECORD"
echo "hello from happy"
echo "happy stderr" >&2
# A real session connects to Happy's server and heartbeats; tell the fake server.
if [ -n "$HAPPY_RECONNECT_SESSION_ID" ] && [ -n "$HAPPY_SERVER_URL" ]; then
  sleep 1
  curl -s -X POST "$HAPPY_SERVER_URL/test/activate/$HAPPY_RECONNECT_SESSION_ID" > /dev/null
fi
sleep 300
"#,
        0o700,
    );
    // Happy starts these itself; launches need them on PATH.
    for agent in ["claude", "codex"] {
        let path = bin.join(agent);
        if !path.exists() {
            install_test_script_with_mode(&path, "#!/bin/sh\n", 0o700);
        }
    }
    fixture.root.path().join("happy-record")
}

fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn wait_for_busy(fixture: &Fixture, workspace: &str) {
    wait_until("blocked acquisition notification", || {
        fixture
            .ok(&["notifications"])
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "resource_busy" && event["workspace"] == workspace)
    });
}

const HAPPY_ENABLED: &str = "[happy]\nenabled = true\n";

#[test]
fn happy_agents_are_refused_until_enabled() {
    let fixture = Fixture::new();
    let record = install_fake_happy(&fixture);
    let output = fixture
        .command()
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "happy-off",
            "--agent",
            "happy-claude",
        ])
        .env("HAPPY_RECORD", &record)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("happy.enabled = true"), "{stderr}");
    // The refusal comes before any work is created.
    assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
}

#[test]
fn happy_sessions_launch_detached_tracked_and_stop_with_the_workspace() {
    let fixture = Fixture::with_config(Some(HAPPY_ENABLED));
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
            // Without Happy credentials, the guidance message is saved instead.
            let prompt_file = launch["prompt_file"].as_str().unwrap();
            assert!(
                fs::read_to_string(prompt_file)
                    .unwrap()
                    .starts_with(GUIDANCE_START)
            );
            let config: toml::Value = toml::from_str(
                &fs::read_to_string(fixture.root.path().join(".codex/config.toml")).unwrap(),
            )
            .unwrap();
            let repository = fixture.ok(&["repo", "list"])[0].clone();
            let directory = Path::new(repository["workspaces_dir"].as_str().unwrap());
            assert_eq!(
                config["projects"][directory.to_str().unwrap()]["trust_level"].as_str(),
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
            let repository = fixture.ok(&["repo", "list"])[0].clone();
            let directory = Path::new(repository["workspaces_dir"].as_str().unwrap());
            assert_eq!(
                config["projects"][directory.to_str().unwrap()]["hasTrustDialogAccepted"],
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
        assert!(
            strip_shoal_arguments(&recorded, '\0').contains(expected_args),
            "{recorded}"
        );
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
    let fixture = Fixture::with_config(Some(HAPPY_ENABLED));
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("worker-template.md"), "General {workspace}").unwrap();
    let record = install_fake_happy(&fixture);
    let gh = fixture.root.path().join("bin/gh");
    let title = "Fix API timeout";
    let body = "Keep the connection alive.";
    install_test_script_with_mode(
        &gh,
        format!(
            "#!/bin/sh\nprintf '%s' '{}'\n",
            serde_json::json!({"state":"OPEN", "number": 34, "title": title, "body": body})
        ),
        0o700,
    );
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
        assert_eq!(name, "34-fix-api-timeout");
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
            assert_eq!(args[6], "--append-system-prompt");
            assert_eq!(after_guidance(args[7]), "General 34-fix-api-timeout");
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
                after_guidance(&prompt),
                format!(
                    "General 34-fix-api-timeout\n\nWork on issue #34: {title}\n\
                     https://github.com/team/project/issues/34\n\n\
                     Issue description:\n{body}\n\n\
                     `shoal view` shows the issue with its comments and later edits."
                )
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
        fixture.ok(&["rm", &name, "--yes"]);
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
    let fixture = Fixture::with_config(Some(HAPPY_ENABLED));
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
        let message = fs::read_to_string(launch["prompt_file"].as_str().unwrap()).unwrap();
        assert_eq!(after_guidance(&message), prompt);
        let plaintext = serde_json::json!({
            "role": "user",
            "content": {"type": "text", "text": message},
            "meta": {"sentFrom": "shoal"},
        })
        .to_string()
        .len();
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
    fixture.add("orphan");
    let launch_orphan = || {
        fixture
            .command()
            .args(["--json", "happy", "codex", "orphan", "--prompt", "hello"])
            .env("HAPPY_SERVER_URL", &server.url)
            // Only the fixture directory and system tools: a real happy must not be found.
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    fixture.root.path().join("bin").display(),
                    fixture.system_path()
                ),
            )
            .output()
            .unwrap()
    };
    // A missing happy is refused before any session is seeded.
    let happy = fixture.root.path().join("bin/happy");
    fs::remove_file(&happy).unwrap();
    let requests = server.state()["requests"].as_array().unwrap().len();
    let output = launch_orphan();
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("happy is not installed"),
        "{output:?}"
    );
    assert_eq!(
        server.state()["requests"].as_array().unwrap().len(),
        requests
    );
    // An executable that cannot start fails only after seeding.
    install_test_script_with_mode(&happy, "#!/nonexistent/interpreter\n", 0o700);
    let output = launch_orphan();
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
        "[cleanup.auto]\nenabled=false\n[resources.lock]\ncapacity=1\n",
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
    install_test_script(bin.join("claude"), "#!/bin/sh\nexit 7\n");
    let agent = fixture
        .command()
        .args(["claude", "waiter"])
        .env("PATH", fixture.restricted_path(&bin))
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
    let list = fixture.run(&["work"]);
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
    assert_eq!(fixture.run(&["work"]).stderr, b"");
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
    assert_eq!(fixture.acknowledge("waiter")["type"], "ok");
    wait_removed(&fixture, "waiter");
    let line = lines
        .recv_timeout(Duration::from_secs(60))
        .expect("followed notification");
    let notification: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(notification["workspace"], "waiter");
    assert_eq!(notification["kind"], "workspace_removed");
    assert_eq!(
        notification["message"],
        "removed after the merge acknowledgement; restore it with shoal restore waiter"
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
    install_test_script(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\ngit config user.email > setup-email\n",
    );
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
    let before = fixture.ok(&["work"]);
    let output = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "bad-profile",
        "--git-profile",
        "unknown",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("git profile unknown is not defined"));
    assert_eq!(fixture.ok(&["work"]), before);
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
fn edit_runs_the_editor_or_the_configured_edit_command() {
    let fixture = Fixture::new();
    let workspace = fixture.add("editing");
    let path = workspace["path"].as_str().unwrap();
    let edit = |visual: Option<&str>, editor: Option<&str>| {
        let mut command = fixture.command();
        for (name, value) in [("VISUAL", visual), ("EDITOR", editor)] {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        command
            .args(["edit", "editing", "--", "--flag"])
            .output()
            .unwrap()
    };
    let output = edit(Some("printf %s|"), Some("false"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{path}|--flag|")
    );
    assert_eq!(
        edit(None, Some("printf %s|")).stdout,
        format!("{path}|--flag|").as_bytes()
    );
    let output = edit(None, None);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("set $VISUAL, $EDITOR or [commands] edit")
    );

    fs::write(
        Path::new(path).join(".shoal.toml"),
        "[commands]\nedit = ['printf', '%s|', '{branch}']\n",
    )
    .unwrap();
    assert_eq!(edit(Some("false"), None).stdout, b"editing|--flag|");
}

#[test]
fn run_lists_command_layers_and_executes_names_that_collide_with_built_ins() {
    let fixture = Fixture::with_config(Some(
        "[commands]\nglobal = ['printf', '%s', 'global command']\nshadowed = ['global']\nwork = ['printf', '%s', 'configured work']\nclaude = ['global-claude']\n",
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

    let output = fixture.run(&["run", "work", "run-command"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"configured work");
    assert!(fixture.ok(&["work"]).is_array());

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
    assert_eq!(command("work")["bare_name"], "built_in");
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
        strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
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
        strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
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
        strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
        "--model\nexample\ncustom default\nprompt=\n"
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
        strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
        "--model\nexample\ncustom default\nprompt=\n"
    );
}

#[test]
fn repository_commands_pick_a_workspace_that_defines_them() {
    let fixture = Fixture::new();
    let defining = fixture.add("defining");
    fixture.add("other");
    let path = Path::new(defining["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[commands]\nlocal-check = ['printf', '%s', 'repository command']\n",
    )
    .unwrap();
    let id = defining["id"].as_str().unwrap();
    let (output, rows) = fixture.pick(&["local-check"], id, "");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"repository command");
    assert!(rows.starts_with(&format!("{id}\t")), "{rows:?}");
    assert_eq!(rows.lines().count(), 1, "{rows:?}");
    let previous = fixture.run(&["local-check", "-"]);
    assert!(previous.status.success(), "{previous:?}");
    assert_eq!(previous.stdout, b"repository command");
    let (output, rows) = fixture.pick(&["lsit"], id, "");
    assert!(!output.status.success());
    assert!(rows.is_empty(), "{rows:?}");
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
        error.contains("unknown command \"lsit\"") && error.contains("no current workspace"),
        "{error}"
    );
    assert!(!error.contains("non-interactive"), "{error}");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[commands]\nlocal-check = ['printf', '%s', 'repository command']\n",
    )
    .unwrap();
    let output = fixture.run(&["local-check"]);
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(error.contains("in the current context"), "{error}");
    assert!(
        error.contains("repository-only commands require a current or explicit workspace"),
        "{error}"
    );
    assert!(!error.contains("define it"), "{error}");
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
fn misspelled_built_ins_are_rejected_with_a_suggestion() {
    let fixture = Fixture::new();
    let workspace = fixture.add("command-target");
    let path = Path::new(workspace["path"].as_str().unwrap());
    for directory in [fixture.root.path(), path] {
        for args in [
            vec!["ports"],
            vec!["ports", "command-target"],
            vec!["run", "ports"],
            vec!["run", "ports", "command-target"],
        ] {
            let output = fixture
                .command()
                .current_dir(directory)
                .args(&args)
                .output()
                .unwrap();
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{directory:?} {args:?}");
            assert!(error.contains("unknown command \"ports\""), "{error}");
            assert!(error.contains("`shoal port`"), "{error}");
            assert!(!error.contains("needs a workspace"), "{error}");
            assert!(!error.contains("non-interactive"), "{error}");
        }
    }
    // A similar built-in name must not prevent an explicitly configured shortcut.
    fs::write(
        path.join(".shoal.toml"),
        "[commands]\nports = ['printf', '%s', 'configured command']\n",
    )
    .unwrap();
    let output = fixture.run(&["ports", "command-target"]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"configured command");
}

#[test]
fn agent_auth_wrappers_are_inherited_without_changing_ordinary_executions() {
    let fixture = Fixture::new();
    let workspace = fixture.add("agent-auth");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    let wrappers = fixture.root.path().join("wrappers with spaces");
    for tool in ["fj", "gh"] {
        let path = wrappers.join(tool);
        install_test_script(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' 'agent {tool}' \"$@\"\n"),
        );
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
fn secrets_reach_agents_but_not_ordinary_executions_or_hooks() {
    let fixture = Fixture::with_config(Some(
        "[secrets.AGENT_TOKEN]\ncommand = ['printf', '%s\\n', 'global secret']\n",
    ));
    let workspace = fixture.add("secrets");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    install_test_script(
        worktree.join("post-setup.sh"),
        "#!/bin/sh\nprintf '%s|%s' \"${AGENT_TOKEN-unset}\" \"${SHOAL_SECRET_ENV-unset}\" > hook-env\n",
    );
    install_test_script(worktree.join("setup.sh"), "#!/bin/sh\n");
    fs::write(
        worktree.join(".shoal.toml"),
        "setup_cmd = 'setup.sh'\npost_setup_cmd = 'post-setup.sh'\n[commands]\nclaude = ['sh', '-c', 'printf \"%s|%s\\n\" \"$AGENT_TOKEN\" \"$SHOAL_SECRET_ENV\"']\n",
    )
    .unwrap();
    let output = fixture.run(&["claude", "secrets"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"global secret|AGENT_TOKEN\n");
    let output = fixture.run(&["exec", "secrets", "--", "printenv", "AGENT_TOKEN"]);
    assert!(!output.status.success(), "{output:?}");

    // A hook started beneath an agent drops the secrets it inherited.
    let output = fixture
        .command()
        .env("AGENT_TOKEN", "inherited")
        .env("SHOAL_SECRET_ENV", "AGENT_TOKEN")
        .args(["setup", "secrets"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(worktree.join("hook-env")).unwrap(),
        "unset|unset"
    );

    // The saved repository config can replace a secret; a failing command
    // stops the launch.
    let saved = fixture.root.path().join("saved.toml");
    fs::write(&saved, "[secrets.AGENT_TOKEN]\ncommand = ['false']\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    let output = fixture.run(&["claude", "secrets"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("secrets.AGENT_TOKEN"), "{stderr}");

    // Secret sources describe this machine, so a tracked file cannot name one.
    fs::write(
        worktree.join(".shoal.toml"),
        "[secrets.OTHER]\ncommand = ['true']\n",
    )
    .unwrap();
    let output = fixture.run(&["config", "show", "secrets"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[secrets] belongs in"), "{stderr}");
}

#[test]
fn tool_secrets_reach_only_their_tools() {
    let fixture = Fixture::with_config(Some(
        "[secrets.TOOL_TOKEN]\ncommand = ['printf', 'tool secret']\ntools = ['show-token']\n",
    ));
    let workspace = fixture.add("tool-secrets");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    let bin = fixture.root.path().join("bin");
    install_test_script(
        bin.join("show-token"),
        "#!/bin/sh\nprintf '%s|%s|%s\\n' \"${TOOL_TOKEN-unset}\" \"$SHOAL_SECRET_ENV\" \"$*\"\n",
    );
    install_test_script(worktree.join("setup.sh"), "#!/bin/sh\n");
    install_test_script(
        worktree.join("post-setup.sh"),
        "#!/bin/sh\nshow-token hook > hook-output\nprintf '%s' \"${SHOAL_SECRET_TOOLS-unset}\" >> hook-output\n",
    );
    fs::write(
        worktree.join(".shoal.toml"),
        "setup_cmd = 'setup.sh'\npost_setup_cmd = 'post-setup.sh'\n[commands]\nclaude = ['sh', '-c', 'printf \"%s\\n\" \"${TOOL_TOKEN-unset}\"; show-token \"two words\" -x']\n",
    )
    .unwrap();
    let output = fixture.run(&["claude", "tool-secrets"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "unset\ntool secret|TOOL_TOKEN|two words -x\n"
    );
    let output = fixture.run(&["exec", "tool-secrets", "--", "show-token"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"unset||\n");
    assert!(
        fs::read_dir(fixture.root.path().join("state"))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("secret-tools-"))
    );

    // A hook started beneath an agent runs the tool without its launcher.
    let launchers = fixture.root.path().join("launchers");
    install_test_script(launchers.join("show-token"), "#!/bin/sh\necho launcher\n");
    let path = std::env::join_paths([launchers.clone(), bin].into_iter().chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let output = fixture
        .command()
        .env("PATH", path)
        .env("SHOAL_SECRET_TOOLS", &launchers)
        .args(["setup", "tool-secrets"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(worktree.join("hook-output")).unwrap(),
        "unset||hook\nunset"
    );

    // An inner launch's launcher hands over to the enclosing launch's, which
    // runs the tool itself rather than returning to the inner one.
    let bin = fixture.root.path().join("bin");
    install_test_script(
        bin.join("show-tokens"),
        "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$INNER_TOKEN\" \"$OUTER_TOKEN\" \"$SHOAL_SECRET_ENV\"\n",
    );
    let mut path = Vec::new();
    for name in ["INNER", "OUTER"] {
        let launchers = fixture.root.path().join(name.to_lowercase());
        fs::create_dir_all(&launchers).unwrap();
        fs::write(
            launchers.join(".secrets.json"),
            format!(
                r#"{{"{name}_TOKEN": {{"command": ["printf", "{name}"], "tools": ["show-tokens"]}}}}"#
            ),
        )
        .unwrap();
        install_test_script(
            launchers.join("show-tokens"),
            format!(
                "#!/bin/sh\nexec {:?} internal secret-tool {:?} show-tokens -- \"$@\"\n",
                env!("CARGO_BIN_EXE_shoal"),
                launchers
            ),
        );
        path.push(launchers);
    }
    path.push(bin);
    let path = std::env::join_paths(path.into_iter().chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let output = fixture
        .command()
        .env("PATH", path)
        .arg("internal")
        .arg("secret-tool")
        .arg(fixture.root.path().join("inner"))
        .args(["show-tokens", "--"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"INNER|OUTER|INNER_TOKEN:OUTER_TOKEN\n");
}

#[test]
fn gated_tool_secrets_wait_for_the_users_approval() {
    let fixture = Fixture::with_config(Some(
        "[secrets.TOOL_TOKEN]\ncommand = ['sh', '-c', 'echo fetched >> \"$0\"; printf secret', 'fetches']\ntools = ['show-token']\nrequires_approval = true\n",
    ));
    let workspace = fixture.add("gated");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    install_test_script(
        fixture.root.path().join("bin").join("show-token"),
        "#!/bin/sh\nprintf '%s\\n' \"$TOOL_TOKEN\"\n",
    );
    fs::write(
        worktree.join(".shoal.toml"),
        "[commands]\nclaude = ['show-token']\n",
    )
    .unwrap();
    let launch = || {
        let output = fixture.run(&["claude", "gated"]);
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            output.stdout,
        )
    };
    let (success, stderr, _) = launch();
    assert!(!success);
    assert!(
        stderr.contains("shoal acquire secret TOOL_TOKEN --reason"),
        "{stderr}"
    );

    let request = pending_approval(scoped_command(
        &fixture,
        "gated",
        &["acquire", "secret", "TOOL_TOKEN", "--reason", "sign in"],
    ));
    assert_eq!(request["target"], "secret/TOOL_TOKEN");
    let id = request["id"].to_string();
    let id = id.as_str();
    let (success, stderr, _) = launch();
    assert!(!success);
    assert!(
        stderr.contains(&format!("approval {id} is pending")),
        "{stderr}"
    );
    // The value is fetched only once the daemon confirms the grant.
    assert!(!worktree.join("fetches").exists());

    fixture.ok(&["approve", id]);
    let (success, stderr, stdout) = launch();
    assert!(success, "{stderr}");
    assert_eq!(stdout, b"secret\n");
    assert!(worktree.join("fetches").exists());

    fixture.ok(&["release", "--workspace", "gated", "secret", "TOOL_TOKEN"]);
    let (success, _, _) = launch();
    assert!(!success);

    // A launcher from before approval was required still follows current policy.
    let launchers = fixture.root.path().join("ungated");
    fs::create_dir_all(&launchers).unwrap();
    fs::write(
        launchers.join(".secrets.json"),
        r#"{"TOOL_TOKEN": {"command": ["sh", "-c", "echo fetched >> \"$0\"; printf secret", "fetches"], "tools": ["show-token"]}}"#,
    )
    .unwrap();
    let output = fixture.run(&[
        "exec",
        "gated",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "internal",
        "secret-tool",
        launchers.to_str().unwrap(),
        "show-token",
        "--",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("shoal acquire secret TOOL_TOKEN --reason"),
        "{stderr}"
    );
}

#[test]
fn agent_git_profile_applies_only_to_agent_processes() {
    let fixture = Fixture::with_config(Some(
        "[git.profiles.agent]\nuser.name = 'Agent Name'\nuser.email = 'agent@example.invalid'\n",
    ));
    git(
        &fixture.repo,
        &["config", "user.email", "main@example.invalid"],
    );
    let workspace = fixture.add("agent-git");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    let commands = "[commands]\nclaude = ['sh', '-c', 'git config user.email; git config core.abbrev; git var GIT_AUTHOR_IDENT; git var GIT_COMMITTER_IDENT']\n";
    fs::write(
        worktree.join(".shoal.toml"),
        format!("{commands}[agent_auth]\ngit_profile = 'agent'\n"),
    )
    .unwrap();
    let output = fixture
        .command()
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.abbrev")
        .env("GIT_CONFIG_VALUE_0", "12")
        .env("GIT_AUTHOR_EMAIL", "inherited@example.invalid")
        .env("GIT_COMMITTER_NAME", "Inherited Name")
        .args(["claude", "agent-git"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 4, "{stdout}");
    assert_eq!(lines[..2], ["agent@example.invalid", "12"], "{stdout}");
    for ident in &lines[2..] {
        assert!(
            ident.starts_with("Agent Name <agent@example.invalid> "),
            "{stdout}"
        );
    }
    let output = fixture.run(&["exec", "agent-git", "--", "git", "config", "user.email"]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"main@example.invalid\n");
    fs::write(
        worktree.join(".shoal.toml"),
        format!("{commands}[agent_auth]\ngit_profile = 'missing'\n"),
    )
    .unwrap();
    let output = fixture.run(&["claude", "agent-git"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("git profile missing is not defined"),
        "{stderr}"
    );
}

#[test]
fn detached_agents_use_auth_wrappers_and_invalid_wrappers_prevent_launch() {
    let fixture = Fixture::new();
    let workspace = fixture.add("detached-auth");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    let wrapper = fixture.root.path().join("fj-agent");
    install_test_script(
        &wrapper,
        "#!/bin/sh\nprintf '%s\\n' 'detached agent' \"$@\"\nexit 23\n",
    );
    fs::write(
        worktree.join(".shoal.toml"),
        "[agent_auth]\nfj = '~/fj-agent'\n",
    )
    .unwrap();
    let log = fixture.root.path().join("agent.log");
    let output = fixture.run(&[
        "internal",
        "detached",
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
    install_fake_agents(&fixture, &["claude"]);
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
    fixture.ok(&["rm", "custom-topic", "--yes"]);
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
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
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
        .args(["--json", "adopt", "existing worktree"])
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
    assert_eq!(w["base_ref"], "refs/heads/main");
    assert_eq!(
        w["base_commit"],
        git(&fixture.repo, &["rev-parse", "main"]).trim()
    );
    fixture.restart();
    let again = fixture.ok(&[
        "adopt",
        "~/existing worktree",
        "--repo",
        fixture.repo.to_str().unwrap(),
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
    assert!(fixture.ok(&["work"]).as_array().unwrap().is_empty());
}

#[test]
fn adopt_copy_copies_dirty_and_untracked_work_to_the_default_directory() {
    let fixture = Fixture::new();
    let source = fixture.root.path().join("external worktree");
    let repo = fixture.repo.to_str().unwrap();
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "copy/topic",
            source.to_str().unwrap(),
        ],
    );
    // A tracked file deleted from a directory that remains stays deleted.
    fs::create_dir(source.join("nested")).unwrap();
    fs::write(source.join("nested/kept"), "kept\n").unwrap();
    fs::write(source.join("nested/deleted"), "deleted\n").unwrap();
    git(&source, &["add", "nested"]);
    git(
        &source,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "-m",
            "Add nested files",
        ],
    );
    fs::remove_file(source.join("nested/deleted")).unwrap();
    fs::write(source.join("tracked"), "changed\n").unwrap();
    fs::write(source.join("untracked"), "new\n").unwrap();
    fs::remove_file(source.join(".gitignore")).unwrap();
    let output = fixture.ok(&["adopt", source.to_str().unwrap(), "--repo", repo, "--copy"]);
    assert_eq!(output["branch"], "copy/topic-2");
    let destination = PathBuf::from(output["path"].as_str().unwrap());
    assert!(destination.starts_with(fixture.shoal_dir()));
    assert_eq!(
        fs::read_to_string(destination.join("tracked")).unwrap(),
        "changed\n"
    );
    assert_eq!(
        fs::read_to_string(destination.join("untracked")).unwrap(),
        "new\n"
    );
    assert!(!destination.join(".gitignore").exists());
    assert_eq!(
        fs::read_to_string(destination.join("nested/kept")).unwrap(),
        "kept\n"
    );
    assert!(!destination.join("nested/deleted").exists());
    assert_eq!(
        fs::read_to_string(source.join("tracked")).unwrap(),
        "changed\n"
    );
    assert!(!source.join(".gitignore").exists());
    assert!(source.exists());
}

#[test]
fn adopt_copy_rolls_back_when_adoption_name_is_taken() {
    let fixture = Fixture::new();
    let source = fixture.root.path().join("external worktree");
    let occupied = fixture.root.path().join("occupied worktree");
    let repo = fixture.repo.to_str().unwrap();
    let occupied_workspace = fixture.ok(&[
        "add",
        repo,
        "copy-topic-2",
        "--path",
        occupied.to_str().unwrap(),
    ]);
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "copy/topic",
            source.to_str().unwrap(),
        ],
    );
    fs::write(source.join("tracked"), "changed in source\n").unwrap();
    fs::write(source.join("untracked"), "preserve source\n").unwrap();
    let worktrees = git(&fixture.repo, &["worktree", "list", "--porcelain"]);
    let repositories = fixture.ok(&["repo", "list"]);
    let destination =
        Path::new(repositories[0]["workspaces_dir"].as_str().unwrap()).join("copy-topic-2");

    let output = fixture.run(&["adopt", source.to_str().unwrap(), "--repo", repo, "--copy"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("workspace name already exists"));
    assert!(source.exists());
    assert_eq!(
        fs::read_to_string(source.join("tracked")).unwrap(),
        "changed in source\n"
    );
    assert_eq!(
        fs::read_to_string(source.join("untracked")).unwrap(),
        "preserve source\n"
    );

    assert!(!destination.exists());
    assert!(
        git(&fixture.repo, &["branch", "--list", "copy/topic-2"])
            .trim()
            .is_empty()
    );
    assert_eq!(
        git(&fixture.repo, &["worktree", "list", "--porcelain"]),
        worktrees
    );
    assert_eq!(
        fs::read_to_string(occupied.join("tracked")).unwrap(),
        "committed\n"
    );
    let workspaces = fixture.ok(&["work"]);
    assert_eq!(workspaces.as_array().unwrap().len(), 1);
    assert_eq!(workspaces[0]["id"], occupied_workspace["id"]);
    assert_eq!(workspaces[0]["path"], occupied_workspace["path"]);
}

#[test]
fn adopt_copy_refuses_a_dangling_symlink_at_the_default_directory() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let add = |branch: &str, name: &str| {
        let path = fixture.root.path().join(name);
        git(
            &fixture.repo,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
        path
    };
    let first = add("copy/first", "first worktree");
    let output = fixture.ok(&["adopt", first.to_str().unwrap(), "--repo", repo, "--copy"]);
    let directory = PathBuf::from(output["path"].as_str().unwrap());
    let directory = directory.parent().unwrap();
    let source = add("copy/second", "second worktree");
    let planted = directory.join("copy-second-2");
    std::os::unix::fs::symlink(fixture.root.path().join("missing"), &planted).unwrap();
    let output = fixture.run(&["adopt", source.to_str().unwrap(), "--repo", repo, "--copy"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("workspace path already exists"));
    assert!(planted.is_symlink());
    let branches = git(&fixture.repo, &["branch", "--list", "copy/second-2"]);
    assert!(branches.trim().is_empty());
}

#[test]
fn adopt_copy_refuses_a_default_directory_that_resolves_into_the_source() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let add = |branch: &str, name: &str| {
        let path = fixture.root.path().join(name);
        git(
            &fixture.repo,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
        fs::canonicalize(path).unwrap()
    };
    let first = add("copy/first", "first worktree");
    let output = fixture.ok(&["adopt", first.to_str().unwrap(), "--repo", repo, "--copy"]);
    let directory = PathBuf::from(output["path"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_owned();
    fixture.ok(&["rm", output["name"].as_str().unwrap(), "--yes"]);
    let source = add("copy/second", "second worktree");
    fs::remove_dir_all(&directory).unwrap();
    std::os::unix::fs::symlink(&source, &directory).unwrap();
    let output = fixture.run(&["adopt", source.to_str().unwrap(), "--repo", repo, "--copy"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("overlaps the source worktree"));
    assert!(!source.join("copy-second-2").exists());
}

#[test]
fn adopt_infers_a_checkout_with_a_separate_git_directory() {
    let fixture = Fixture::new();
    let root = fixture.root.path();
    let checkout = root.join("separate");
    fs::create_dir(&checkout).unwrap();
    let checkout = fs::canonicalize(checkout).unwrap();
    let git_dir = root.join("separate.git");
    git(
        &checkout,
        &[
            "init",
            "-b",
            "main",
            "--separate-git-dir",
            git_dir.to_str().unwrap(),
        ],
    );
    git(
        &checkout,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    fixture.ok(&["repo", "add", checkout.to_str().unwrap()]);
    let path = root.join("separate-worktree");
    git(
        &checkout,
        &["worktree", "add", "-b", "topic", path.to_str().unwrap()],
    );
    let adopted = fixture.ok(&["adopt", path.to_str().unwrap()]);
    assert_eq!(adopted["name"], "topic");
}

#[test]
fn adopt_infers_the_repository_from_the_worktree_remote() {
    let fixture = Fixture::new();
    fixture.add_github_origin();
    let adopt = |name: &str, remote: &str| {
        let clone = init_repo(fixture.root.path(), name, &[("tracked", "clone\n")]);
        git(&clone, &["remote", "add", "origin", remote]);
        let path = fixture.root.path().join(format!("{name}-worktree"));
        git(
            &clone,
            &["worktree", "add", "-b", "topic", path.to_str().unwrap()],
        );
        let output = fixture.run(&["adopt", path.to_str().unwrap()]);
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    // The shared remote selects the registration, whose checkout then refuses
    // a worktree of another clone.
    let error = adopt("clone", "git@github.com:team/project.git");
    assert!(
        error.contains("linked worktree of this repository"),
        "{error}"
    );
    let error = adopt("other", "git@github.com:team/other.git");
    assert!(error.contains("pass --repo <repository>"), "{error}");
    assert!(fixture.ok(&["work"]).as_array().unwrap().is_empty());
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
    fixture.ok(&["adopt", adopted.to_str().unwrap(), "--repo", repo]);
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
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 2);
}

#[test]
fn custom_agents_launch_with_layered_prompts_scope_and_notifications() {
    let fixture = Fixture::with_config(Some(
        "default_agent = 'helper'\nworker_template = 'Follow {branch}'\n[commands]\nhelper = ['missing-global-launcher']\n",
    ));
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    for (name, script) in [
        (
            "gh",
            "#!/bin/sh\nprintf '%s' '{\"state\":\"OPEN\",\"number\":37,\"title\":\"Literal {branch}\",\"body\":\"$(false)\"}'\n",
        ),
        (
            "fake-agent",
            "#!/bin/sh\ntest -n \"$SHOAL_SCOPE_TOKEN\" || exit 99\nprintf '%s\\0' \"$@\" > \"$HOME/agent-args\"\nexit 7\n",
        ),
    ] {
        let path = bin.join(name);
        install_test_script_with_mode(&path, script, 0o700);
    }
    fixture.add_github_origin();
    let saved = fixture.root.path().join("saved.toml");
    fs::write(
        &saved,
        "[commands]\nhelper = ['fake-agent', '--prompt={prompt}', '{args}', '{branch}']\n",
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
        "add",
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
    let args = fs::read_to_string(fixture.root.path().join("agent-args")).unwrap();
    assert!(
        args.starts_with(&format!("--prompt={GUIDANCE_START}")),
        "{args}"
    );
    assert_eq!(
        remove_guidance(&args),
        format!(
            "--prompt=Follow {branch}\n\nWork on issue #37: Literal {{branch}}\n\
             https://github.com/team/project/issues/37\n\nIssue description:\n$(false)\n\n\
             `shoal view` shows the issue with its comments and later edits.\0\
             literal {{prompt}}\0{branch}\0"
        )
    );
    assert_eq!(
        fixture.ok(&["inspect", workspace["id"].as_str().unwrap()])["executions"],
        serde_json::json!([])
    );
    let notifications = fixture.ok(&["notifications"]);
    assert!(
        notifications
            .to_string()
            .contains("helper exited with code 7")
    );
    assert!(!fixture.root.path().join(".claude.json").exists());
    assert!(!fixture.root.path().join(".codex/config.toml").exists());

    let output = fixture.run(&[
        "run",
        "helper",
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
    fs::write(&saved, "[commands]\nhelper = ['fake-agent', '{args}']\n").unwrap();
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
        "helper",
        "--",
        "user message",
    ]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        remove_guidance(&fs::read_to_string(fixture.root.path().join("agent-args")).unwrap()),
        "Follow custom-add\0user message\0"
    );
    let before = fixture.ok(&["work"]);
    let unknown = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "unknown-agent",
        "--agent",
        "typo",
    ]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown agent"));
    assert_eq!(fixture.ok(&["work"]), before);
}

#[test]
fn configured_ai_tools_run_as_agents_from_their_own_command() {
    let fixture = Fixture::with_config(Some(
        "worker_template = 'Follow {branch}'\n[ai.droid]\ncommand = ['fake-droid', '{args}']\n\
         [ai.notes]\nskill_dir = '~/notes'\n",
    ));
    let bin = fixture.root.path().join("bin");
    for name in ["fake-droid", "repo-droid"] {
        let path = bin.join(name);
        install_test_script_with_mode(
            &path,
            format!(
                "#!/bin/sh\ntest -n \"$SHOAL_SCOPE_TOKEN\" || exit 99\nprintf '{name}\\0' > \"$HOME/agent-args\"\nprintf '%s\\0' \"$@\" >> \"$HOME/agent-args\"\nexit 7\n"
            ),
            0o700,
        );
    }
    let workspace = fixture.add("provider");
    let args =
        || remove_guidance(&fs::read_to_string(fixture.root.path().join("agent-args")).unwrap());
    for words in [
        vec!["droid", "provider", "--", "user message"],
        vec!["run", "droid", "provider", "--", "user message"],
    ] {
        let output = fixture.run(&words);
        assert_eq!(output.status.code(), Some(7), "{words:?}: {output:?}");
        assert_eq!(
            args(),
            format!(
                "fake-droid\0Follow {}\0user message\0",
                workspace["branch"].as_str().unwrap()
            )
        );
    }
    assert!(
        fixture
            .ok(&["notifications"])
            .to_string()
            .contains("droid exited with code 7")
    );

    // A repository launcher replaces the one [ai] supplies.
    let saved = fixture.root.path().join("saved.toml");
    fs::write(&saved, "[commands]\ndroid = ['repo-droid']\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    let output = fixture.run(&["droid", "provider"]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert!(args().starts_with("repo-droid\0Follow "), "{}", args());

    // Built-in providers without specialized launchers place the prompt the
    // same way.
    fs::copy(bin.join("fake-droid"), bin.join("opencode")).unwrap();
    let output = fixture.run(&["opencode", "provider", "--", "user message"]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        args(),
        format!(
            "fake-droid\0--prompt\0Follow {}\0user message\0",
            workspace["branch"].as_str().unwrap()
        )
    );

    // A tool with only a skill directory is not an agent.
    let output = fixture.run(&["notes", "provider"]);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown command \"notes\""),
        "{output:?}"
    );

    // A provider whose launcher is missing never starts.
    fs::remove_file(bin.join("repo-droid")).unwrap();
    let output = fixture.run(&["droid", "provider"]);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cannot start droid: repo-droid is not installed"),
        "{output:?}"
    );
}

#[test]
fn resource_hooks_retain_leases_on_failure_and_run_during_removal() {
    let mut fixture = Fixture::new();
    install_test_script(
        fixture.repo.join("permit.sh"),
        r#"#!/bin/sh
set -eu
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
test "$PWD" = "$SHOAL_WORKSPACE_PATH"
printf '%s\n' "$SHOAL_RESOURCE_LEASE" >> "$HOME/$SHOAL_HOOK"
test ! -f "$SHOAL_HOOK-fails" || { echo 'resource busy' >&2; exit 3; }
"#,
    );
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
        vec!["rm", "hooked", "--yes"],
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
    fixture.ok(&["rm", "waiter", "--yes"]);
    let events = fs::read_to_string(fixture.root.path().join("pre_resource_release")).unwrap();
    assert_eq!(events.lines().count(), 4);
    fixture.ok(&["rm", "hooked", "--yes"]);
}

#[test]
fn resource_hooks_exclude_concurrent_release_setup_and_removal() {
    let fixture = Fixture::new();
    install_test_script(
        fixture.repo.join("permit.sh"),
        r#"#!/bin/sh
set -eu
touch "$HOME/hook-entered"
while test ! -f "$HOME/hook-continue"; do sleep 0.05; done
"#,
    );
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
    let leases = fixture.ok(&["inspect", "hooked"])["resources"].clone();
    assert_eq!(leases.as_array().unwrap().len(), 1);
    assert_eq!(leases[0]["resource"], "signing");
    for args in [
        vec!["resource", "acquire", "signing", "hooked"],
        vec!["resource", "release", "signing", "hooked"],
        vec!["setup", "hooked"],
        vec!["rm", "hooked", "--yes"],
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
    fixture.ok(&["rm", "hooked", "--yes"]);
}

#[test]
fn workspace_hook_resolution_preserves_layers_and_directories() {
    let mut fixture = Fixture::with_config(Some(""));
    let workspace = fixture.add("hooks");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    // Global hooks are installed only after creation, so these paths need not
    // exist: resolution must not run a hook or require its executable yet.
    fs::write(
        fixture.root.path().join(".config/shoal/config.toml"),
        "pre_setup_cmd = 'global'\npost_setup_cmd = 'global'\npre_remove_cmd = 'global'\npost_remove_cmd = 'global'\n\
         post_resource_acquire_cmd = 'global'\npre_resource_release_cmd = 'global'\n",
    )
    .unwrap();
    fixture.restart();
    let resolve = |kind: &str| {
        fixture.request_with_id(
            2,
            serde_json::json!({"workspace_hook":{"workspace":workspace["id"],"kind":kind}}),
        )
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
        ("post_setup", true),
        ("pre_remove", true),
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
fn global_post_setup_and_pre_remove_hooks_run_without_setup() {
    let fixture = Fixture::with_config(Some(
        "post_setup_cmd = 'hook.sh'\npre_remove_cmd = 'hook.sh'\n[env]\nSESSION = '{workspace}'\nPROFILE = '{workspace_path}/profile'\n",
    ));
    install_test_script(
        fixture.repo.join("hook.sh"),
        r#"#!/bin/sh
set -eu
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
test -f tracked
test "$SESSION" = "$SHOAL_WORKSPACE"
test "$PROFILE" = "$SHOAL_WORKSPACE_PATH/profile"
printf '%s\n' "$SHOAL_HOOK" >> "$HOME/hooks"
"#,
    );
    commit_resource_config(&fixture.repo, "");
    let workspace = fixture.add("global-hooks");
    assert_eq!(workspace["state"], "ready");
    let markers = fixture.root.path().join("hooks");
    assert_eq!(fs::read_to_string(&markers).unwrap(), "post_setup\n");
    fixture.ok(&["rm", "global-hooks", "--yes"]);
    assert!(!Path::new(workspace["path"].as_str().unwrap()).exists());
    assert_eq!(
        fs::read_to_string(&markers).unwrap(),
        "post_setup\npre_remove\n"
    );
}

#[test]
fn pre_setup_hook_gates_readiness_and_supports_global_defaults() {
    let fixture = Fixture::with_config(Some("pre_setup_cmd = 'before.sh'\n"));
    install_test_script(
        fixture.repo.join("before.sh"),
        r#"#!/bin/sh
set -eu
test "$SHOAL_HOOK" = pre_setup
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
echo before >> "$HOME/setup-order"
test ! -f "$HOME/fail-before" || { echo 'prepare failed' >&2; exit 7; }
"#,
    );
    install_test_script(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\necho setup >> \"$HOME/setup-order\"\n",
    );
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
    fixture.ok(&["rm", "hooked", "--yes"]);
    fixture.ok(&["rm", "hook-only", "--yes"]);
}

#[test]
fn post_remove_hook_uses_checkout_and_reports_failure_after_removal() {
    let fixture = Fixture::with_config(Some(
        "post_remove_cmd = '/usr/bin/false'\n[env]\nPROFILE = '{workspace_path}/global-profile'\n",
    ));
    install_test_script(
        fixture.repo.join("after removal.sh"),
        r#"#!/bin/sh
set -eu
test "$SHOAL_HOOK" = post_remove
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
test ! -e "$SHOAL_WORKSPACE_PATH"
if [ "$SHOAL_WORKSPACE" = hooked ]; then
  test "$PROFILE" = "$SHOAL_WORKSPACE_PATH/saved-profile"
else
  test "$PROFILE" = "$SHOAL_WORKSPACE_PATH/global-profile"
fi
printf '%s\n%s\n%s\n' "$PWD" "$SHOAL_WORKSPACE" "$SHOAL_WORKSPACE_PATH" >> "$HOME/removed"
test ! -f "$HOME/fail-after" || { echo 'external cleanup failed' >&2; exit 8; }
"#,
    );
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
    assert!(!fixture.run(&["rm", "hooked", "--yes"]).status.success());
    assert!(!fixture.root.path().join("removed").exists());
    fs::write(
        &config,
        "post_remove_cmd = 'after removal.sh'\n[env]\nPROFILE = '{workspace_path}/saved-profile'\n",
    )
    .unwrap();
    fs::write(fixture.root.path().join("fail-after"), "").unwrap();
    let result = fixture.ok(&["rm", "hooked", "--yes"]);
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
    fixture.ok(&["rm", "missing", "--yes"]);
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

#[test]
fn agents_notify_the_user_about_their_own_workspace_without_completing_it() {
    let fixture = Fixture::new();
    fixture.add("sender");
    fixture.add("other");
    let output = scoped_command(&fixture, "sender", &["notify", "PR #12 is ready to merge"]);
    assert!(output.status.success(), "{output:?}");
    // A message that stopped applying is withdrawn by the number notify gave.
    let output = scoped_command(&fixture, "sender", &["notify", "tests are failing"]);
    assert!(output.status.success(), "{output:?}");
    let sent: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = sent["notification"].as_i64().unwrap().to_string();
    let output = scoped_command(&fixture, "sender", &["notifications", "withdraw", &id]);
    assert!(output.status.success(), "{output:?}");
    for args in [
        &["notifications", "withdraw", &id][..],
        &["notify", "hello", "--workspace", "other"][..],
        &["notify", "two\nlines"],
        &["notifications"],
    ] {
        let output = scoped_command(&fixture, "sender", args);
        assert!(
            !output.status.success(),
            "scoped {args:?} unexpectedly succeeded"
        );
    }
    let notifications = fixture.ok(&["notifications"]);
    let messages: Vec<_> = notifications
        .as_array()
        .unwrap()
        .iter()
        .filter(|notification| notification["kind"] == "agent_message")
        .map(|notification| (&notification["workspace"], &notification["message"]))
        .collect();
    assert_eq!(
        messages,
        [(
            &serde_json::json!("sender"),
            &serde_json::json!("PR #12 is ready to merge")
        )]
    );
    assert!(fixture.ok(&["inspect", "sender"])["completion"].is_null());
}

#[test]
fn notifications_kept_unread_stay_new() {
    let fixture = Fixture::new();
    fixture.add("sender");
    for message in ["first", "second"] {
        let output = scoped_command(&fixture, "sender", &["notify", message]);
        assert!(output.status.success(), "{output:?}");
    }
    let unread = || fixture.ok(&["daemon", "status"])["daemon"]["unread_notifications"].clone();
    let messages = |notifications: &[Value]| -> Vec<(i64, String)> {
        notifications
            .iter()
            .filter(|notification| notification["kind"] == "agent_message")
            .map(|notification| {
                (
                    notification["id"].as_i64().unwrap(),
                    notification["message"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let before = unread();
    let listed = fixture.ok(&["notifications", "--keep-unread"]);
    let listed = messages(listed.as_array().unwrap());
    let texts: Vec<_> = listed.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(texts, ["first", "second"]);
    assert_eq!(unread(), before);
    // A limited listing counts what it printed among the ones remaining.
    let limited = fixture.run(&["notifications", "--keep-unread", "--limit", "1"]);
    let remaining = before.as_u64().unwrap() - 1;
    assert_eq!(
        String::from_utf8_lossy(&limited.stderr),
        format!(
            "{remaining} more new notification{}; run shoal notifications again\n",
            if remaining == 1 { "" } else { "s" }
        )
    );

    // A follower resuming after the first sees only the second, and keeps it unread.
    let mut follower = fixture
        .command()
        .args(["--json", "notifications", "--follow", "--keep-unread"])
        .args(["--resume", &listed[0].0.to_string()])
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
    let followed = loop {
        let line = lines
            .recv_timeout(Duration::from_secs(60))
            .expect("followed notification");
        let notification: Value = serde_json::from_str(&line).unwrap();
        if let [message] = &messages(&[notification])[..] {
            break message.clone();
        }
    };
    follower.kill().unwrap();
    follower.wait().unwrap();
    assert_eq!(followed, listed[1]);
    assert_eq!(unread(), before);

    let read = fixture.ok(&["notifications"]);
    assert_eq!(messages(read.as_array().unwrap()), listed);
    assert_eq!(fixture.ok(&["notifications"]), serde_json::json!([]));
}

#[test]
fn the_wake_hook_exits_with_its_agent() {
    let fixture = Fixture::new();
    let workspace = fixture.add("waking");
    let path = Path::new(workspace["path"].as_str().unwrap());
    // The sleep stands in for the agent; the hook would otherwise wait for an
    // update that never comes.
    let mut agent = Command::new("sleep").arg("600").spawn().unwrap();
    let mut hook = fixture
        .command()
        .current_dir(path)
        .args(["internal", "wake", "--agent", &agent.id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    agent.kill().unwrap();
    agent.wait().unwrap();
    // A hang guard only: the hook checks its agent every second.
    wait_until("the wake hook to exit", || {
        hook.try_wait().unwrap().is_some()
    });
    let output = hook.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

#[test]
fn the_user_messages_agents_who_read_their_own_messages_once() {
    let fixture = Fixture::new();
    fixture.add("receiver");
    fixture.add("other");
    fixture.ok(&["message", "Stop the dev server", "--workspace", "receiver"]);
    let output = scoped_command(
        &fixture,
        "receiver",
        &["message", "hello", "--workspace", "receiver"],
    );
    assert!(!output.status.success(), "agents cannot send: {output:?}");
    let output = scoped_command(&fixture, "receiver", &["messages", "other"]);
    assert!(!output.status.success(), "{output:?}");
    let output = scoped_command(&fixture, "receiver", &["messages"]);
    assert!(output.status.success(), "{output:?}");
    let messages: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(messages[0]["message"], "Stop the dev server");
    assert_eq!(messages.as_array().unwrap().len(), 1);
    assert_eq!(fixture.ok(&["messages", "receiver"]), serde_json::json!([]));

    // Agent hooks print nothing without messages, then deliver new ones as
    // context for the event they answer.
    let hook = |stdin: &str| {
        let mut child = fixture
            .command()
            .args(["exec", "receiver", "--", env!("CARGO_BIN_EXE_shoal")])
            .args(["messages", "--hook"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let event = r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#;
    assert_eq!(hook(event), "");
    fixture.ok(&["message", "Free disk space", "--workspace", "receiver"]);
    let output: Value = serde_json::from_str(&hook(event)).unwrap();
    assert_eq!(
        output["hookSpecificOutput"],
        serde_json::json!({
            "hookEventName": "PostToolUse",
            "additionalContext": "New Shoal messages for this workspace:\n- Free disk space"
        })
    );
    assert_eq!(hook(event), "");

    // The wake hook prints the next update for the agent and exits 2, which
    // makes Claude Code start the idle agent's next turn with it.
    fixture.ok(&["message", "Rebase onto main", "--workspace", "receiver"]);
    let output = fixture
        .command()
        .args(["exec", "receiver", "--", env!("CARGO_BIN_EXE_shoal")])
        .args([
            "internal",
            "wake",
            "--agent",
            &std::process::id().to_string(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "New Shoal messages for this workspace:\n- Rebase onto main\n"
    );
    assert_eq!(hook(event), "");
}

#[test]
fn agents_report_their_turn_state_until_their_execution_ends() {
    let fixture = Fixture::new();
    fixture.add("reporter");
    fixture.add("other");
    let output = scoped_command(
        &fixture,
        "reporter",
        &["internal", "agent-state", "waiting", "--workspace", "other"],
    );
    assert!(!output.status.success(), "{output:?}");
    // The execution reports, then lists itself while its state is current.
    let output = fixture
        .command()
        .args(["exec", "reporter", "--", "sh", "-c"])
        .arg(r#""$0" internal agent-state waiting >/dev/null && "$0" work && "$0" status"#)
        .arg(env!("CARGO_BIN_EXE_shoal"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let shown = String::from_utf8(output.stdout).unwrap();
    assert!(
        shown
            .lines()
            .any(|line| line.contains("reporter") && line.contains("needs input")),
        "{shown}"
    );
    assert!(shown.contains("Agent state:   needs input"), "{shown}");
    assert!(
        fixture.ok(&["status", "reporter"])["workspace"]
            .get("agent_state")
            .is_none()
    );
    let events = fixture
        .command()
        .args(["--json", "internal", "events"])
        .output()
        .unwrap();
    assert!(events.status.success());
    let states: Vec<Value> = String::from_utf8(events.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["kind"] == "agent_state")
        .map(|event| {
            assert_eq!(event["name"], "reporter");
            event["agent_state"]["state"].clone()
        })
        .collect();
    assert_eq!(states, [Value::from("waiting"), Value::Null]);
}

#[test]
fn agent_state_reports_from_an_input_that_never_closes() {
    let fixture = Fixture::new();
    fixture.add("reporter");
    let mut child = fixture
        .command()
        .args([
            "internal",
            "agent-state",
            "waiting",
            "--workspace",
            "reporter",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Held open and silent, like a pipe or socket a script inherited.
    let stdin = child.stdin.take();
    let status = try_wait_for(|| child.try_wait().unwrap());
    if status.is_none() {
        let _ = child.kill();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(status.is_some_and(|status| status.success()), "{output:?}");
    assert_eq!(
        fixture.ok(&["status", "reporter"])["workspace"]["agent_state"]["state"],
        "waiting"
    );
}

#[test]
fn agents_mark_their_workspace_ready_for_review_until_new_commits() {
    let fixture = Fixture::new();
    let added = fixture.add("reviewed");
    fixture.add("other");
    let path = Path::new(added["path"].as_str().unwrap());
    let output = scoped_command(&fixture, "reviewed", &["ready"]);
    assert!(output.status.success(), "{output:?}");
    let marks: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(marks[0]["url"].is_null() && marks[0]["stale"] == false);
    for args in [
        &["ready", "--workspace", "other"][..],
        &["ready", "pr"],
        &["unready", "pr", "12"],
    ] {
        let output = scoped_command(&fixture, "reviewed", args);
        assert!(
            !output.status.success(),
            "scoped {args:?} unexpectedly succeeded"
        );
    }
    let row = |fixture: &Fixture| {
        let output = fixture.run(&["work"]);
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .find(|line| line.contains("reviewed"))
            .unwrap()
            .to_owned()
    };
    let inbox = |fixture: &Fixture| String::from_utf8(fixture.run(&["inbox"]).stdout).unwrap();
    assert!(row(&fixture).contains("ready"));
    assert!(!row(&fixture).contains("outdated"));
    assert_eq!(inbox(&fixture), "ready  reviewed\n");
    let listed = fixture.ok(&["inbox"]);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["kind"], "ready");
    assert_eq!(listed[0]["workspace"]["name"], "reviewed");

    fs::write(path.join("later"), "later\n").unwrap();
    git(path, &["add", "later"]);
    git(
        path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "later",
        ],
    );
    // An outdated mark no longer needs the user.
    assert!(row(&fixture).contains("ready (outdated)"));
    assert_eq!(inbox(&fixture), "Nothing needs you\n");
    assert_eq!(
        fixture.ok(&["status", "reviewed"])["workspace"]["review"][0]["stale"],
        true
    );

    let output = scoped_command(&fixture, "reviewed", &["unready"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(!row(&fixture).contains("ready"));
    assert_eq!(fixture.ok(&["inbox"]), serde_json::json!([]));
    assert!(
        fixture.ok(&["inspect", "reviewed"])["workspace"]
            .get("review")
            .is_none()
    );
}

/// Agent arguments without the hooks that deliver agent messages, which
/// built-in launches register.
fn without_message_hooks<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut remaining = Vec::new();
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        match (arg, args.clone().next()) {
            ("--settings", Some(value)) if value.contains("messages --hook") => {}
            ("-c", Some(value)) if value.starts_with("hooks.") => {}
            _ => {
                remaining.push(arg);
                continue;
            }
        }
        args.next();
    }
    remaining
}

/// Captured agent arguments, one per `separator`, without message hooks or
/// arguments that carry only Shoal's built-in guidance.
fn strip_shoal_arguments(text: &str, separator: char) -> String {
    let text = strip_guidance(text, separator);
    without_message_hooks(&text.split(separator).collect::<Vec<_>>()).join(&separator.to_string())
}

/// Every agent prompt starts with Shoal's guidance, which spans these lines.
const GUIDANCE_START: &str = "You are working in Shoal workspace ";
const GUIDANCE_END: &str = "Do not end your session without it.";

/// Removes each argument, with the flag that introduces it, whose whole value is
/// the guidance; guidance followed by additions stays for [`after_guidance`].
fn strip_guidance(text: &str, separator: char) -> String {
    let wrappers = [
        (format!("--append-system-prompt{separator}"), String::new()),
        (
            format!("-c{separator}developer_instructions=\"\"\"\n"),
            "\"\"\"".to_owned(),
        ),
        (String::new(), String::new()),
    ];
    let mut text = text.to_owned();
    let mut from = 0;
    while let Some(start) = text[from..].find(GUIDANCE_START).map(|start| from + start) {
        let Some(end) = text[start..]
            .find(GUIDANCE_END)
            .map(|end| start + end + GUIDANCE_END.len())
        else {
            break;
        };
        let (before, after) = (&text[..start], &text[end..]);
        let wrapper = wrappers.iter().find(|(prefix, suffix)| {
            before
                .strip_suffix(prefix.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.ends_with(separator))
                && after
                    .strip_prefix(suffix.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(separator))
        });
        match wrapper {
            Some((prefix, suffix)) => {
                let removed_end = (end + suffix.len() + separator.len_utf8()).min(text.len());
                text.replace_range(start - prefix.len()..removed_end, "");
                from = start - prefix.len();
            }
            None => from = end,
        }
    }
    text
}

#[test]
fn guidance_markers_match_the_bundled_guidance() {
    let guidance = include_str!("../src/config/templates/agent.md").trim_end();
    assert!(guidance.starts_with(GUIDANCE_START));
    assert!(guidance.ends_with(GUIDANCE_END));
}

/// The additions that follow Shoal's guidance in a prompt.
fn after_guidance(prompt: &str) -> String {
    assert!(prompt.starts_with(GUIDANCE_START), "{prompt}");
    remove_guidance(prompt)
}

/// `text` with each copy of the guidance and the blank line after it removed.
fn remove_guidance(text: &str) -> String {
    let mut text = text.to_owned();
    while let Some(start) = text.find(GUIDANCE_START) {
        let Some(end) = text[start..].find(GUIDANCE_END) else {
            break;
        };
        let mut end = start + end + GUIDANCE_END.len();
        if text[end..].starts_with("\n\n") {
            end += 2;
        }
        text.replace_range(start..end, "");
    }
    text
}

#[test]
fn agents_withdraw_questions_they_no_longer_need_answered() {
    let fixture = Fixture::new();
    fixture.add("asker");
    let output = scoped_command(
        &fixture,
        "asker",
        &["ask", "Which database?", "--recommend", "SQLite"],
    );
    assert!(output.status.success(), "{output:?}");
    let question: Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = question["id"].to_string();
    let output = scoped_command(&fixture, "asker", &["questions", "withdraw", &id]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fixture.ok(&["questions"]),
        serde_json::json!({"questions": [], "omitted": 0})
    );
    let refused = fixture.run(&["answer", &id, "Postgres"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("was withdrawn"),
        "{refused:?}"
    );
}

#[test]
fn agents_wait_for_the_answer_to_their_question() {
    let fixture = Fixture::new();
    fixture.add("asker");
    // Unscoped, so the workspace can be renamed while it waits.
    let asker = fixture
        .command()
        .args(["--json", "ask", "Which database?", "--recommend", "SQLite"])
        .args(["--choice", "SQLite", "--choice", "Postgres"])
        .args(["--workspace", "asker", "--wait"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let question = wait_for("the question", || {
        fixture.ok(&["questions"])["questions"]
            .as_array()
            .unwrap()
            .first()
            .cloned()
    });
    assert_eq!(question["workspace"], "asker");
    assert_eq!(question["recommendation"], "SQLite");
    let id = question["id"].to_string();

    let output = scoped_command(&fixture, "asker", &["answer", &id, "1"]);
    assert!(!output.status.success(), "agents cannot answer: {output:?}");
    let renamed = fixture.rename("asker", "renamed");
    assert_eq!(renamed["data"]["name"], "renamed", "{renamed}");
    fixture.ok(&["answer", &id, "2"]);
    let output = asker.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let answered: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answered["answer"], "Postgres");
    // The wait printed the answer, so its message is not delivered again.
    assert_eq!(fixture.ok(&["messages", "renamed"]), serde_json::json!([]));
    assert_eq!(
        fixture.ok(&["questions"]),
        serde_json::json!({"questions": [], "omitted": 0})
    );

    let output = scoped_command(
        &fixture,
        "renamed",
        &["ask", "Which cache?", "--recommend", "None"],
    );
    assert!(output.status.success(), "{output:?}");
    let open: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(open["answer"], Value::Null);
    // The delivered answer freed the first question's number.
    assert_eq!(open["id"], question["id"]);
    assert_eq!(
        fixture.ok(&["questions"])["questions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // An upgraded release resumes a wait by the recorded number and the
    // question's text without asking again.
    let workspace = open["workspace_id"].as_str().unwrap();
    let id = open["id"].to_string();
    let resumed = fixture
        .command()
        .args([
            "--json",
            "ask",
            "Which cache?",
            "--recommend",
            "None",
            "--wait",
        ])
        .args(["--workspace", workspace, "--resume", &id])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    fixture.ok(&["answer", &id, "Redis"]);
    let output = resumed.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let answered: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answered["answer"], "Redis");
    assert_eq!(
        fixture.ok(&["questions"]),
        serde_json::json!({"questions": [], "omitted": 0})
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

#[test]
fn repository_resources_return_checkouts_and_preserve_them_on_cleanup() {
    let mut fixture = Fixture::new();
    let related = init_repo(
        fixture.root.path(),
        "server",
        &[("DESIGN.MD", "server design\n")],
    );
    let registration = fixture.ok(&["repo", "add", related.to_str().unwrap(), "--name", "server"]);
    set_repository_toml(&fixture, "[resources.server]\nkind='repo'\nrepo='server'\n");
    fixture.add("consumer");
    fixture.add("other-consumer");
    assert!(
        fixture.ok(&["resource", "consumer"])["leases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let output = scoped_command(&fixture, "consumer", &["resource", "acquire", "server"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lease: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(lease["mode"], "read");
    assert_eq!(lease["repository"]["id"], registration["id"]);
    assert_eq!(lease["repository"]["path"], related.to_str().unwrap());
    for args in [
        vec!["resource", "acquire", "server", "consumer"],
        vec!["resource", "consumer"],
        vec!["inspect", "consumer"],
    ] {
        let rendered = fixture.run(&args);
        assert!(rendered.status.success());
        assert!(String::from_utf8_lossy(&rendered.stdout).contains(related.to_str().unwrap()));
    }
    assert_eq!(
        fs::read_to_string(related.join("DESIGN.MD")).unwrap(),
        "server design\n"
    );
    fixture.ok(&["resource", "acquire", "server", "other-consumer"]);
    let overview = fixture.ok(&["resource", "consumer"]);
    assert_eq!(overview["pools"][0]["used"], 1);
    assert_eq!(overview["pools"][0]["resources"][0]["readers"], 2);
    assert_eq!(
        overview["pools"][0]["resources"][0]["write_available"],
        false
    );
    let write = fixture.run(&[
        "resource", "acquire", "server", "consumer", "--mode", "write", "--name", "writer",
    ]);
    assert!(!write.status.success());
    fixture.restart();
    assert_eq!(
        fixture.ok(&["resource", "acquire", "server", "consumer"]),
        lease
    );
    let removal = fixture.run(&["repo", "rm", "server", "--yes"]);
    assert!(!removal.status.success());
    assert!(String::from_utf8_lossy(&removal.stderr).contains("borrowed"));
    assert!(related.join("DESIGN.MD").exists());
    // Definition edits do not change the recorded checkout on renewal or release.
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='missing'\n",
    );
    assert_eq!(
        fixture.ok(&["resource", "acquire", "server", "consumer"]),
        lease
    );
    set_repository_toml(&fixture, "[resources.server]\nkind='repo'\nrepo='server'\n");
    let before = git(&related, &["status", "--porcelain"]);
    let head = git(&related, &["rev-parse", "HEAD"]);
    let worktrees = git(&related, &["worktree", "list", "--porcelain"]);
    fixture.ok(&["rm", "consumer", "--yes"]);
    assert_eq!(git(&related, &["status", "--porcelain"]), before);
    assert_eq!(git(&related, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(&related, &["worktree", "list", "--porcelain"]),
        worktrees
    );
    fixture.ok(&["resource", "release", "server", "other-consumer"]);
    fixture.ok(&["repo", "rm", "server", "--yes"]);
    assert!(!related.exists());
}

#[test]
fn acquire_picks_an_omitted_resource_of_its_kind() {
    let fixture = Fixture::with_config(Some("[resources.lock]\n[resources.workers]\ncapacity=2\n"));
    let related = init_repo(fixture.root.path(), "server", &[("tracked", "server\n")]);
    fixture.ok(&["repo", "add", related.to_str().unwrap(), "--name", "server"]);
    set_repository_toml(&fixture, "[resources.server]\nkind='repo'\nrepo='server'\n");
    fixture.add("consumer");
    let args = ["acquire", "resource", "--workspace", "consumer"];
    let (output, rows) = fixture.pick(&args, "workers", "");
    assert!(output.status.success(), "{output:?}");
    assert!(
        rows.contains("lock\t") && rows.contains("workers\t"),
        "{rows:?}"
    );
    assert!(!rows.contains("server\t"), "{rows:?}");
    let (output, rows) = fixture.pick(
        &["acquire", "repo", "--workspace", "consumer"],
        "server",
        "",
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rows.lines().count(), 1, "{rows:?}");
    let pools: Vec<_> = fixture.ok(&["inspect", "consumer"])["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|lease| lease["pool"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(pools.len(), 2, "{pools:?}");
    assert!(pools.contains(&"workers".into()) && pools.contains(&"server".into()));
}

#[test]
fn acquire_port_picks_an_omitted_configured_port() {
    let fixture = Fixture::new();
    fixture.add("server");
    let none = fixture.run(&["acquire", "port", "--workspace", "server"]);
    assert!(String::from_utf8_lossy(&none.stderr).contains("no configured ports"));
    set_repository_toml(&fixture, "[ports.web]\n[ports.api]\n");
    let (output, rows) = fixture.pick(&["acquire", "port", "--workspace", "server"], "web", "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains("api\t") && rows.contains("web\t"), "{rows:?}");
    let reserved = fixture.ok(&["leases", "port", "--workspace", "server"])["reserved"].clone();
    assert_eq!(reserved.as_array().unwrap().len(), 1, "{reserved}");
    assert_eq!(reserved[0]["name"], "web");
}

#[test]
fn repository_resources_select_registrations_by_remote_url() {
    let fixture = Fixture::new();
    let related = init_repo(fixture.root.path(), "server", &[("tracked", "server\n")]);
    git(
        &related,
        &[
            "remote",
            "add",
            "origin",
            "https://example.test/team/server.git",
        ],
    );
    let registration = fixture.ok(&["repo", "add", related.to_str().unwrap(), "--name", "api"]);
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='git@example.test:team/server.git'\n",
    );
    fixture.add("consumer");
    let lease = fixture.ok(&["acquire", "repo", "server", "--workspace", "consumer"]);
    assert_eq!(lease["repository"]["id"], registration["id"]);
    assert_eq!(lease["repository"]["path"], related.to_str().unwrap());
    let repos = fixture.ok(&["leases", "repo", "--workspace", "consumer"]);
    assert_eq!(repos["pools"][0]["name"], "server");
    assert_eq!(repos["leases"][0]["id"], lease["id"]);
    fixture.ok(&["resource", "release", "server", "consumer"]);
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='https://example.test/team/other.git'\n",
    );
    let missing = fixture.run(&["resource", "acquire", "server", "consumer"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("shoal repo add"));
}

#[test]
fn repository_resources_validate_targets_before_claiming_and_bind_approvals() {
    let fixture = Fixture::new();
    let related = init_repo(fixture.root.path(), "server", &[("tracked", "server\n")]);
    fixture.ok(&["repo", "add", related.to_str().unwrap(), "--name", "server"]);
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='server'\nrequires_approval=true\n",
    );
    fixture.add("consumer");
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='missing'\n",
    );
    let missing = fixture.run(&["resource", "acquire", "server", "consumer"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("repository is not registered: missing")
    );
    assert!(
        fixture.ok(&["inspect", "consumer"])["resources"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='server'\nrequires_approval=true\n",
    );
    let moved = fixture.root.path().join("moved-server");
    fs::rename(&related, &moved).unwrap();
    let unavailable = fixture.run(&["resource", "acquire", "server", "consumer"]);
    assert!(!unavailable.status.success());
    assert!(String::from_utf8_lossy(&unavailable.stderr).contains("checkout is unavailable"));
    fs::rename(&moved, &related).unwrap();
    let pending = pending_approval(scoped_command(
        &fixture,
        "consumer",
        &[
            "resource",
            "acquire",
            "server",
            "--reason",
            "Read server contract",
        ],
    ));
    assert_eq!(
        pending["specification"]["repository"]["path"],
        related.to_str().unwrap()
    );
    fixture.ok(&["approve", &pending["id"].to_string()]);
    // Reusing the configured name for another registration cannot reuse approval.
    fixture.ok(&["repo", "rm", "server", "--yes"]);
    let replacement = init_repo(
        fixture.root.path(),
        "replacement",
        &[("tracked", "other\n")],
    );
    fixture.ok(&[
        "repo",
        "add",
        replacement.to_str().unwrap(),
        "--name",
        "server",
    ]);
    let changed = scoped_command(&fixture, "consumer", &["resource", "acquire", "server"]);
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("settings changed"));
    fixture.ok(&["resource", "release", "server", "consumer"]);
    let pending = pending_approval(scoped_command(
        &fixture,
        "consumer",
        &[
            "resource",
            "acquire",
            "server",
            "--reason",
            "Read replacement contract",
        ],
    ));
    fixture.ok(&["approve", &pending["id"].to_string()]);
    let granted = scoped_command(&fixture, "consumer", &["resource", "acquire", "server"]);
    assert!(
        granted.status.success(),
        "{}",
        String::from_utf8_lossy(&granted.stderr)
    );
    let lease: Value = serde_json::from_slice(&granted.stdout).unwrap();
    assert_eq!(lease["repository"]["path"], replacement.to_str().unwrap());
}

#[test]
fn approval_decisions_pick_an_omitted_pending_request() {
    let fixture = Fixture::with_config(Some("[resources.signing]\nrequires_approval=true\n"));
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    fixture.add("agent");
    let none = fixture.run(&["approve"]);
    assert!(String::from_utf8_lossy(&none.stderr).contains("no pending approval requests"));
    let signing = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "resource", "signing", "--reason", "sign build"],
    ));
    let web = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    let (signing, web) = (signing["id"].to_string(), web["id"].to_string());
    let (output, rows) = fixture.pick(&["approve"], &web, "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains(&format!("{signing}\t")) && rows.contains(&format!("{web}\t")));
    let (output, rows) = fixture.pick(&["deny"], &signing, "");
    assert!(output.status.success(), "{output:?}");
    assert!(!rows.contains(&format!("{web}\t")), "{rows:?}");
    let statuses: Vec<_> = fixture
        .ok(&["approvals"])
        .as_array()
        .unwrap()
        .iter()
        .map(|request| (request["id"].to_string(), request["status"].clone()))
        .collect();
    assert!(statuses.contains(&(web, "approved".into())));
    assert!(statuses.contains(&(signing, "denied".into())));
}

#[test]
fn agents_withdraw_requests_they_no_longer_need() {
    let fixture = Fixture::new();
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    fixture.add("agent");
    let web = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    let id = web["id"].to_string();
    let output = scoped_command(&fixture, "agent", &["approvals", "withdraw", &id]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["approvals"])[0]["status"], "withdrawn");
    let refused = fixture.run(&["approve", &id]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("is withdrawn"),
        "{refused:?}"
    );
}

fn pending_approval(output: Output) -> Value {
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
fn release_cancels_pending_requests_without_leases() {
    let fixture = Fixture::with_config(Some(
        "[resources.signing]\nrequires_approval=true\n[secrets.TOKEN]\ncommand=['true']\ntools=['sign']\nrequires_approval=true\n",
    ));
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    fixture.add("agent");
    for args in [
        &["acquire", "resource", "signing", "--reason", "sign build"][..],
        &["acquire", "port", "web", "--reason", "serve preview"],
        &["acquire", "secret", "TOKEN", "--reason", "sign build"],
    ] {
        pending_approval(scoped_command(&fixture, "agent", args));
    }
    assert_eq!(fixture.ok(&["approvals"]).as_array().unwrap().len(), 3);
    let released = fixture.ok(&[
        "release",
        "resource",
        "signing",
        "--name",
        "default",
        "--workspace",
        "agent",
    ]);
    assert_eq!(
        released["released"],
        serde_json::json!([{"kind": "resource", "pool": "signing", "name": "default"}])
    );
    let released = fixture.ok(&["release", "--workspace", "agent"]);
    assert_eq!(
        released["released"],
        serde_json::json!([{"kind": "port", "name": "web"}, {"kind": "secret", "name": "TOKEN"}])
    );
    assert!(
        fixture
            .ok(&["approvals"])
            .as_array()
            .unwrap()
            .iter()
            .all(|request| request["active"] == false)
    );
}

#[test]
fn resource_approvals_require_unscoped_decisions_and_preserve_capacity() {
    let mut fixture = Fixture::with_config(Some("[resources.signing]\nrequires_approval=true\n"));
    fixture.add("agent");
    fixture.add("human");
    let args = ["resource", "acquire", "signing", "--reason", "sign build"];
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
    let id = pending["id"].to_string();
    let id = id.as_str();
    assert_eq!(pending["workspace"], "agent");
    assert_eq!(
        fixture.ok(&["resource", "agent"])["pools"][0]["resources"][0]["requires_approval"],
        true
    );
    assert_eq!(
        pending_approval(scoped_command(&fixture, "agent", &args))["id"],
        pending["id"]
    );
    assert_eq!(fixture.ok(&["approvals"])[0]["id"], pending["id"]);
    assert!(
        fixture.ok(&["resource", "agent"])["leases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !scoped_command(&fixture, "agent", &["approve", id])
            .status
            .success()
    );
    assert!(
        !scoped_command(&fixture, "agent", &["deny", id])
            .status
            .success()
    );
    assert!(
        !scoped_command(&fixture, "human", &["approvals", "agent"])
            .status
            .success()
    );
    let other: Value =
        serde_json::from_slice(&scoped_command(&fixture, "human", &["approvals"]).stdout).unwrap();
    assert!(other.as_array().unwrap().is_empty());
    fixture.ok(&["resource", "acquire", "signing", "human"]);
    fixture.ok(&["approve", id]);
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
    // Release deleted the decided request, so the new one reuses its number.
    let next = pending_approval(scoped_command(&fixture, "agent", &args));
    assert_eq!(next["status"], "pending");
    assert_eq!(next["id"], pending["id"]);
    fixture.ok(&["deny", &next["id"].to_string()]);
    let denied: Value =
        serde_json::from_slice(&scoped_command(&fixture, "agent", &args).stdout).unwrap();
    assert_eq!(denied["code"], "approval_denied");
    fixture.ok(&["resource", "release", "signing", "agent"]);
    assert!(fixture.ok(&["approvals"]).as_array().unwrap().is_empty());
}

#[test]
fn resource_approvals_bind_a_member_even_when_the_pool_is_busy() {
    let fixture = Fixture::with_config(Some(
        "[resource_pools.devices]\ncapacity=1\n\
         [resource_pools.devices.resources.alpha]\nrequires_approval=true\n\
         [resource_pools.devices.resources.beta]\nrequires_approval=true\n",
    ));
    fixture.add("human");
    fixture.add("agent");
    fixture.ok(&[
        "resource",
        "acquire",
        "devices",
        "human",
        "--resource",
        "alpha",
    ]);
    let args = ["resource", "acquire", "devices", "--reason", "test device"];
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
    let id = pending["id"].to_string();
    let id = id.as_str();
    assert_eq!(pending["specification"]["member"], "alpha");
    assert_eq!(pending["specification"]["mode"], "permit");
    assert_eq!(fixture.ok(&["approvals"])[0], pending);
    let overview = fixture.ok(&["resource", "agent"]);
    assert_eq!(overview["pools"][0]["used"], 1);
    assert_eq!(overview["leases"], serde_json::json!([]));
    fixture.ok(&["approve", id]);
    let busy = scoped_command(&fixture, "agent", &args);
    assert_eq!(busy.status.code(), Some(2));
    let busy: Value = serde_json::from_slice(&busy.stdout).unwrap();
    assert_eq!(busy["code"], "resource_busy");
    assert_eq!(
        busy["message"],
        "no compatible capacity for alpha in pool devices; held by human"
    );
    let changed = scoped_command(
        &fixture,
        "agent",
        &["resource", "acquire", "devices", "--resource", "beta"],
    );
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("member changed; release it first"));
    fixture.ok(&["resource", "release", "devices", "human"]);
    let acquired = scoped_command(&fixture, "agent", &args);
    assert!(acquired.status.success());
    let lease: Value = serde_json::from_slice(&acquired.stdout).unwrap();
    assert_eq!(lease["resource"], "alpha");
    assert_eq!(fixture.ok(&["resource", "agent"])["pools"][0]["used"], 1);
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
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
    fixture.ok(&["approve", &pending["id"].to_string()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["resource", "release", "cache", "agent"]);
    fixture.restart();
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["resource", "release", "cache", "agent"]);
    let write = pending_approval(scoped_command(
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
    assert!(fixture.ok(&["approvals"]).as_array().unwrap().is_empty());
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
        let pending = pending_approval(scoped_command(&fixture, "agent", &args));
        assert!(
            fixture.ok(&["port", "agent"])["reserved"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let id = pending["id"].to_string();
        let id = id.as_str();
        fixture.ok(&["approve", id]);
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
            assert_ne!(pending_approval(next)["id"], id);
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
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
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
        pending_approval(scoped_command(&fixture, "agent", &explicit))["id"],
        pending["id"]
    );
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    assert!(
        events
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).unwrap()[0] == "list")
    );
    assert!(!fixture.root.path().join("sim-devices.json").exists());
    fixture.ok(&["approve", &pending["id"].to_string()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["sim", "release", "default", "agent"]);
    fixture.restart();
    assert!(
        scoped_command(&fixture, "agent", &explicit)
            .status
            .success()
    );
    fixture.ok(&["sim", "release", "default", "agent"]);
    let clean = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["sim", "acquire", "--clean", "--reason", "isolate app state"],
    ));
    assert_ne!(clean["id"], pending["id"]);
    fixture.ok(&["deny", &clean["id"].to_string()]);
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
            "--queue",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let id = wait_for("pending acquisition approval", || {
        let requests = fixture.ok(&["approvals"]);
        requests
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["status"] == "pending")
            .map(|request| request["id"].to_string())
    });
    fixture.ok(&["approve", &id]);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let history = fixture.ok(&["sim", "history", "agent"]);
    assert_eq!(history[0]["status"], "acquired");
    assert_eq!(
        history
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["request"]["reason"] == "reset test data")
            .count(),
        1,
        "polling must keep one clean audit request"
    );
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
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
    assert_eq!(pending["lifetime"], "lease");
    fixture.ok(&["approve", &pending["id"].to_string()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["sim", "release", "default", "agent"]);
    assert_eq!(
        pending_approval(scoped_command(&fixture, "agent", &args))["status"],
        "pending"
    );
    fixture.ok(&["sim", "release", "default", "agent"]);
}

#[test]
#[cfg(target_os = "macos")]
fn exported_scopes_request_simulator_approval_before_mutations() {
    let config = SIM_CONFIG.replace(
        "[simulators.profiles.phone]",
        "[simulators.profiles.phone]\nrequires_approval=true",
    );
    let fixture = Fixture::with_tools(Some(&config), true);
    let workspace = fixture.add("external");
    let exported = fixture.ok(&["env", "external"]);
    let values: std::collections::BTreeMap<String, String> =
        serde_json::from_value(exported).unwrap();
    let acquire = |args: &[&str]| {
        fixture
            .command()
            .envs(&values)
            .current_dir(workspace["path"].as_str().unwrap())
            .args(["--json", "sim", "acquire"])
            .args(args)
            .output()
            .unwrap()
    };
    let pending = pending_approval(acquire(&["--reason", "test app"]));
    fixture.ok(&["deny", &pending["id"].to_string()]);
    fixture.ok(&["sim", "release", "default", "external"]);
    let clean = pending_approval(acquire(&["--clean", "--reason", "isolate app state"]));
    assert_eq!(clean["specification"]["clean"], true);
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    assert!(
        events
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).unwrap()[0] == "list")
    );
    assert!(!fixture.root.path().join("sim-devices.json").exists());
}

#[test]
fn tampered_resource_approvals_fail_instead_of_selecting_a_member() {
    let fixture = Fixture::with_config(Some("[resources.signing]\nrequires_approval=true\n"));
    fixture.add("agent");
    let args = ["resource", "acquire", "signing", "--reason", "sign build"];
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
    fixture.ok(&["approve", &pending["id"].to_string()]);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let mut record = pending.clone();
    record["status"] = "approved".into();
    record["specification"] = serde_json::json!({
        "preferred": null, "env": "PORT_WEB", "on_conflict": "suggest", "range": [3000, 3100]
    });
    let tamper = |record: &Value| {
        db.execute(
            "UPDATE access_requests SET record=?2 WHERE number=?1",
            rusqlite::params![pending["id"].as_i64().unwrap(), record.to_string()],
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
    let previous = pending_approval(scoped_command(&fixture, "agent", &args));
    fs::write(fixture.root.path().join(".config/shoal/config.toml"), "").unwrap();
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join(".shoal.toml"),
        config,
    )
    .unwrap();
    fixture.restart();
    let current = pending_approval(scoped_command(&fixture, "agent", &args));
    assert_ne!(previous["target"], current["target"]);
    fixture.ok(&["approve", &current["id"].to_string()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["resource", "release", "signing", "agent"]);
    assert!(fixture.ok(&["approvals"]).as_array().unwrap().is_empty());
    assert_ne!(
        pending_approval(scoped_command(&fixture, "agent", &args))["id"],
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
    // The macOS developer-tool shim resolves the tool by its invoked basename.
    let real_bin = bin.join("real");
    fs::create_dir(&real_bin).unwrap();
    std::os::unix::fs::symlink(real_git, real_bin.join("git")).unwrap();
    install_test_script(
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
exec "$HOME/bin/real/git" "$@"
"#,
    );
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
        ("--is-ancestor", vec!["rm", "worker", "--yes"]),
        ("show-ref", vec!["rm", "worker", "--yes"]),
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
        assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
    }
}

#[test]
#[ignore = "manual release-mode measurement"]
fn benchmark_daemon_reads() {
    let fixture = Fixture::new();
    for name in ["bench0", "bench1", "bench2", "bench3"] {
        fixture.add(name);
    }
    let protocol = fixture.protocol();
    println!("method,clients,requests_per_s,p50_us,p95_us,p99_us");
    for method in ["inspect_workspace", "workspace_status"] {
        for clients in [1, 16] {
            let start = Instant::now();
            let mut latencies = thread::scope(|scope| {
                let mut tasks = Vec::new();
                for client in 0..clients {
                    let fixture = &fixture;
                    tasks.push(scope.spawn(move || {
                        let mut samples = Vec::new();
                        for _ in 0..100 {
                            let start = Instant::now();
                            let response = fixture.raw_request(
                                protocol,
                                1,
                                serde_json::json!({method: {"workspace": format!("bench{}", client % 4)}}),
                            );
                            assert_eq!(response["type"], if method == "workspace_status" { "workspace_status" } else { "inspection" }, "{response}");
                            if method == "workspace_status" {
                                assert!(response["data"]["diff_error"].is_null(), "{response}");
                            }
                            samples.push(start.elapsed().as_micros());
                        }
                        samples
                    }));
                }
                tasks
                    .into_iter()
                    .flat_map(|task| task.join().unwrap())
                    .collect::<Vec<_>>()
            });
            let elapsed = start.elapsed().as_secs_f64();
            latencies.sort_unstable();
            println!(
                "{method},{clients},{:.0},{},{},{}",
                latencies.len() as f64 / elapsed,
                latencies[latencies.len() / 2],
                latencies[latencies.len() * 95 / 100],
                latencies[latencies.len() * 99 / 100]
            );
        }
    }
}

fn set_repository_toml(fixture: &Fixture, text: &str) {
    let config = fixture.root.path().join("repository.toml");
    fs::write(&config, text).unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        config.to_str().unwrap(),
    ]);
}

fn install_done_hook(fixture: &Fixture) -> PathBuf {
    let hook = fixture.root.path().join("done hook.sh");
    install_test_script(
        &hook,
        "#!/bin/sh\nprintf '%s|%s|%s|%s|%s|%s|%s|%s|%s|%s\\n' \"$SHOAL_HOOK\" \"$SHOAL_WORKSPACE\" \"$SHOAL_WORKSPACE_ID\" \"$SHOAL_WORKSPACE_PATH\" \"$PWD\" \"$SHOAL_STATE_DIR\" \"$SHOAL_DONE_CHOICE\" \"${SHOAL_SCOPE_TOKEN-unset}\" \"${SHOAL_EXECUTION_ID-unset}\" \"${SHOAL_RESERVED_PORT_ENV-unset}\" >> \"$HOME/done-events\"\n",
    );
    hook
}

#[test]
fn agent_exit_hook_exposes_custom_agent_results_without_completing_the_assignment() {
    let fixture = Fixture::with_config(Some("post_agent_exit_cmd = '/usr/bin/false'\n"));
    let hook = fixture.root.path().join("agent-exit.sh");
    install_test_script(
        &hook,
        "#!/bin/sh\nprintf '%s|%s|%s|%s|%s|%s|%s\\n' \"$SHOAL_HOOK\" \"$SHOAL_WORKSPACE\" \"$SHOAL_AGENT\" \"$SHOAL_AGENT_EXIT_CODE\" \"$SHOAL_AGENT_EXIT_COMPLETE\" \"${SHOAL_SCOPE_TOKEN-unset}\" \"${SHOAL_DONE_CHOICE-unset}\" >> \"$HOME/agent-exits\"\n",
    );
    set_repository_toml(
        &fixture,
        &format!(
            "post_agent_exit_cmd = '{}'\n[commands]\nhelper = ['/bin/sh', '-c', 'exit 7']\n",
            hook.display()
        ),
    );
    let agent = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "agent-work",
        "--agent",
        "helper",
    ]);
    assert_eq!(agent.status.code(), Some(7), "{agent:?}");
    let events = fixture.root.path().join("agent-exits");
    wait_until("agent exit hook", || {
        fs::read_to_string(&events).is_ok_and(|events| {
            events.contains("post_agent_exit|agent-work|helper|7|true|unset|unset\n")
        })
    });
    assert_eq!(
        fs::read_to_string(&events).unwrap(),
        "post_agent_exit|agent-work|helper|7|true|unset|unset\n"
    );
    assert!(fixture.ok(&["inspect", "agent-work"])["completion"].is_null());
    assert_eq!(
        fixture.run(&["run", "helper", "agent-work"]).status.code(),
        Some(7)
    );
    assert_eq!(fs::read_to_string(&events).unwrap().lines().count(), 1);
    // Falling back to a failing global hook must preserve the agent's code.
    fixture.ok(&[
        "config",
        "unset",
        "post_agent_exit_cmd",
        "--repo",
        fixture.repo.to_str().unwrap(),
    ]);
    let agent = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "agent-work",
        "--agent",
        "helper",
    ]);
    assert_eq!(agent.status.code(), Some(7), "{agent:?}");
    wait_until("failed agent exit hook", || {
        fixture
            .ok(&["notifications", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .any(|n| {
                n["kind"] == "hook_failed"
                    && n["message"]
                        .as_str()
                        .unwrap()
                        .contains("post_agent_exit_cmd exited with 1")
            })
    });
}

#[test]
fn agent_exit_hook_can_signal_completion_without_a_post_done_hook() {
    let fixture = Fixture::new();
    let hook = fixture.root.path().join("finish-assignment");
    install_test_script(
        &hook,
        format!(
            "#!/bin/sh\nexec '{}' --json done \"$SHOAL_WORKSPACE\"\n",
            env!("CARGO_BIN_EXE_shoal")
        ),
    );
    set_repository_toml(
        &fixture,
        &format!(
            "post_agent_exit_cmd = '{}'\n[commands]\nhelper = ['/bin/sh', '-c', 'exit 7']\n[done]\ncleanup = false\n",
            hook.display()
        ),
    );
    let agent = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "agent-work",
        "--agent",
        "helper",
    ]);
    assert_eq!(agent.status.code(), Some(7), "{agent:?}");
    wait_until("completion from exit hook", || {
        fixture.ok(&["inspect", "agent-work"])["completion"]["cleanup"] == false
    });
}

#[test]
fn agent_exit_acknowledgement_does_not_wait_for_the_hook() {
    let fixture = Fixture::new();
    let hook = fixture.root.path().join("exit-hook");
    let barrier = fixture.root.path().join("release-exit");
    assert!(
        Command::new("mkfifo")
            .arg(&barrier)
            .status()
            .unwrap()
            .success()
    );
    install_test_script(
        &hook,
        "#!/bin/sh\ntouch \"$HOME/exit-started\"\nread release < \"$HOME/release-exit\"\ntouch \"$HOME/exit-finished\"\n",
    );
    set_repository_toml(
        &fixture,
        &format!(
            "post_agent_exit_cmd = '{}'\n[commands]\nhelper = ['/bin/sh', '-c', 'exit 7']\n",
            hook.display()
        ),
    );
    let mut agent = fixture
        .command()
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "agent-work",
            "--agent",
            "helper",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("agent exit hook started", || {
        fixture.root.path().join("exit-started").exists()
    });
    let mut status = None;
    wait_until("agent wrapper acknowledged", || {
        status = agent.try_wait().unwrap();
        status.is_some()
    });
    assert_eq!(status.unwrap().code(), Some(7));
    assert!(!fixture.root.path().join("exit-finished").exists());
    fs::write(&barrier, "release\n").unwrap();
    wait_until("agent exit hook finished", || {
        fixture.root.path().join("exit-finished").exists()
    });
}

#[test]
fn post_done_hook_runs_for_scoped_done_and_retained_cleanup_with_config_layers() {
    let mut fixture = Fixture::with_config(Some("post_done_cmd = '/usr/bin/false'\n"));
    let workspace = fixture.add("hooked");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let hook = install_done_hook(&fixture);
    let config = format!("post_done_cmd = '{}'\n", hook.display());
    // The worktree file overrides the global failure hook.
    fs::write(
        path.join(".shoal.toml"),
        format!("{config}[done]\ncleanup = false\n"),
    )
    .unwrap();
    let completion = fixture.ok(&[
        "exec",
        "hooked",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "--json",
        "done",
    ]);
    assert_eq!(completion["cleanup"], false);
    let events = fixture.root.path().join("done-events");
    wait_until("keep hook", || events.exists());
    let identity = format!(
        "post_done|hooked|{}|{}|{}|{}|",
        workspace["id"].as_str().unwrap(),
        path.display(),
        path.display(),
        fixture.root.path().join("state").display()
    );
    assert_eq!(
        fs::read_to_string(&events).unwrap(),
        format!("{identity}keep|unset|unset|unset\n")
    );
    // Saved repository config overrides the file and determines the default choice.
    fs::write(
        path.join(".shoal.toml"),
        "post_done_cmd = '/usr/bin/false'\n",
    )
    .unwrap();
    set_repository_toml(&fixture, &format!("{config}[done]\ncleanup=true\n"));
    // Retention must survive removal of the configuration file later in this
    // test; otherwise the cleanup sweep can win the race against the last done.
    fs::write(path.join("retain-work"), "uncommitted work\n").unwrap();
    fixture.ok(&["done", "hooked"]);
    wait_until("completion cleanup error", || {
        fixture.ok(&["inspect", "hooked"])["completion"]["error"].is_string()
    });
    assert_eq!(
        fs::read_to_string(&events).unwrap(),
        format!("{identity}keep|unset|unset|unset\n{identity}cleanup|unset|unset|unset\n")
    );
    fixture.restart();
    assert_eq!(fs::read_to_string(&events).unwrap().lines().count(), 2);
    // Failure is best effort and cannot cancel a recorded choice.
    fixture.ok(&["repo", "config", fixture.repo.to_str().unwrap(), "--clear"]);
    fs::write(path.join(".shoal.toml"), "[done]\ncleanup = false\n").unwrap();
    assert_eq!(fixture.ok(&["done", "hooked"])["cleanup"], false);
    assert!(
        fixture
            .ok(&["notifications", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "hook_failed"
                && n["message"]
                    .as_str()
                    .unwrap()
                    .contains("post_done_cmd exited with 1"))
    );
}

#[test]
fn post_done_hook_observes_persisted_completion_and_excludes_lifecycle_and_permits() {
    let fixture = Fixture::with_config(Some("[resources.device]\n"));
    fixture.add("hooked");
    let hook = install_done_hook(&fixture);
    let barrier = fixture.root.path().join("release-done");
    assert!(
        Command::new("mkfifo")
            .arg(&barrier)
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        &hook,
        "#!/bin/sh\ntouch \"$HOME/done-started\"\nread release < \"$HOME/release-done\"\n",
    )
    .unwrap();
    set_repository_toml(
        &fixture,
        &format!(
            "post_done_cmd = '{}'\nsetup_cmd = '/usr/bin/true'\n[done]\ncleanup = false\n",
            hook.display()
        ),
    );
    let mut done = fixture
        .command()
        .args(["done", "hooked"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("post-done hook started", || {
        fixture.root.path().join("done-started").exists()
    });
    assert_eq!(
        fixture.ok(&["inspect", "hooked"])["completion"]["cleanup"],
        false
    );
    for args in [
        vec!["setup", "hooked"],
        vec!["rm", "hooked", "--yes"],
        vec!["resource", "acquire", "device", "hooked"],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success(), "{args:?}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("hook finishes"),
            "{output:?}"
        );
    }
    fs::write(&barrier, "release\n").unwrap();
    assert!(done.wait().unwrap().success());
    fixture.ok(&["resource", "acquire", "device", "hooked"]);
}

#[test]
fn done_defaults_to_cleanup_and_releases_workspace_resources() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n[resources.device]\n"));
    let hook = install_done_hook(&fixture);
    let workspace = fixture.add("finished");
    set_repository_toml(&fixture, &format!("post_done_cmd = '{}'\n", hook.display()));
    fixture.ok(&["port", "acquire", "web", "finished"]);
    fixture.ok(&["resource", "acquire", "device", "finished"]);
    let mut wrapper = fixture
        .command()
        .args(["exec", "finished", "--", "sleep", "60"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "finished");
    let completion = fixture.ok(&["done", "finished"]);
    assert_eq!(completion["cleanup"], true);
    wait_removed(&fixture, "finished");
    assert!(
        fs::read_to_string(fixture.root.path().join("done-events"))
            .unwrap()
            .contains("|cleanup|")
    );
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
fn agents_follow_a_keep_default_and_can_request_cleanup() {
    let fixture = Fixture::with_config(Some("[done]\ncleanup=false\n"));
    fixture.add("keep-default");
    assert_eq!(fixture.ok(&["done", "keep-default"])["cleanup"], false);
    // Agents can explicitly clean their own workspace despite a keep default.
    fixture.run(&[
        "exec",
        "keep-default",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "done",
        "--cleanup",
    ]);
    wait_removed(&fixture, "keep-default");
}

#[test]
fn done_records_completion_without_claiming_dirty_work_is_merged() {
    let fixture = Fixture::new();
    let workspace = fixture.add("unfinished");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("work"), "retain").unwrap();
    fixture.ok(&["done", "unfinished"]);
    wait_until("completion cleanup error", || {
        fixture.ok(&["inspect", "unfinished"])["completion"]["error"].is_string()
    });
    assert_eq!(fs::read_to_string(path.join("work")).unwrap(), "retain");
    assert!(fixture.ok(&["inspect", "unfinished"])["pr_cleanup"].is_null());
}

#[test]
fn multiple_prs_clean_up_only_after_every_merge_and_honor_done_policy() {
    for cleanup in [false, true] {
        let mut fixture = Fixture::with_config(Some(&format!(
            "[cleanup.auto]\nenabled=false\n[done]\ncleanup={cleanup}\n"
        )));
        let hook = install_done_hook(&fixture);
        set_repository_toml(&fixture, &format!("post_done_cmd = '{}'\n", hook.display()));
        let workspace = fixture.add("multi");
        fixture.add_github_origin();
        let bin = fixture.root.path().join("bin");
        install_test_script(
            bin.join("gh"),
            "#!/bin/sh\ncat \"$HOME/pr-$3.json\"\necho \"$3\" >> \"$HOME/queries\"\n",
        );
        let head = git(
            Path::new(workspace["path"].as_str().unwrap()),
            &["rev-parse", "HEAD"],
        );
        let write_pr = |number: u64, state: &str, commit: &str| {
            fs::write(fixture.root.path().join(format!("pr-{number}.json")),
                serde_json::json!({"number":number,"state":state,"headRefName":"multi","commits":[{"oid":commit}]}).to_string()).unwrap();
        };
        write_pr(1, "OPEN", "earlier");
        write_pr(2, "OPEN", head.trim());
        fixture.ok(&["link", "pr", "1", "assigned", "--workspace", "multi"]);
        fixture.ok(&["link", "pr", "2", "assigned", "--workspace", "multi"]);
        fixture.ok(&["link", "pr", "1", "assigned", "--workspace", "multi"]);
        assert_eq!(
            fixture.ok(&["inspect", "multi"])["pr_cleanup"]["urls"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        // Scoped cancellation selects one PR and preserves the other.
        fixture.ok(&[
            "exec",
            "multi",
            "--",
            env!("CARGO_BIN_EXE_shoal"),
            "--json",
            "unlink",
            "pr",
            "2",
            "assigned",
        ]);
        assert_eq!(
            fixture.ok(&["inspect", "multi"])["pr_cleanup"]["url"],
            "https://github.com/team/project/pull/1"
        );
        fixture.ok(&[
            "exec",
            "multi",
            "--",
            env!("CARGO_BIN_EXE_shoal"),
            "--json",
            "link",
            "pr",
            "2",
            "assigned",
        ]);
        fixture.ok(&["done", "multi"]);
        write_pr(1, "MERGED", "earlier");
        write_pr(2, "CLOSED", head.trim());
        let root = fixture.root.path().to_owned();
        let queries = root.join("queries");
        fs::write(&queries, "").unwrap();
        fixture.restart();
        wait_until("second PR queried", || {
            fs::read_to_string(&queries)
                .unwrap()
                .lines()
                .any(|s| s == "2")
        });
        assert!(fixture.ok(&["inspect", "multi"])["pr_cleanup"]["merged_head"].is_null());
        fs::write(root.join("pr-2.json"), serde_json::json!({"number":2,"state":"MERGED","headRefName":"multi","commits":[{"oid":head.trim()}]}).to_string()).unwrap();
        fixture.restart();
        if cleanup {
            wait_removed(&fixture, "multi");
        } else {
            wait_until("confirmed merges", || {
                fixture.ok(&["inspect", "multi"])["pr_cleanup"]["merged_head"] == head.trim()
            });
            let state = fixture.ok(&["inspect", "multi"]);
            assert_eq!(state["pr_cleanup"]["merged_head"], head.trim());
            fixture.ok(&["notifications"]);
            // A changed default must not change a kept assignment.
            fs::write(
                root.join(".config/shoal/config.toml"),
                "[cleanup.auto]\nenabled=false\n[done]\ncleanup=true\n",
            )
            .unwrap();
            fixture.restart();
            assert_eq!(
                fixture.ok(&["inspect", "multi"])["completion"]["cleanup"],
                false
            );
        }
        let db = rusqlite::Connection::open(root.join("state/state.db")).unwrap();
        let count: i64 = db.query_row("SELECT count(*) FROM notifications WHERE kind='workspace_done' AND workspace='multi'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            fs::read_to_string(root.join("done-events"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}

#[test]
fn merged_base_prs_retarget_and_restack_the_workspaces_stacked_on_them() {
    for tool in ["gh", "fj"] {
        let mut fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
        let lower = fixture.add("lower");
        let repo = fixture.repo.to_str().unwrap().to_owned();
        fixture.ok(&["add", &repo, "upper", "--base", "lower"]);
        let origin = if tool == "gh" {
            "git@github.com:team/project.git"
        } else {
            "https://forge.example/team/project.git"
        };
        git(&fixture.repo, &["remote", "add", "origin", origin]);
        git(
            &fixture.repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let root = fixture.root.path().to_owned();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        // Both forges read and PATCH the PR through their API: gh api, or
        // curl with fj's token.
        install_test_script(
            bin.join("gh"),
            r#"#!/bin/sh
if [ "$1" != api ]; then cat "$HOME/pr-$3"; exit; fi
printf 'HTTP/2.0 200 OK\n\n'
if [ "$5" = GET ]; then cat "$HOME/rest-${6##*/}"
else echo "$5 $6 $(cat)" >> "$HOME/edits"; echo '{}'
fi
"#,
        );
        install_forgejo_curl(&bin);
        let fake_api = fs::read_to_string(bin.join("curl")).unwrap();
        install_test_script(
            bin.join("curl"),
            fake_api.replacen(
                "cat >/dev/null\n",
                r#"case "$*" in *PATCH*)
  printf '%s\n' "$@" > "$HOME/curl-args"; cat > "$HOME/curl-stdin"
  echo "2 main" >> "$HOME/edits"; printf '{}\n200'; exit;;
esac
cat >/dev/null
"#,
                1,
            ),
        );
        let keys = if cfg!(target_os = "macos") {
            root.join("Library/Application Support/forgejo-cli.forgejo-cli/keys.json")
        } else {
            root.join(".local/share/forgejo-cli/keys.json")
        };
        fs::create_dir_all(keys.parent().unwrap()).unwrap();
        fs::write(
            &keys,
            r#"{"hosts":{"forge.example":{"type":"Application","token":"secret123"}}}"#,
        )
        .unwrap();
        let head = git(
            Path::new(lower["path"].as_str().unwrap()),
            &["rev-parse", "HEAD"],
        );
        let head = head.trim();
        forgejo_api(
            &root,
            "repos/team/project/pulls/1/commits?limit=50&page=1",
            &serde_json::json!([{"sha": head}]),
        );
        let write_pr = |number: u64, state: &str, head_branch: &str, base: &str| {
            let mut rest = forgejo_pull(number, &state.to_lowercase(), head_branch, head);
            rest["base"]["ref"] = base.into();
            rest["head"]["repo"]["full_name"] = "team/project".into();
            forgejo_api(&root, &format!("repos/team/project/pulls/{number}"), &rest);
            fs::write(root.join(format!("rest-{number}")), rest.to_string()).unwrap();
            let cli = serde_json::json!({"number": number, "state": state.to_uppercase(),
                "title": "Stacked", "headRefName": head_branch, "baseRefName": base,
                "isCrossRepository": false, "commits": [{"oid": head}]});
            fs::write(root.join(format!("pr-{number}")), cli.to_string()).unwrap();
        };
        write_pr(1, "Open", "lower", "main");
        write_pr(2, "Open", "upper", "lower");
        fixture.ok(&["link", "pr", "1", "assigned", "--workspace", "lower"]);
        fixture.ok(&["link", "pr", "2", "assigned", "--workspace", "upper"]);
        fixture.restart();
        assert_eq!(
            fixture.ok(&["inspect", "upper"])["workspace"]["base_workspace"]["name"],
            "lower"
        );

        write_pr(1, "Merged", "lower", "main");
        fixture.restart();
        wait_until("restacked upper", || {
            fixture.ok(&["inspect", "upper"])["workspace"]["base_workspace"].is_null()
        });
        let edits = fs::read_to_string(root.join("edits")).unwrap();
        if tool == "gh" {
            assert_eq!(
                edits,
                "PATCH repos/team/project/pulls/2 {\"base\":\"main\"}\n"
            );
        } else {
            assert_eq!(edits, "2 main\n");
            let args = fs::read_to_string(root.join("curl-args")).unwrap();
            assert!(args.contains("PATCH\n"), "{args}");
            assert!(args.contains("{\"base\":\"main\"}\n"), "{args}");
            assert!(
                args.contains("https://forge.example/api/v1/repos/team/project/pulls/2\n"),
                "{args}"
            );
            assert!(!args.contains("secret123"), "{args}");
            let config = fs::read_to_string(root.join("curl-stdin")).unwrap();
            assert_eq!(config, "header = \"Authorization: token secret123\"\n");
        }
        let updates = fixture.ok(&["watch", "pr", "--workspace", "upper", "--timeout", "1"]);
        let update = &updates["updates"][0];
        assert_eq!(update["kind"], "base_merged");
        let message = update["message"].as_str().unwrap();
        assert!(
            message.contains("Shoal retargeted this PR to main"),
            "{message}"
        );
        assert!(
            message.contains(&format!("git rebase --onto origin/main {head}")),
            "{message}"
        );
        let events = fixture
            .command()
            .args(["--json", "internal", "events"])
            .output()
            .unwrap();
        let restacked = String::from_utf8(events.stdout)
            .unwrap()
            .lines()
            .any(|line| {
                let event: Value = serde_json::from_str(line).unwrap();
                event["kind"] == "base_changed"
                    && event["name"] == "upper"
                    && event["cause"] == "pr"
            });
        assert!(restacked);
    }
}

#[test]
fn failed_restacks_are_reported_even_when_pr_cleanup_is_disabled() {
    let mut fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    let lower = fixture.add("lower");
    let repo = fixture.repo.to_str().unwrap().to_owned();
    fixture.ok(&["add", &repo, "upper", "--base", "lower"]);
    fixture.add_github_origin();
    let root = fixture.root.path().to_owned();
    // Fail the lookups whose arguments contain the pattern in ~/fail.
    install_test_script(
        root.join("bin/gh"),
        "#!/bin/sh\n[ -e \"$HOME/fail\" ] && case \"$*\" in *\"$(cat \"$HOME/fail\")\"*) exit 1;; esac\ncat \"$HOME/pr-$3\"\n",
    );
    let head = git(
        Path::new(lower["path"].as_str().unwrap()),
        &["rev-parse", "HEAD"],
    );
    let write_pr = |number: u64, state: &str, branch: &str, base: &str| {
        let pr = serde_json::json!({"number": number, "state": state, "title": "Stacked",
            "headRefName": branch, "baseRefName": base, "isCrossRepository": false,
            "commits": [{"oid": head.trim()}]});
        fs::write(root.join(format!("pr-{number}")), pr.to_string()).unwrap();
    };
    write_pr(1, "OPEN", "lower", "main");
    write_pr(2, "OPEN", "upper", "lower");
    fixture.ok(&["link", "pr", "1", "assigned", "--workspace", "lower"]);
    fixture.ok(&["link", "pr", "2", "assigned", "--workspace", "upper"]);
    fs::write(
        root.join(".config/shoal/config.toml"),
        "[cleanup.auto]\nenabled=false\n[cleanup.pr]\nenabled=false\n",
    )
    .unwrap();
    // An unknown merge holds the base back and records why, without alerts.
    fs::write(root.join("fail"), "commits").unwrap();
    fixture.restart();
    wait_until("recorded merge lookup failure", || {
        fixture.ok(&["inspect", "lower"])["pr_cleanup"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("could not check whether"))
    });
    assert!(
        fixture
            .ok(&["notifications"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    // A merge that left out the base's HEAD keeps the stack, since rebasing
    // onto it would drop the missing commits.
    fs::remove_file(root.join("fail")).unwrap();
    let pr = serde_json::json!({"number": 1, "state": "MERGED", "headRefName": "lower",
        "commits": [{"oid": "0".repeat(40)}]});
    fs::write(root.join("pr-1"), pr.to_string()).unwrap();
    fixture.restart();
    wait_until("recorded unmerged base HEAD", || {
        fixture.ok(&["inspect", "lower"])["pr_cleanup"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("do not contain the base workspace's HEAD"))
    });
    assert_eq!(
        fixture.ok(&["inspect", "upper"])["workspace"]["base_workspace"]["name"],
        "lower"
    );
    // A failure after the merge is confirmed is reported.
    write_pr(1, "MERGED", "lower", "main");
    fs::write(root.join("fail"), "baseRefName").unwrap();
    fixture.restart();
    wait_until("reported restack failure", || {
        fixture.ok(&["inspect", "lower"])["pr_cleanup"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("could not move the workspaces stacked on it"))
    });
    assert_eq!(
        fixture.ok(&["inspect", "upper"])["workspace"]["base_workspace"]["name"],
        "lower"
    );
    // Recovery restacks and clears the recorded error.
    fs::remove_file(root.join("fail")).unwrap();
    fixture.restart();
    wait_until("restack after recovery", || {
        let lower = fixture.ok(&["inspect", "lower"]);
        lower["pr_cleanup"]["error"].is_null()
            && fixture.ok(&["inspect", "upper"])["workspace"]["base_workspace"].is_null()
    });
}

fn issue_fixture(tool: &str) -> Fixture {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    git(&fixture.repo, &["config", "protocol.allow", "never"]);
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let host = if tool == "gh" {
        "github.com"
    } else {
        "forge.example"
    };
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://{host}/team/repo"),
        ],
    );
    let bin = fixture.root.path().join("bin");
    if tool == "gh" {
        install_test_script(bin.join(tool), "#!/bin/sh\ncat \"$HOME/issue-response\"\n");
    } else {
        install_forgejo_curl(&bin);
    }
    write_issue_state(&fixture, tool, "Open");
    fixture.ok(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "issue-work",
        "--issue",
        "316",
        "--base",
        "HEAD",
    ]);
    fixture
}

fn write_issue_state(fixture: &Fixture, tool: &str, state: &str) {
    if tool == "gh" {
        let response = serde_json::json!({"number":316,"title":"Issue","body":"context",
            "state":state.to_uppercase()});
        fs::write(
            fixture.root.path().join("issue-response"),
            response.to_string(),
        )
        .unwrap();
    } else {
        let response = serde_json::json!({"number":316,"title":"Issue","body":"context",
            "state":state.to_lowercase(),"labels":[],"pull_request":null});
        forgejo_api(fixture.root.path(), "repos/team/repo/issues/316", &response);
    }
}

#[test]
fn already_closed_issues_are_rejected_before_creating_workspaces() {
    for tool in ["gh", "fj"] {
        let fixture = issue_fixture(tool);
        write_issue_state(&fixture, tool, "Closed");
        let before = fixture.ok(&["work"]);
        let output = fixture.run(&[
            "add",
            fixture.repo.to_str().unwrap(),
            "closed-work",
            "--issue",
            "316",
            "--base",
            "HEAD",
        ]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("issue is already closed"));
        assert_eq!(fixture.ok(&["work"]), before);
    }
}

/// Runs a session's command in place of a zmx session, recording each call
/// and serving the sessions listed in `$HOME/zmx-sessions`. A detached
/// session keeps the environment it was created with and runs sent text as
/// its shell would, logging the output to `$HOME/zmx-<name>.log`.
const PASSTHROUGH_ZMX: &str = r#"#!/usr/bin/env python3
import json, os, subprocess, sys
from pathlib import Path
root = Path(os.environ['HOME'])
args = sys.argv[1:]
with (root / 'zmx-calls').open('a') as calls:
    calls.write(json.dumps(args) + '\n')
if args[:1] == ['set']:
    pass
elif args[:1] == ['run']:
    (root / f'zmx-{args[1]}.env').write_text(json.dumps(dict(os.environ)))
elif args[:1] == ['send'] and not (root / f'zmx-{args[1]}.env').exists():
    # An attached session's agent reads what is typed; record that it was.
    (root / 'zmx-sent').touch()
elif args[:1] == ['send']:
    env = json.loads((root / f'zmx-{args[1]}.env').read_text())
    env['ZMX_SESSION'] = args[1]
    with (root / f'zmx-{args[1]}.log').open('a') as log:
        subprocess.run(['sh', '-c', args[2].rstrip('\r')], env=env,
            stdin=subprocess.DEVNULL, stdout=log, stderr=log)
elif args[:1] == ['list']:
    sessions = root / 'zmx-sessions'
    print(sessions.read_text() if sessions.exists() else 'no sessions found')
elif args[:1] == ['attach']:
    args = args[1:]
    if args[:1] == ['--labels']:
        args = args[2:]
    if len(args) > 1:
        os.environ['ZMX_SESSION'] = args[0]
        if os.environ.get('ZMX_TEST_LEAK_INPUT_MODE'):
            import subprocess
            status = subprocess.run(args[1:]).returncode
            with open('/dev/tty', 'wb', buffering=0) as terminal:
                terminal.write(b'\x1b[?1000h')
            sys.exit(status)
        os.execvp(args[1], args[1:])
else:
    sys.exit(1)
"#;

fn zmx_calls(fixture: &Fixture) -> Vec<Vec<String>> {
    fs::read_to_string(fixture.root.path().join("zmx-calls"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Agent launches need their executables on PATH, which interactive commands
/// limit to the fixture's `bin`.
fn install_fake_agents(fixture: &Fixture, agents: &[&str]) {
    for agent in agents {
        install_test_script(fixture.root.path().join("bin").join(agent), "#!/bin/sh\n");
    }
}

fn install_fake_herdr(fixture: &Fixture) {
    install_test_script(
        fixture.root.path().join("bin/herdr"),
        r#"#!/usr/bin/env python3
import json, os, shlex, sys
from pathlib import Path
root = Path(os.environ['HOME'])
args = sys.argv[1:]
# Tab watchers poll `tab get`; keep those reads out of the recorded actions.
log = 'herdr-tab-gets' if args[:2] == ['tab', 'get'] else 'herdr-calls'
with (root / log).open('a') as calls:
    calls.write(json.dumps(args) + '\n')
if args[:2] == ['tab', 'create']:
    print(json.dumps({'result': {'tab': {'tab_id': 'w1:t9'}, 'root_pane': {'pane_id': 'w1:p9'}}}))
elif args[:2] == ['pane', 'run']:
    (root / 'herdr-worker').write_text(json.dumps(shlex.split(args[3])))
elif args[:2] == ['tab', 'rename'] and (root / 'fail-rename').exists():
    sys.exit('rename-failed')
elif args[:2] == ['tab', 'get']:
    closed = root / 'closed-tabs'
    if closed.exists() and args[2] in closed.read_text().split():
        print(json.dumps({'error': {'code': 'tab_not_found'}}))
        sys.exit(1)
    if (root / 'herdr-unreachable').exists():
        print(json.dumps({'error': {'code': 'server_not_running'}}))
        sys.exit(1)
    print(json.dumps({'result': {'tab_id': args[2]}}))
elif args[:2] == ['pane', 'process-info']:
    idle = root / 'idle-panes'
    at_prompt = idle.exists() and args[3] in idle.read_text().split()
    processes = {'shell_pid': 1, 'foreground_process_group_id': 1 if at_prompt else 2}
    print(json.dumps({'result': {'process_info': processes}}))
print('{}') if args[:2] not in (['tab', 'create'], ['pane', 'process-info']) else None
"#,
    );
}

fn herdr_call(fixture: &Fixture, args: &[&str], answer: &str) -> (Output, String) {
    fixture.interactive_command(
        fixture
            .command()
            .args(args)
            .env("HERDR_ENV", "1")
            .env("HERDR_WORKSPACE_ID", "w1")
            .env("HERDR_TAB_ID", "w1:t1")
            .env("HERDR_PANE_ID", "w1:p1"),
        answer,
    )
}

fn herdr_worker(fixture: &Fixture) -> Vec<String> {
    serde_json::from_slice(&fs::read(fixture.root.path().join("herdr-worker")).unwrap()).unwrap()
}

/// The worker Herdr runs in the created tab, with that tab's environment.
fn herdr_worker_command(fixture: &Fixture) -> Command {
    let mut command = fixture.command();
    command
        .env_remove("SHOAL_STATE_DIR")
        .args(&herdr_worker(fixture)[1..])
        .env("HERDR_TAB_ID", "w1:t9");
    let calls = herdr_calls(fixture);
    let create = calls
        .iter()
        .rfind(|call| call[..2] == ["tab", "create"])
        .unwrap();
    for pair in create.windows(2).filter(|pair| pair[0] == "--env") {
        let (name, value) = pair[1].split_once('=').unwrap();
        command.env(name, value);
    }
    command
}

/// The file the tab's shell follows to enter the workspace its worker added.
fn herdr_tab_directive(fixture: &Fixture) -> PathBuf {
    herdr_calls(fixture)[0]
        .iter()
        .find_map(|arg| arg.strip_prefix("SHOAL_SHELL_DIRECTIVE="))
        .map(PathBuf::from)
        .unwrap()
}

fn herdr_calls(fixture: &Fixture) -> Vec<Vec<String>> {
    fs::read_to_string(fixture.root.path().join("herdr-calls"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn herdr_handoff_preserves_choices_and_literal_arguments() {
    let fixture = Fixture::with_config(Some(
        "[commands]\npi = ['sh', '-c', 'printf \"%s\\0\" \"$@\" > \"$HOME/agent-args\"; echo \"${SHOAL_ADD_PLAN-unset}\" > \"$HOME/agent-plan\"; exit 7', 'agent', '{args}']\n",
    ));
    install_fake_herdr(&fixture);
    let directive = fixture.root.path().join("caller-directive");
    fs::write(&directive, "unchanged").unwrap();
    let args = [
        "add",
        fixture.repo.to_str().unwrap(),
        "herdr-agent",
        "--agent",
        "pi",
        "--",
        "two words",
        "quote ' ; $(touch injected)",
    ];
    let (caller, _) = fixture.interactive_command(
        fixture
            .command()
            .args(args)
            .env("HERDR_ENV", "1")
            .env("HERDR_WORKSPACE_ID", "w1")
            .env("SHOAL_SHELL_DIRECTIVE", &directive),
        "",
    );
    assert!(caller.status.success(), "{caller:?}");
    assert!(fixture.ok(&["work"]).as_array().unwrap().is_empty());
    assert_eq!(fs::read_to_string(directive).unwrap(), "unchanged");
    let calls = herdr_calls(&fixture);
    assert_eq!(&calls[0][..3], ["tab", "create", "--workspace"]);
    assert!(calls[0].iter().any(|arg| arg == "--no-focus"));
    assert_eq!(&calls[1][..3], ["pane", "run", "w1:p9"]);
    let state = fixture.root.path().join("state");
    assert_eq!(
        &herdr_worker(&fixture)[1..],
        [
            "--state-dir",
            state.to_str().unwrap(),
            "internal",
            "herdr",
            "--close-when-done"
        ]
    );
    let (agent, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert_eq!(agent.status.code(), Some(7), "{agent:?} {transcript}");
    // The tab's shell, not the caller's, enters the workspace after the agent.
    let tab_directive = herdr_tab_directive(&fixture);
    let workspace = fixture.ok(&["inspect", "herdr-agent"]);
    assert_eq!(
        fs::read_to_string(&tab_directive).unwrap(),
        format!("{}\n", workspace["workspace"]["path"].as_str().unwrap())
    );
    fs::remove_file(tab_directive).unwrap();
    assert_eq!(
        strip_shoal_arguments(
            &fs::read_to_string(fixture.root.path().join("agent-args")).unwrap(),
            '\0'
        ),
        "two words\0quote ' ; $(touch injected)\0"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("agent-plan")).unwrap(),
        "unset\n"
    );
    assert!(!fixture.root.path().join("injected").exists());
    assert_eq!(herdr_calls(&fixture).len(), 3);
}

#[test]
fn herdr_resume_all_reuses_stopped_panes_and_labels_new_tabs_as_add_does() {
    let fixture = Fixture::with_config(Some("[herdr]\ntab_name = 'resumed {branch}'\n"));
    install_fake_herdr(&fixture);
    // The first agent's pane is back at its prompt; the second's is busy.
    fs::write(fixture.root.path().join("idle-panes"), "w1:p5\n").unwrap();
    let mut expected = Vec::new();
    for (name, pane) in [("first", "w1:p5"), ("second", "w1:p6")] {
        let workspace = fixture.add(name);
        let id = workspace["id"].as_str().unwrap();
        let state = fixture.root.path().join("state/workspaces").join(id);
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join(format!("{name}-agent.recovery.json")),
            format!(r#"{{"agent":"claude","place":"{pane}"}}"#),
        )
        .unwrap();
        expected.push((
            workspace["path"].as_str().unwrap().to_owned(),
            id.to_owned(),
            format!("{name}-agent"),
        ));
    }
    let (output, transcript) = herdr_call(&fixture, &["resume", "--all"], "");
    assert!(output.status.success(), "{output:?} {transcript}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Resuming 2 agents in Herdr tabs"),
        "{output:?}"
    );
    let calls = herdr_calls(&fixture);
    assert_eq!(calls.len(), 5, "{calls:?}");
    let resumed = |run: &[String], (_, id, execution): &(String, String, String)| {
        let worker = run[3].split(' ').collect::<Vec<_>>();
        assert_eq!(
            worker[worker.len() - 4..],
            ["resume", id.as_str(), "--execution", execution.as_str()]
        );
    };
    assert_eq!(calls[0][..4], ["pane", "process-info", "--pane", "w1:p5"]);
    assert_eq!(calls[1][..3], ["pane", "run", "w1:p5"]);
    resumed(&calls[1], &expected[0]);
    assert_eq!(calls[2][..4], ["pane", "process-info", "--pane", "w1:p6"]);
    let create = &calls[3];
    assert_eq!(create[..2], ["tab", "create"]);
    let cwd = create.iter().position(|arg| arg == "--cwd").unwrap();
    assert_eq!(create[cwd + 1], expected[1].0);
    let label = create.iter().position(|arg| arg == "--label").unwrap();
    assert_eq!(create[label + 1], "resumed second");
    assert!(create.iter().any(|arg| arg == "--no-focus"));
    assert_eq!(calls[4][..3], ["pane", "run", "w1:p9"]);
    resumed(&calls[4], &expected[1]);
}

#[test]
fn herdr_resume_honors_new_tab_and_enabled_settings() {
    let fixture = Fixture::with_config(Some("[herdr]\nnew_tab = false\n"));
    install_fake_herdr(&fixture);
    fs::write(fixture.root.path().join("idle-panes"), "w1:p5\n").unwrap();
    for (name, pane) in [("first", "w1:p5"), ("second", "w1:p6")] {
        let workspace = fixture.add(name);
        let state = fixture
            .root
            .path()
            .join("state/workspaces")
            .join(workspace["id"].as_str().unwrap());
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join(format!("{name}-agent.recovery.json")),
            format!(r#"{{"agent":"claude","place":"{pane}"}}"#),
        )
        .unwrap();
    }
    // Without new tabs, an idle recorded pane is still reused.
    let (output, transcript) = herdr_call(&fixture, &["resume", "--all"], "");
    assert!(output.status.success(), "{output:?} {transcript}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Resuming 1 agents in Herdr tabs"),
        "{stdout}"
    );
    assert!(stdout.contains("--execution second-agent"), "{stdout}");
    let calls = herdr_calls(&fixture);
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[1][..3], ["pane", "run", "w1:p5"]);
    assert!(calls.iter().all(|call| call[..2] != ["tab", "create"]));

    // Disabled, Herdr is never called and every agent is listed.
    fs::remove_file(fixture.root.path().join("herdr-calls")).unwrap();
    fs::write(
        fixture.root.path().join(".config/shoal/config.toml"),
        "[herdr]\nenabled = false\n",
    )
    .unwrap();
    let (output, transcript) = herdr_call(&fixture, &["resume", "--all"], "");
    assert!(output.status.success(), "{output:?} {transcript}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("Herdr tabs"), "{stdout}");
    assert!(stdout.contains("--execution first-agent"), "{stdout}");
    assert!(stdout.contains("--execution second-agent"), "{stdout}");
    assert!(!fixture.root.path().join("herdr-calls").exists());
}

#[test]
fn herdr_reports_issue_lookup_errors_before_the_agent_picker() {
    let fixture = Fixture::new();
    fixture.add_github_origin();
    install_fake_herdr(&fixture);
    install_test_script(fixture.root.path().join("bin/codex"), "#!/bin/sh\nexit 0\n");
    install_test_script(
        fixture.root.path().join("bin/gh"),
        "#!/bin/sh\necho lookup-failed >&2\nexit 1\n",
    );
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        "#!/bin/sh\necho \"$*\" >&2\nhead -n 1\n",
    );
    let (caller, transcript) = herdr_call(&fixture, &["add", "34"], "");
    assert!(!caller.status.success(), "{caller:?} {transcript}");
    assert!(transcript.contains("Repository>"), "{transcript}");
    assert!(transcript.contains("lookup-failed"), "{transcript}");
    assert!(!transcript.contains("Agent>"), "{transcript}");
    assert!(!fixture.root.path().join("herdr-calls").exists());
    assert!(fixture.ok(&["work"]).as_array().unwrap().is_empty());
}

#[test]
fn herdr_labels_name_issues_by_repository_and_number() {
    let fixture = Fixture::with_config(Some("[commands]\npi = ['sh', '-c', 'exit 0']\n"));
    fixture.add_github_origin();
    fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    install_fake_herdr(&fixture);
    install_test_script(
        fixture.root.path().join("bin/gh"),
        "#!/bin/sh\necho '{\"state\":\"OPEN\",\"number\":34,\"title\":\"Fix API timeout\",\"body\":\"Details\"}'\n",
    );
    let url = "https://github.com/team/project/issues/34";
    for (mut args, initial, renamed, fail_rename) in [
        (
            vec![
                "add",
                "34",
                "--repo",
                fixture.repo.to_str().unwrap(),
                "--agent",
                "pi",
            ],
            "project#34",
            "project#34",
            false,
        ),
        (
            vec![
                "add",
                url,
                "--repo",
                fixture.repo.to_str().unwrap(),
                "--agent",
                "pi",
            ],
            "project#34",
            "project#34",
            false,
        ),
        (
            vec![
                "add",
                fixture.repo.to_str().unwrap(),
                "fix/short",
                "--issue",
                url,
                "--agent",
                "pi",
            ],
            "project#34",
            "project#34",
            true,
        ),
        (
            vec![
                "add",
                fixture.repo.to_str().unwrap(),
                "--existing",
                "fix/short",
                "--agent",
                "pi",
            ],
            "fix/short",
            "fix/short",
            false,
        ),
    ] {
        if !args.contains(&"--existing") {
            args.extend(["--base", "HEAD"]);
        }
        let marker = fixture.root.path().join("fail-rename");
        if fail_rename {
            fs::write(&marker, "").unwrap();
        } else if marker.exists() {
            fs::remove_file(&marker).unwrap();
        }
        let (caller, transcript) = herdr_call(&fixture, &args, "");
        assert!(caller.status.success(), "{caller:?} {transcript}");
        let calls = herdr_calls(&fixture);
        let create = &calls[calls.len() - 2];
        let label = create.iter().position(|arg| arg == "--label").unwrap();
        assert_eq!(create[label + 1], initial);
        let (worker, transcript) =
            fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
        assert!(worker.status.success(), "{worker:?} {transcript}");
        assert_eq!(transcript.contains("rename-failed"), fail_rename);
        let calls = herdr_calls(&fixture);
        assert_eq!(calls.last().unwrap(), &["tab", "rename", "w1:t9", renamed]);
        if args[1] == "34" {
            fixture.ok(&["rm", "34-fix-api-timeout", "--yes"]);
            wait_for_herdr_close(&fixture);
        }
    }
}

#[test]
fn herdr_tab_name_templates_survive_handoff_and_use_the_allocated_branch() {
    let fixture = Fixture::with_config(Some(
        "[commands]\npi = ['sh', '-c', 'exit 0']\n[herdr]\ntab_name = 'global'\n",
    ));
    fixture.add_github_origin();
    fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "[herdr]\ntab_name = 'worktree'\n",
    )
    .unwrap();
    install_fake_herdr(&fixture);
    install_test_script(
        fixture.root.path().join("bin/gh"),
        "#!/bin/sh\necho '{\"state\":\"OPEN\",\"number\":34,\"title\":\"Literal {branch} $(touch injected)\",\"body\":\"Details\"}'\n",
    );
    git(&fixture.repo, &["branch", "topic"]);
    for (issue, initial, renamed) in [
        (
            Some("https://github.com/team/project/issues/34"),
            "project topic #34: Literal {branch} $(touch injected)",
            "project topic-2 #34: Literal {branch} $(touch injected)",
        ),
        // Removal deleted the first topic-2, which had no commits.
        (None, "project topic #: ", "project topic-2 #: "),
    ] {
        fixture.ok(&[
            "config",
            "set",
            "herdr.tab_name",
            "'{repo} {branch} #{issue_number}: {issue_title}'",
            "--repo",
            fixture.repo.to_str().unwrap(),
        ]);
        let mut args = vec![
            "add",
            fixture.repo.to_str().unwrap(),
            "topic",
            "--base",
            "HEAD",
            "--agent",
            "pi",
        ];
        if let Some(url) = issue {
            args.extend(["--issue", url]);
        }
        let (caller, transcript) = herdr_call(&fixture, &args, "");
        assert!(caller.status.success(), "{caller:?} {transcript}");
        let calls = herdr_calls(&fixture);
        let create = &calls[calls.len() - 2];
        let label = create.iter().position(|arg| arg == "--label").unwrap();
        assert_eq!(create[label + 1], initial);
        fixture.ok(&[
            "config",
            "set",
            "herdr.tab_name",
            "'changed after handoff'",
            "--repo",
            fixture.repo.to_str().unwrap(),
        ]);
        let (worker, transcript) =
            fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
        assert!(worker.status.success(), "{worker:?} {transcript}");
        let calls = herdr_calls(&fixture);
        assert_eq!(calls.last().unwrap(), &["tab", "rename", "w1:t9", renamed]);
        assert!(!fixture.root.path().join("injected").exists());
        fixture.ok(&["rm", "topic", "--yes"]);
        wait_for_herdr_close(&fixture);
    }
}

#[test]
fn herdr_plans_carry_issue_urls_instead_of_bodies() {
    let fixture = Fixture::new();
    fixture.add_github_origin();
    install_fake_herdr(&fixture);
    install_fake_agents(&fixture, &["codex"]);
    // Larger than one Linux argument may be, so it cannot ride in the tab's --env.
    let body = "x".repeat(256 * 1024);
    let response = fixture.root.path().join("issue-response");
    fs::write(
        &response,
        serde_json::json!({"state": "OPEN", "number": 34, "title": "Large", "body": body})
            .to_string(),
    )
    .unwrap();
    install_test_script(
        fixture.root.path().join("bin/gh"),
        format!("#!/bin/sh\ncat '{}'\n", response.display()),
    );
    let repo = fixture.repo.to_str().unwrap();
    let args = [
        "add", "34", "--repo", repo, "--agent", "codex", "--base", "HEAD",
    ];
    let (caller, transcript) = herdr_call(&fixture, &args, "");
    assert!(caller.status.success(), "{caller:?} {transcript}");
    let calls = herdr_calls(&fixture);
    let plan = calls[0]
        .iter()
        .find_map(|arg| arg.strip_prefix("SHOAL_ADD_PLAN="))
        .unwrap();
    let plan: Value = serde_json::from_str(plan).unwrap();
    assert_eq!(plan["issue"], "https://github.com/team/project/issues/34");
    // The issue prompt lets the agent start without the user.
    assert!(calls[0].iter().any(|arg| arg == "--no-focus"));
}

#[test]
fn herdr_focus_defaults_follow_the_agent_input_and_allow_overrides() {
    for (focus, agent, flag, expected) in [
        (None, None, false, "--focus"),
        (Some(false), None, false, "--no-focus"),
        (None, Some("pi"), false, "--focus"),
        (Some(true), Some("pi"), false, "--focus"),
        (Some(false), Some("pi"), false, "--no-focus"),
        (Some(false), Some("pi"), true, "--focus"),
    ] {
        let mut config = focus.map_or_else(String::new, |focus| format!("focus = {focus}\n"));
        config.push_str("[commands]\npi = ['sh', '-c', 'exit 0']\n");
        let fixture = Fixture::with_config(Some(&config));
        install_fake_herdr(&fixture);
        let mut args = vec!["add", fixture.repo.to_str().unwrap(), "focus"];
        if let Some(agent) = agent {
            args.extend(["--agent", agent]);
        }
        if flag {
            args.push("--focus");
        }
        let (caller, transcript) = herdr_call(&fixture, &args, "");
        assert!(caller.status.success(), "{caller:?} {transcript}");
        assert!(herdr_calls(&fixture)[0].iter().any(|arg| arg == expected));
    }

    let fixture = Fixture::new();
    fixture.add_github_origin();
    install_fake_herdr(&fixture);
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        "#!/bin/sh\nawk -F '\t' '$2 == \"No agent\" {print}'\n",
    );
    install_test_script(
        fixture.root.path().join("bin/gh"),
        "#!/bin/sh\necho '{\"state\":\"OPEN\",\"number\":34,\"title\":\"Focus\",\"body\":\"\"}'\n",
    );
    let (caller, transcript) = herdr_call(
        &fixture,
        &["add", "34", "--repo", fixture.repo.to_str().unwrap()],
        "",
    );
    assert!(caller.status.success(), "{caller:?} {transcript}");
    assert!(herdr_calls(&fixture)[0].iter().any(|arg| arg == "--focus"));
}

#[test]
fn herdr_no_agent_opens_workspace_shell_and_honors_focus_setting() {
    let fixture = Fixture::with_config(Some("focus = false\n"));
    install_fake_herdr(&fixture);
    let (caller, _) = herdr_call(
        &fixture,
        &["add", fixture.repo.to_str().unwrap(), "herdr-shell"],
        "",
    );
    assert!(caller.status.success());
    assert!(
        herdr_calls(&fixture)[0]
            .iter()
            .any(|arg| arg == "--no-focus")
    );
    let shell = fixture.root.path().join("bin/test-shell");
    install_test_script(&shell, "#!/bin/sh\npwd > \"$HOME/shell-cwd\"\n");
    let (worker, transcript) =
        fixture.interactive_command(herdr_worker_command(&fixture).env("SHELL", shell), "");
    assert!(worker.status.success(), "{worker:?} {transcript}");
    let workspace = fixture.ok(&["inspect", "herdr-shell"]);
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("shell-cwd"))
            .unwrap()
            .trim(),
        workspace["workspace"]["path"].as_str().unwrap()
    );
    assert_eq!(herdr_calls(&fixture).len(), 3);
    fixture.ok(&["rm", "herdr-shell", "--yes"]);
    wait_for_herdr_close(&fixture);
}

#[test]
fn herdr_opt_out_and_noninteractive_calls_stay_in_place() {
    let fixture = Fixture::new();
    install_fake_herdr(&fixture);
    for (name, mode) in [
        ("here", "here"),
        ("json", "json"),
        ("pipe", "pipe"),
        ("outside", "outside"),
    ] {
        let mut command = fixture.command();
        command.args(["add", fixture.repo.to_str().unwrap(), name]);
        if mode != "outside" {
            command
                .env("HERDR_ENV", "1")
                .env("HERDR_WORKSPACE_ID", "w1");
        }
        let output = match mode {
            "here" => fixture.interactive_command(command.arg("--here"), "").0,
            "json" => fixture.interactive_command(command.arg("--json"), "").0,
            "outside" => fixture.interactive_command(&mut command, "").0,
            _ => command.output().unwrap(),
        };
        assert!(output.status.success(), "{output:?}");
    }
    let (help, _) = herdr_call(&fixture, &["add", "--help"], "");
    assert!(help.status.success());
    assert!(!fixture.root.path().join("herdr-calls").exists());
    for (name, config) in [
        ("no-tab", "[herdr]\nnew_tab = false\n"),
        ("disabled", "[herdr]\nenabled = false\n"),
    ] {
        fs::write(fixture.repo.join(".shoal.toml"), config).unwrap();
        let (output, _) = herdr_call(&fixture, &["add", fixture.repo.to_str().unwrap(), name], "");
        assert!(output.status.success(), "{output:?}");
        assert!(!fixture.root.path().join("herdr-calls").exists());
    }
}

#[test]
fn herdr_retains_setup_failures_and_can_disable_completion_closure() {
    let fixture = Fixture::with_config(Some(
        "[herdr]\nclose_when_done = false\n[commands]\npi = ['sh', '-c', 'exit 0']\n[done]\ncleanup = false\n",
    ));
    install_fake_herdr(&fixture);
    let (caller, _) = herdr_call(
        &fixture,
        &[
            "add",
            fixture.repo.to_str().unwrap(),
            "retain",
            "--agent",
            "pi",
        ],
        "",
    );
    assert!(caller.status.success());
    assert!(
        !herdr_worker(&fixture)
            .iter()
            .any(|arg| arg == "--close-when-done")
    );
    let (worker, _) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert!(worker.status.success(), "{worker:?}");
    assert_eq!(herdr_calls(&fixture).len(), 3);
    fixture.ok(&["done", "retain"]);
    assert_eq!(herdr_calls(&fixture).len(), 3);
    // Enable closure, then fail setup before the agent can start.
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "setup_cmd = 'fail-setup'\n[herdr]\nclose_when_done = true\n",
    )
    .unwrap();
    install_test_script(fixture.repo.join("fail-setup"), "#!/bin/sh\nexit 17\n");
    git(&fixture.repo, &["add", ".shoal.toml", "fail-setup"]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "setup",
        ],
    );
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        "#!/bin/sh\ngrep 'Keep'\n",
    );
    let (caller, _) = herdr_call(
        &fixture,
        &[
            "add",
            fixture.repo.to_str().unwrap(),
            "failed-setup",
            "--agent",
            "pi",
        ],
        "",
    );
    assert!(caller.status.success());
    let (worker, _) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "\n");
    assert_eq!(worker.status.code(), Some(1));
    assert_eq!(herdr_calls(&fixture).len(), 6);
}

#[test]
fn herdr_closes_when_done_cleanup_terminates_the_agent() {
    let fixture = Fixture::with_config(Some(
        "[commands]\npi = ['sh', '-c', '\"$SHOAL_TEST_BINARY\" done --cleanup; exec sleep 600']\n",
    ));
    install_fake_herdr(&fixture);
    let (caller, _) = herdr_call(
        &fixture,
        &[
            "add",
            fixture.repo.to_str().unwrap(),
            "done-tab",
            "--agent",
            "pi",
        ],
        "",
    );
    assert!(caller.status.success());
    let (worker, transcript) = fixture.interactive_command(
        herdr_worker_command(&fixture).env("SHOAL_TEST_BINARY", env!("CARGO_BIN_EXE_shoal")),
        "",
    );
    assert_eq!(worker.status.code(), Some(143), "{worker:?} {transcript}");
    wait_for_herdr_close(&fixture);
    wait_until("done cleanup", || {
        fixture.ok(&["work"]).as_array().unwrap().is_empty()
    });
    // The closed tab's shell never reaches a prompt to consume its directive.
    let directive = herdr_tab_directive(&fixture);
    wait_until("directive removal", || !directive.exists());
}

#[test]
fn herdr_retains_detached_happy_exits_and_closes_on_done_after_restart() {
    let mut fixture =
        Fixture::with_config(Some("[done]\ncleanup = false\n[happy]\nenabled = true\n"));
    install_fake_herdr(&fixture);
    install_fake_agents(&fixture, &["claude"]);
    install_test_script(
        fixture.root.path().join("bin/happy"),
        "#!/bin/sh\nprintf done > \"$HOME/happy-exited\"\nexit 7\n",
    );
    let (caller, _) = herdr_call(
        &fixture,
        &[
            "add",
            fixture.repo.to_str().unwrap(),
            "happy-tab",
            "--agent",
            "happy-claude",
        ],
        "",
    );
    assert!(caller.status.success());
    let (worker, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert!(worker.status.success(), "{worker:?} {transcript}");
    wait_until("Happy execution exit", || {
        fixture.root.path().join("happy-exited").exists()
    });
    wait_until("Happy execution reporting", || {
        fixture.ok(&["inspect", "happy-tab"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    assert_eq!(herdr_calls(&fixture).len(), 3);
    fixture.restart();
    fixture.ok(&["done", "happy-tab"]);
    wait_for_herdr_close(&fixture);
    assert!(fixture.ok(&["inspect", "happy-tab"])["completion"].is_object());
}

#[test]
fn herdr_tab_watchers_end_with_their_tab_whatever_the_daemon_reports() {
    let mut fixture = Fixture::new();
    install_fake_herdr(&fixture);
    let watch = |fixture: &Fixture, workspace: &str, tab: &str| {
        fixture
            .command()
            .env("PATH", fixture.interactive_path())
            .args(["internal", "herdr-watch", workspace, tab])
            .spawn()
            .unwrap()
    };
    let tab_gets = |fixture: &Fixture, tab: &str| {
        fs::read_to_string(fixture.root.path().join("herdr-tab-gets"))
            .unwrap_or_default()
            .lines()
            .filter(|line| serde_json::from_str::<Vec<String>>(line).unwrap()[2] == tab)
            .count()
    };
    let close_tab = |fixture: &Fixture, tab: &str| {
        let closed = fixture.root.path().join("closed-tabs");
        let mut tabs = fs::read_to_string(&closed).unwrap_or_default();
        tabs.push_str(&format!("{tab}\n"));
        fs::write(closed, tabs).unwrap();
    };
    let active = fixture.add("active")["id"].as_str().unwrap().to_owned();
    let mut watchers = Vec::new();
    for (tab, daemon_running) in [("w1:t1", true), ("w1:t2", false)] {
        if !daemon_running {
            fixture.daemon.child.kill().unwrap();
            fixture.daemon.child.wait().unwrap();
        }
        let mut watcher = watch(&fixture, &active, tab);
        wait_until("Herdr tab polling", || tab_gets(&fixture, tab) >= 2);
        assert!(watcher.try_wait().unwrap().is_none());
        // A failed tab query is not a closed tab.
        let unreachable = fixture.root.path().join("herdr-unreachable");
        fs::write(&unreachable, "").unwrap();
        let seen = tab_gets(&fixture, tab);
        wait_until("failed Herdr tab polling", || {
            tab_gets(&fixture, tab) >= seen + 2
        });
        assert!(watcher.try_wait().unwrap().is_none());
        fs::remove_file(unreachable).unwrap();
        close_tab(&fixture, tab);
        watchers.push(watcher);
    }
    for mut watcher in watchers {
        let status = wait_for("tab watcher exit", || watcher.try_wait().unwrap());
        assert!(status.success());
    }
    // Only reads: a gone tab leaves nothing to close.
    assert!(!fixture.root.path().join("herdr-calls").exists());
}

fn wait_for_herdr_close(fixture: &Fixture) {
    wait_until("Herdr tab closure", || {
        herdr_calls(fixture).last().unwrap() == &["tab", "close", "w1:t9"]
    });
}

#[test]
fn scoped_undone_withdraws_completion_without_keeping_the_workspace() {
    let fixture = issue_fixture("gh");
    let scoped = |fixture: &Fixture, args: &[&str]| {
        let mut command = vec![
            "exec",
            "issue-work",
            "--",
            env!("CARGO_BIN_EXE_shoal"),
            "--json",
        ];
        command.extend_from_slice(args);
        fixture.ok(&command)
    };
    fixture.ok(&["hold", "acquire", "issue-work", "--name", "review"]);
    scoped(&fixture, &["done"]);
    assert_eq!(
        fixture.ok(&["inspect", "issue-work"])["workspace"]["done"],
        true
    );
    let undone = scoped(&fixture, &["undone"]);
    assert_eq!(undone["withdrawn"]["cleanup"], true);
    assert!(scoped(&fixture, &["undone"])["withdrawn"].is_null());
    let inspection = fixture.ok(&["inspect", "issue-work"]);
    assert!(inspection["completion"].is_null());
    assert_eq!(inspection["workspace"]["done"], false);
    fixture.ok(&["hold", "release", "issue-work", "--name", "review"]);
    // Only holds keep a workspace; cleanup removes the merged one on request,
    // which needs a default branch it can fetch.
    git(&fixture.repo, &["remote", "remove", "origin"]);
    assert_eq!(
        fixture.ok(&["cleanup"])["removed"],
        serde_json::json!(["issue-work"])
    );
}

#[test]
fn exported_environment_delivers_ports_and_confines_external_processes() {
    let mut fixture = Fixture::new();
    install_test_script(fixture.repo.join("setup.sh"), "#!/bin/sh\nexit 0\n");
    git(&fixture.repo, &["add", "setup.sh"]);
    commit_resource_config(&fixture.repo, "setup_cmd = 'setup.sh'\n");
    let workspace = fixture.add("external");
    fixture.add("other");
    let web = fixture.ok(&["port", "acquire", "web", "external"]);
    let api = fixture.ok(&["port", "acquire", "api", "external", "--env", "API_PORT"]);
    let exported = fixture.ok(&["env", "external"]);
    let values: std::collections::BTreeMap<String, String> =
        serde_json::from_value(exported.clone()).unwrap();
    assert_eq!(exported["SHOAL_WORKSPACE_ID"], workspace["id"]);
    assert_eq!(exported["SHOAL_RUN_ID"], workspace["id"]);
    assert_eq!(exported["SHOAL_WORKSPACE"], "external");
    assert_eq!(
        exported["SHOAL_STATE_DIR"],
        fixture.root.path().join("state").to_str().unwrap()
    );
    assert_eq!(exported["SHOAL_PORT_WEB"], web["port"].to_string());
    assert_eq!(exported["API_PORT"], api["port"].to_string());
    assert_eq!(
        exported["SHOAL_RESERVED_PORT_ENV"],
        "API_PORT:SHOAL_PORT_WEB"
    );
    assert!(!values.contains_key("SHOAL_EXECUTION_ID"));
    let child = support::isolated(fixture.root.path(), "sh")
        .envs(&values)
        .args(["-c", "printf '%s|%s' \"$SHOAL_PORT_WEB\" \"$API_PORT\""])
        .output()
        .unwrap();
    assert!(child.status.success());
    assert_eq!(
        String::from_utf8(child.stdout).unwrap(),
        format!("{}|{}", web["port"], api["port"])
    );
    assert!(
        fixture.ok(&["inspect", "external"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let scoped = |args: &[&str]| {
        fixture
            .command()
            .envs(&values)
            .current_dir(workspace["path"].as_str().unwrap())
            .args(args)
            .output()
            .unwrap()
    };
    for args in [
        vec!["status"],
        vec!["inspect"],
        vec!["port"],
        vec!["setup"],
        vec!["exec", "--", env!("CARGO_BIN_EXE_shoal"), "port"],
    ] {
        let output = scoped(&args);
        assert!(output.status.success(), "{args:?}: {output:?}");
    }
    for args in [
        vec!["add", fixture.repo.to_str().unwrap(), "forbidden"],
        vec!["rm", "external", "--yes"],
        vec!["rm", "other", "--yes"],
        vec!["notifications"],
        vec!["inspect", "other"],
        vec!["env"],
        vec!["env", "other"],
        vec!["env", "external", "--revoke", &values["SHOAL_SCOPE_TOKEN"]],
    ] {
        let output = scoped(&args);
        assert!(!output.status.success(), "{args:?}: {output:?}");
    }
    fixture.restart();
    let output = fixture
        .command()
        .envs(&values)
        .args(["inspect", "external"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn exported_tokens_expire_on_revocation_and_successful_removal() {
    let fixture = Fixture::new();
    let workspace = fixture.add("external");
    fixture.add("other");
    let first = fixture.ok(&["env", "external"]);
    let second = fixture.ok(&["env", "external"]);
    let first_token = first["SHOAL_SCOPE_TOKEN"].as_str().unwrap();
    let second_token = second["SHOAL_SCOPE_TOKEN"].as_str().unwrap();
    let inspect = |token: &str| {
        fixture
            .command()
            .env("SHOAL_SCOPE_TOKEN", token)
            .args(["inspect", "external"])
            .output()
            .unwrap()
    };
    let wrong_owner = fixture.run(&["env", "other", "--revoke", first_token]);
    assert!(!wrong_owner.status.success());
    assert!(inspect(first_token).status.success());
    assert_eq!(
        fixture.ok(&["env", "external", "--revoke", first_token])["revoked"],
        true
    );
    let revoked = inspect(first_token);
    assert!(!revoked.status.success());
    assert!(String::from_utf8_lossy(&revoked.stderr).contains("expired or unknown"));
    assert!(inspect(second_token).status.success());
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join("dirty"),
        "preserve\n",
    )
    .unwrap();
    assert!(!fixture.run(&["rm", "external"]).status.success());
    assert!(inspect(second_token).status.success());
    fixture.ok(&["rm", "external", "--yes"]);
    let removed = inspect(second_token);
    assert!(!removed.status.success());
    assert!(String::from_utf8_lossy(&removed.stderr).contains("expired or unknown"));
    // The branch had no commits, so removal deleted it too.
    fixture.ok(&["add", fixture.repo.to_str().unwrap(), "external"]);
    assert!(!inspect(second_token).status.success());
    assert!(fixture.ok(&["env", "external"])["SHOAL_SCOPE_TOKEN"].is_string());
}

#[test]
fn holds_are_visible_and_manual_removal_confirms_holders() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("held");
    let hold = fixture.ok(&[
        "hold",
        "acquire",
        "held",
        "--name",
        "app-thread",
        "--reason",
        "Open thread",
    ]);
    assert_eq!(hold["workspace_id"], workspace["id"]);
    let expected = serde_json::json!([hold]);
    for command in ["status", "inspect"] {
        assert_eq!(
            fixture.ok(&[command, "held"])["workspace"]["holds"],
            expected
        );
    }
    assert_eq!(fixture.ok(&["work"])[0]["holds"], expected);
    assert_eq!(fixture.ok(&["hold", "held"]), expected);
    assert_eq!(fixture.ok(&["hold", "list", "held"]), expected);
    assert_eq!(fixture.ok(&["hold", "--all"])[0]["holds"], expected);
    assert!(
        String::from_utf8_lossy(&fixture.run(&["status", "held"]).stdout)
            .contains("app-thread (Open thread)")
    );
    // Workspace processes list their holds but cannot change them.
    let scoped = |args: &[&str]| {
        let mut command = vec!["exec", "held", "--", env!("CARGO_BIN_EXE_shoal"), "--json"];
        command.extend_from_slice(args);
        fixture.run(&command)
    };
    let listed = scoped(&["hold"]);
    assert!(listed.status.success(), "{listed:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&listed.stdout).unwrap(),
        expected
    );
    for action in ["acquire", "release"] {
        let output = scoped(&["hold", action, "--name", "scoped"]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("shoal ask"),
            "{output:?}"
        );
    }
    fixture.restart();
    assert_eq!(fixture.ok(&["hold", "held"]), expected);
    let (no, prompt) = fixture.interactive(&["rm", "held"], "n\n");
    assert!(!no.status.success());
    assert!(
        prompt.find("app-thread (Open thread)").unwrap()
            < prompt.find("Are you sure? [y/N]").unwrap()
    );
    let removed = fixture.ok(&["rm", "held", "--yes"]);
    assert_eq!(removed["holds"], expected);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM workspace_holds", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let missing = fixture.add("missing-held");
    // A commit of its own keeps the branch from the kept-branch sweep.
    commit_file(
        Path::new(missing["path"].as_str().unwrap()),
        "missing-held",
        "work",
    );
    fixture.ok(&["hold", "acquire", "missing-held", "--name", "app"]);
    fs::remove_dir_all(missing["path"].as_str().unwrap()).unwrap();
    fixture.restart();
    wait_removed(&fixture, "missing-held");
    assert!(!git(&fixture.repo, &["branch", "--list", "missing-held"]).is_empty());
    assert_eq!(
        db.query_row("SELECT count(*) FROM workspace_holds", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn terminal_agents_run_tracked_in_a_labelled_zmx_session_unless_nested_or_detached() {
    let fixture = Fixture::new();
    let workspace = fixture.add("persist");
    let id = workspace["id"].as_str().unwrap();
    let home = fixture.root.path();
    install_test_script(
        home.join("bin/claude"),
        "#!/bin/sh\nprintf '%s %s\\n' \"${ZMX_SESSION:-none}\" \"${SHOAL_EXECUTION_ID:+tracked}\" >> \"$HOME/agent-runs\"\n",
    );
    let runs = || fs::read_to_string(home.join("agent-runs")).unwrap_or_default();

    let launch = |command: &mut Command| fixture.terminal_command(command, "", true);
    let (output, transcript) = launch(fixture.command().args(["claude", "persist"]));
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let calls = zmx_calls(&fixture);
    assert_eq!(calls[0], ["list"]);
    let labels = format!("shoal.workspace={id} shoal.agent=claude");
    assert_eq!(calls[1][..4], ["attach", "--labels", &labels, "persist"]);
    let worker = &calls[1][4..];
    let session = worker.iter().position(|arg| arg == "session").unwrap();
    assert_eq!(worker[session - 1], "internal");
    assert_eq!(
        worker[session + 1..session + 5],
        [id, "claude", "--", "claude"]
    );
    assert_eq!(runs(), "persist tracked\n");

    // A session name in use gets the next suffix.
    fs::write(
        home.join("zmx-sessions"),
        format!("  name=persist\tpid=1\tclients=0\tshoal.workspace={id}\n"),
    )
    .unwrap();
    let (output, transcript) = launch(fixture.command().args(["claude", "persist"]));
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let attaches = |calls: Vec<Vec<String>>| calls.into_iter().filter(|call| call[0] == "attach");
    assert_eq!(
        attaches(zmx_calls(&fixture)).next_back().unwrap()[3],
        "persist-2"
    );

    // Inside a session or without a terminal, the agent runs in place.
    let before = zmx_calls(&fixture).len();
    let (output, transcript) = launch(
        fixture
            .command()
            .env("ZMX_SESSION", "outer")
            .args(["claude", "persist"]),
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let output = fixture.run(&["claude", "persist"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(zmx_calls(&fixture).len(), before);
    assert_eq!(
        runs(),
        "persist tracked\npersist-2 tracked\nouter tracked\nnone tracked\n"
    );
}

/// A fake agent that records its directory and NUL-separated arguments.
fn install_recording_agent(fixture: &Fixture, agent: &str) {
    install_test_script(
        fixture.root.path().join("bin").join(agent),
        "#!/bin/sh\npwd > \"$HOME/chat-cwd\"\nprintf '%s\\0' \"$@\" > \"$HOME/chat-args\"\necho \"${ZMX_SESSION:-none}\" >> \"$HOME/chat-sessions\"\necho \"$SHOAL_CHAT\" > \"$HOME/chat-id\"\n",
    );
}

/// Every retained journal event.
fn journal(fixture: &Fixture) -> Vec<Value> {
    let output = fixture
        .command()
        .args(["--json", "internal", "events"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn chat_args(fixture: &Fixture) -> Vec<String> {
    fs::read_to_string(fixture.root.path().join("chat-args"))
        .unwrap()
        .split_terminator('\0')
        .map(str::to_owned)
        .collect()
}

#[test]
fn chat_runs_the_agent_in_the_checkout_in_a_labelled_zmx_session() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    install_recording_agent(&fixture, "claude");
    let repository = fixture.ok(&["repo", "list"])[0].clone();
    let id = repository["id"].as_str().unwrap();
    let checkout = fs::canonicalize(&fixture.repo).unwrap();

    let launch = |command: &mut Command| fixture.terminal_command(command, "", true);
    let (output, transcript) = launch(fixture.command().args([
        "chat",
        fixture.repo.to_str().unwrap(),
        "--agent",
        "claude",
        "--",
        "--model",
        "fast",
    ]));
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let calls = zmx_calls(&fixture);
    assert_eq!(calls[0], ["list"]);
    let labels = format!("shoal.chat={id} shoal.agent=claude");
    assert_eq!(calls[1][..3], ["attach", "--labels", &labels]);
    // The session's Shoal records the chat and becomes its agent.
    let chat = calls[1].iter().position(|arg| arg == "chat").unwrap();
    assert_eq!(
        calls[1][chat..],
        [
            "chat", id, "--here", "--agent", "claude", "--", "--model", "fast"
        ]
    );
    let started = journal(&fixture)
        .into_iter()
        .find(|event| event["kind"] == "chat_started")
        .unwrap();
    assert_eq!(started["type"], "chat");
    assert_eq!(started["chat"]["repository_id"], id);
    assert_eq!(started["chat"]["agent"], "claude");
    assert_eq!(started["chat"]["number"], 1);
    assert_eq!(
        fs::read_to_string(home.join("chat-id")).unwrap().trim(),
        started["chat"]["id"].as_str().unwrap()
    );
    assert_eq!(
        fs::read_to_string(home.join("chat-cwd")).unwrap().trim(),
        checkout.to_str().unwrap()
    );
    let args = chat_args(&fixture);
    assert_eq!(args[0], "--append-system-prompt");
    assert!(args[1].contains(checkout.to_str().unwrap()), "{}", args[1]);
    // The chat's hooks deliver its messages and wake it, but report no
    // workspace turn state.
    assert_eq!(args[2], "--settings");
    let hooks: Value = serde_json::from_str(&args[3]).unwrap();
    let command = |event: &str, group: usize| {
        hooks["hooks"][event][group]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert!(command("PostToolUse", 0).ends_with(" messages --hook"));
    assert!(command("Stop", 0).contains(" internal wake "));
    assert!(!args[3].contains("agent-state"), "{}", args[3]);
    assert_eq!(args[4..], ["--model", "fast"]);
    // The checkout is trusted as workspace launches trust theirs.
    let claude: Value =
        serde_json::from_str(&fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
    assert_eq!(
        claude["projects"][checkout.to_str().unwrap()]["hasTrustDialogAccepted"],
        true
    );
    // Chats are not workspace executions.
    assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));

    // Inside a session the agent replaces Shoal in place, and a later chat
    // starts another one instead of reusing it.
    let before = zmx_calls(&fixture).len();
    let (output, transcript) = launch(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("ZMX_SESSION", "outer")
            .args(["chat", "--agent", "claude"]),
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(zmx_calls(&fixture).len(), before);
    assert_eq!(
        fs::read_to_string(home.join("chat-sessions")).unwrap(),
        format!("{}\nouter\n", calls[1][3])
    );
}

#[test]
fn workspaces_belong_to_the_chat_that_opened_them() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    // The chat's agent opens a workspace while the chat runs.
    install_test_script(
        home.join("bin/claude"),
        format!(
            "#!/bin/sh\nshoal='{}'\n\"$shoal\" --json add \"$PWD\" chat-worker </dev/null > \"$HOME/chat-add\"\n\"$shoal\" work > \"$HOME/chat-work\"\n\"$shoal\" --json work --chats > \"$HOME/chat-chats\"\n\"$shoal\" status chat-worker > \"$HOME/chat-status\"\n",
            env!("CARGO_BIN_EXE_shoal")
        ),
    );
    // The agent's commands reach the daemon of an explicit state directory.
    let state = home.join("state");
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("ZMX_SESSION", "outer")
            .env_remove("SHOAL_STATE_DIR")
            .arg("--state-dir")
            .arg(&state)
            .args(["chat", "--agent", "claude"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let started = journal(&fixture)
        .into_iter()
        .find(|event| event["kind"] == "chat_started")
        .unwrap();
    let chat = &started["chat"]["id"];
    let added: Value =
        serde_json::from_str(&fs::read_to_string(home.join("chat-add")).unwrap()).unwrap();
    assert_eq!(added["chat"], *chat, "{added}");
    let changed = journal(&fixture)
        .into_iter()
        .find(|event| event["kind"] == "chat_changed")
        .unwrap();
    assert_eq!((&changed["name"], &changed["chat"]), (&added["name"], chat));

    // While it runs, `work` lists the chat with its workspaces under it.
    let chats: Value =
        serde_json::from_str(&fs::read_to_string(home.join("chat-chats")).unwrap()).unwrap();
    assert_eq!(chats, serde_json::json!([started["chat"]]));
    let name = fs::canonicalize(&fixture.repo).unwrap();
    let name = name.file_name().unwrap().to_str().unwrap();
    let rows = fs::read_to_string(home.join("chat-work")).unwrap();
    let rows: Vec<Vec<&str>> = rows
        .lines()
        .map(|row| {
            row.split("  ")
                .map(str::trim)
                .filter(|cell| !cell.is_empty())
                .collect()
        })
        .collect();
    assert_eq!(
        rows,
        [
            vec![
                format!("◆ {name} chat").as_str(),
                "process running",
                "claude"
            ],
            vec!["○ └ chat-worker", "inactive"],
        ]
    );

    let status = fs::read_to_string(home.join("chat-status")).unwrap();
    assert!(
        status.contains(&format!("Chat:          {name} chat (claude)\n")),
        "{status}"
    );

    // Once its agent exits the chat ends, and its workspace returns to the user.
    assert_eq!(fixture.ok(&["work", "--chats"]), serde_json::json!([]));
    assert_eq!(fixture.ok(&["work"])[0]["chat"], Value::Null);
    let ended = journal(&fixture)
        .into_iter()
        .find(|event| event["kind"] == "chat_ended")
        .unwrap();
    assert_eq!(ended["chat"], started["chat"]);

    // Opening it outside a chat gives it back to the user.
    let reopened = fixture.ok(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "chat-worker",
    ]);
    assert_eq!(reopened["chat"], Value::Null);
}

#[test]
fn a_chat_receives_the_events_of_the_workers_it_coordinates() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    // The chat's agent opens a workspace, whose agent then reports.
    install_test_script(
        home.join("bin/claude"),
        format!(
            "#!/bin/sh\nshoal='{}'\n\"$shoal\" add \"$PWD\" chat-worker </dev/null >/dev/null\n\"$shoal\" internal agent-state working --workspace chat-worker\n\"$shoal\" notify --workspace chat-worker 'Tests pass'\necho '{{\"hook_event_name\":\"PostToolUse\"}}' | \"$shoal\" messages --hook > \"$HOME/chat-hook\"\n\"$shoal\" messages --hook </dev/null > \"$HOME/chat-hook-again\"\n",
            env!("CARGO_BIN_EXE_shoal")
        ),
    );
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("ZMX_SESSION", "outer")
            .args(["chat", "--agent", "claude"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let hook: Value =
        serde_json::from_str(&fs::read_to_string(home.join("chat-hook")).unwrap()).unwrap();
    assert_eq!(
        hook["hookSpecificOutput"]["additionalContext"],
        "New Shoal messages about the workspaces this chat coordinates:\n- chat-worker: Tests pass"
    );
    // A delivered message does not come back, and never reached the user.
    assert_eq!(
        fs::read_to_string(home.join("chat-hook-again")).unwrap(),
        ""
    );
    assert_eq!(fixture.ok(&["notifications"]), serde_json::json!([]));
}

#[test]
fn chat_instructions_append_the_checkout_template_to_the_global_one() {
    let fixture = Fixture::new();
    install_recording_agent(&fixture, "claude");
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("chat-template.md"),
        "Discuss {repo} at {path}",
    )
    .unwrap();
    fs::write(
        fixture.repo.join("chat-template.md"),
        "Plans live in docs {branch}",
    )
    .unwrap();
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    let chat = || {
        let (output, transcript) = fixture.terminal_command(
            fixture
                .command()
                .current_dir(&fixture.repo)
                .env("ZMX_SESSION", "outer")
                .args(["chat", "--agent", "claude"]),
            "",
            true,
        );
        assert!(output.status.success(), "{output:?}\n{transcript}");
        chat_args(&fixture)
    };
    let name = checkout.file_name().unwrap().to_str().unwrap();
    assert_eq!(
        chat()[..2],
        [
            "--append-system-prompt".to_owned(),
            format!(
                "Discuss {name} at {}\n\nPlans live in docs {{branch}}",
                checkout.display()
            ),
        ]
    );
    // Empty templates pass no instructions.
    fs::write(config_dir.join("chat-template.md"), "").unwrap();
    fs::write(fixture.repo.join("chat-template.md"), "").unwrap();
    assert!(!chat().iter().any(|arg| arg == "--append-system-prompt"));
}

#[test]
fn chat_syncs_the_default_branch_before_the_agent_starts() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    install_test_script(
        home.join("bin/claude"),
        "#!/bin/sh\ngit rev-parse HEAD > \"$HOME/chat-head\"\nprintf '%s\\0' \"$@\" > \"$HOME/chat-args\"\n",
    );
    let author = upstream_remote(&fixture);
    commit_file(&author, "chat-template.md", "Upstream chat guidance\n");
    git(&author, &["push", "origin", "main"]);
    let chat = || {
        let (output, transcript) = fixture.terminal_command(
            fixture
                .command()
                .current_dir(&fixture.repo)
                .env("ZMX_SESSION", "outer")
                .args(["chat", "--agent", "claude"]),
            "",
            true,
        );
        assert!(output.status.success(), "{output:?}\n{transcript}");
        let head = fs::read_to_string(home.join("chat-head")).unwrap();
        (
            head,
            format!("{}{transcript}", String::from_utf8_lossy(&output.stderr)),
        )
    };
    // A refused agent leaves the checkout as it was.
    let stale = git(&fixture.repo, &["rev-parse", "main"]);
    let (output, transcript) = fixture.interactive_command(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .args(["chat", "--agent", "happy-claude"]),
        "",
    );
    assert!(!output.status.success(), "{transcript}");
    assert!(
        transcript.contains("Happy sessions are off"),
        "{transcript}"
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), stale);
    // The checkout on the default branch advances before the agent reads it,
    // and the instructions come from the refreshed checkout.
    let upstream = git(&author, &["rev-parse", "HEAD"]);
    assert_eq!(chat().0, upstream);
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), upstream);
    let instructions = &chat_args(&fixture)[1];
    assert!(
        instructions.contains("Upstream chat guidance"),
        "{instructions}"
    );
    // Another checked-out branch stays put while the default branch advances.
    git(&fixture.repo, &["switch", "-c", "notes"]);
    commit_file(&author, "later", "later\n");
    git(&author, &["push", "origin", "main"]);
    let later = git(&author, &["rev-parse", "HEAD"]);
    assert_eq!(chat().0, upstream);
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), later);
    assert_eq!(git(&fixture.repo, &["branch", "--show-current"]), "notes\n");
    // A refresh that cannot fast-forward warns and the chat still starts.
    git(&fixture.repo, &["switch", "main"]);
    commit_file(&fixture.repo, "local", "local\n");
    let local = git(&fixture.repo, &["rev-parse", "main"]);
    commit_file(&author, "remote", "remote\n");
    git(&author, &["push", "origin", "main"]);
    let (head, output) = chat();
    assert_eq!(head, local);
    assert!(
        output.contains("warning:") && output.contains("diverged"),
        "{output}"
    );
}

#[test]
fn chat_opens_a_focused_herdr_tab_at_the_checkout() {
    let fixture = Fixture::with_config(Some("focus = false\n"));
    install_fake_herdr(&fixture);
    install_recording_agent(&fixture, "codex");
    let repository = fixture.ok(&["repo", "list"])[0].clone();
    let id = repository["id"].as_str().unwrap();
    let checkout = fs::canonicalize(&fixture.repo).unwrap();

    let (caller, transcript) = herdr_call(
        &fixture,
        &[
            "chat",
            fixture.repo.to_str().unwrap(),
            "--agent",
            "codex",
            "--",
            "--search",
        ],
        "",
    );
    assert!(caller.status.success(), "{caller:?} {transcript}");
    let create = &herdr_calls(&fixture)[0];
    assert_eq!(create[..2], ["tab", "create"]);
    let option = |name: &str| {
        let at = create.iter().position(|arg| arg == name).unwrap();
        create[at + 1].clone()
    };
    assert_eq!(Path::new(&option("--cwd")), fixture.repo);
    assert!(option("--label").ends_with(" chat"));
    assert!(create.iter().any(|arg| arg == "--focus"));
    let worker = herdr_worker(&fixture);
    let chat = worker.iter().position(|arg| arg == "chat").unwrap();
    assert_eq!(
        worker[chat..],
        ["chat", id, "--here", "--agent", "codex", "--", "--search"]
    );

    // The tab's own call runs the agent in its pane.
    let (output, transcript) = fixture.interactive_command(
        herdr_worker_command(&fixture)
            .current_dir(&fixture.repo)
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "w1:p9"),
        "",
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("chat-cwd"))
            .unwrap()
            .trim(),
        checkout.to_str().unwrap()
    );
    let args = chat_args(&fixture);
    assert_eq!(args[0], "-c");
    assert!(
        args[1].starts_with("developer_instructions="),
        "{}",
        args[1]
    );
    let hooks = args[2..6].chunks(2).map(|flag| {
        assert_eq!(flag[0], "-c");
        flag[1].split_once('=').unwrap().0
    });
    assert_eq!(
        hooks.collect::<Vec<_>>(),
        ["hooks.PostToolUse", "hooks.UserPromptSubmit"]
    );
    // Codex's default launcher appends its own options after forwarded ones.
    assert_eq!(args[6], "--search");
    let codex = fs::read_to_string(fixture.root.path().join(".codex/config.toml")).unwrap();
    assert!(
        codex.contains(checkout.to_str().unwrap()) && codex.contains("trusted"),
        "{codex}"
    );
    assert!(!fixture.root.path().join("zmx-calls").exists());
}

#[test]
fn chat_agents_without_an_instructions_template_get_them_as_their_first_message() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    let chat = |agent: &str| {
        install_recording_agent(&fixture, agent);
        let (output, transcript) = fixture.interactive_command(
            fixture
                .command()
                .args(["chat", repo, "--agent", agent, "--here", "--", "--fast"]),
            "",
        );
        assert!(output.status.success(), "{output:?}\n{transcript}");
        chat_args(&fixture)
    };
    // The message leads the arguments, and asks the agent to wait for the user.
    let args = chat("pi");
    assert_eq!(args.len(), 2, "{args:?}");
    assert!(args[0].contains(checkout.to_str().unwrap()), "{}", args[0]);
    assert!(
        args[0].ends_with(
            "\n\nReply only that you are ready, then wait for the user's first message."
        ),
        "{}",
        args[0]
    );
    assert_eq!(args[1], "--fast");
    // A launcher with `{prompt}` takes it there.
    assert_eq!(chat("opencode"), ["--prompt", &args[0], "--fast"]);
}

#[test]
fn plan_opens_a_chat_on_an_issue_that_needs_a_plan() {
    let fixture = Fixture::with_config(Some(
        "[issue]\nplan_label = 'needs-plan'\nplanned_label = 'planned'\n",
    ));
    forge_origin(&fixture);
    // The agents' directory is on the terminal's PATH.
    install_forgejo_curl(&fixture.root.path().join("bin"));
    let issue = |number: u64, title: &str| {
        serde_json::json!({"number": number, "title": title, "state": "open",
            "body": format!("Body of {number}."), "labels": [{"name": "needs-plan"}],
            "html_url": format!("https://forge.example/team/project/issues/{number}"),
            "pull_request": null})
    };
    for (number, title) in [(12, "Clarify retries"), (9, "Pick a cache")] {
        forgejo_api(
            fixture.root.path(),
            &format!("repos/team/project/issues/{number}"),
            &issue(number, title),
        );
    }
    forgejo_api(
        fixture.root.path(),
        "repos/team/project/issues?type=issues&state=open&labels=needs-plan&limit=50&page=1",
        &serde_json::json!([issue(12, "Clarify retries"), issue(9, "Pick a cache")]),
    );
    let plan = |args: &[&str], agent: &str| {
        install_recording_agent(&fixture, agent);
        let (output, transcript) = fixture.interactive_command(
            fixture
                .command()
                .arg("plan")
                .args(args)
                .args(["--agent", agent, "--here"])
                .current_dir(&fixture.repo),
            "",
        );
        assert!(output.status.success(), "{output:?}\n{transcript}");
        chat_args(&fixture)
    };

    // The oldest labelled issue opens the chat with the chat template and
    // the planning task, which swaps the configured labels.
    let args = plan(&["--oldest"], "pi");
    assert_eq!(args.len(), 1, "{args:?}");
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    assert!(args[0].contains(checkout.to_str().unwrap()), "{}", args[0]);
    for expected in [
        "\n\nPlan issue #9: Pick a cache\nhttps://forge.example/team/project/issues/9\n\nBody of 9.",
        "Then replace its `needs-plan` label with `planned`.",
        "`shoal add --issue 9 --agent <name>`.",
    ] {
        assert!(args[0].contains(expected), "{}", args[0]);
    }
    assert!(
        !args[0].contains("Reply only that you are ready"),
        "{}",
        args[0]
    );
    assert!(plan(&["--newest"], "pi")[0].contains("Plan issue #12: Clarify retries"));

    // An agent with an instructions template keeps them apart and starts on
    // the task as its prompt.
    // A named agent that cannot start is refused before the issue is chosen.
    let output = fixture
        .command()
        .args(["plan", "--agent", "missing-agent"])
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown agent \"missing-agent\""),
        "{stderr}"
    );

    let args = plan(&["https://forge.example/team/project/issues/12"], "claude");
    assert_eq!(args[0], "--append-system-prompt");
    assert!(!args[1].contains("Plan issue"), "{}", args[1]);
    assert!(
        args.last()
            .unwrap()
            .starts_with("Plan issue #12: Clarify retries"),
        "{args:?}"
    );
}

#[test]
fn happy_chats_run_detached_in_the_checkout_while_their_session_lives() {
    let fixture = Fixture::with_config(Some(HAPPY_ENABLED));
    let record = install_fake_happy(&fixture);
    let repo = fixture.repo.to_str().unwrap();
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    let chat = |agent: &str| {
        let _ = fs::remove_file(&record);
        let output = fixture
            .command()
            .args([
                "--json", "chat", repo, "--agent", agent, "--", "--model", "test",
            ])
            .env("HAPPY_RECORD", &record)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let started: Value = serde_json::from_slice(&output.stdout).unwrap();
        wait_until("fake happy record", || record.exists());
        (started, fs::read_to_string(&record).unwrap())
    };
    let args = |recorded: &str| -> Vec<String> {
        recorded
            .split_once("args=")
            .unwrap()
            .1
            .split_once("\nscope=")
            .unwrap()
            .0
            .split('\0')
            .filter(|arg| !arg.is_empty())
            .map(str::to_owned)
            .collect()
    };

    // Happy's Claude takes the instructions as arguments and waits.
    let (started, recorded) = chat("happy-claude");
    let chat_record = &started["chat"];
    assert_eq!(chat_record["agent"], "happy-claude");
    assert_eq!(chat_record["number"], 1);
    assert!(
        recorded.contains(&format!("cwd={}\n", checkout.display())),
        "{recorded}"
    );
    assert!(recorded.contains(&format!("chat={}\n", chat_record["id"].as_str().unwrap())));
    assert!(recorded.contains("stdin=eof\nstdout=notty"), "{recorded}");
    let claude = args(&recorded);
    assert_eq!(
        claude[..5],
        [
            "claude",
            "--happy-starting-mode",
            "remote",
            "--started-by",
            "daemon"
        ]
    );
    assert_eq!(claude[5], "--append-system-prompt");
    assert!(
        claude[6].contains(checkout.to_str().unwrap()),
        "{}",
        claude[6]
    );
    assert_eq!(claude[7..], ["--model", "test"]);
    assert_eq!(started["prompt_file"], Value::Null);
    // The chat lasts while the session runs, not the command that started it.
    let pid = chat_record["pid"].as_u64().unwrap();
    assert_eq!(
        fixture.ok(&["work", "--chats"]),
        serde_json::json!([chat_record])
    );
    let log = PathBuf::from(started["log"].as_str().unwrap());
    wait_until("the session's output", || {
        fs::read_to_string(&log).is_ok_and(|output| output.contains("hello from happy"))
    });

    // Happy's Codex takes them as its first message, saved here when Happy
    // cannot deliver it.
    let (started, recorded) = chat("happy-codex");
    assert_eq!(started["chat"]["number"], 2);
    assert_eq!(
        args(&recorded),
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
    let message = fs::read_to_string(started["prompt_file"].as_str().unwrap()).unwrap();
    assert!(message.contains(checkout.to_str().unwrap()), "{message}");
    assert!(
        message.ends_with("Reply only that you are ready, then wait for the user's first message."),
        "{message}"
    );
    assert_eq!(started["prompt_delivered"], false);

    for pid in [pid, started["chat"]["pid"].as_u64().unwrap()] {
        Command::new("kill").arg(pid.to_string()).status().unwrap();
    }
    wait_until("the chats to end", || {
        fixture.ok(&["work", "--chats"]) == serde_json::json!([])
    });
}

#[test]
fn chat_picks_chat_agents_and_refuses_unsupported_launches() {
    let fixture = Fixture::new();
    install_fake_agents(&fixture, &["claude", "happy"]);
    let repo = fixture.repo.to_str().unwrap();
    // Without a default agent a picker offers the installed agents that can run.
    let (output, rows) = fixture.pick(&["chat", repo], "0", "");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        rows.lines()
            .map(|row| row.split('\t').nth(1).unwrap())
            .collect::<Vec<_>>(),
        ["claude"]
    );
    let (output, transcript) = fixture.interactive_command(
        fixture
            .command()
            .args(["chat", repo, "--agent", "happy-claude"]),
        "",
    );
    assert!(!output.status.success());
    assert!(
        transcript.contains("cannot start happy-claude: Happy sessions are off"),
        "{transcript}"
    );
    let output = fixture.run(&["chat", repo, "--agent", "claude"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("chat needs a terminal"));
    // The checkout has no value for a workspace's placeholders.
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "[commands]\nclaude = ['claude', '--name', '{workspace}']\n",
    )
    .unwrap();
    let (output, transcript) = fixture.interactive_command(
        fixture
            .command()
            .args(["chat", repo, "--agent", "claude", "--here"]),
        "",
    );
    assert!(!output.status.success());
    assert!(
        transcript.contains(
            "the claude command uses {workspace}, which has no value outside a workspace"
        ),
        "{transcript}"
    );
    fs::remove_file(fixture.repo.join(".shoal.toml")).unwrap();
    fixture.add("scoped");
    let output = fixture.run(&[
        "exec",
        "scoped",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "chat",
        repo,
        "--agent",
        "claude",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("workspace processes cannot start chats"),
        "{output:?}"
    );
}

#[test]
fn terminal_add_detaches_launches_with_input_and_attaches_the_rest() {
    // `shift` drops Shoal's guidance, the first argument.
    let fixture = Fixture::with_config(Some(
        "[commands]\npi = ['sh', '-c', 'shift; echo \"$ZMX_SESSION ${SHOAL_EXECUTION_ID:+tracked} $*\" >> \"$HOME/agent-runs\"', 'agent', '{args}']\n",
    ));
    let home = fixture.root.path();
    let repo = fixture.repo.to_str().unwrap();
    let runs = || fs::read_to_string(home.join("agent-runs")).unwrap_or_default();

    // An agent with input starts in a detached session that runs the plan.
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .args(["add", repo, "background", "--agent", "pi", "--", "go"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert!(
        transcript.contains(
            "Started background in a detached zmx session; attach with `shoal attach background`"
        ),
        "{transcript}"
    );
    let calls = zmx_calls(&fixture);
    assert_eq!(calls[1], ["run", "background", "-d", "true"]);
    assert_eq!(calls[2][..2], ["send", "background"]);
    assert!(
        calls[2][2].starts_with("SHOAL_ENDS_SESSION=1 ")
            && calls[2][2].ends_with(" internal add && exit; exec \"$SHELL\" -l\r"),
        "{calls:?}"
    );
    let session: Value =
        serde_json::from_str(&fs::read_to_string(home.join("zmx-background.env")).unwrap())
            .unwrap();
    assert!(session["SHOAL_ADD_PLAN"].is_string(), "{session}");
    let log = fs::read_to_string(home.join("zmx-background.log")).unwrap_or_default();
    assert_eq!(runs(), "background tracked go\n", "{log}");
    let workspace = fixture.ok(&["inspect", "background"]);
    let id = workspace["workspace"]["id"].as_str().unwrap();
    let labels = [
        "set",
        "background",
        &format!("shoal.workspace={id}"),
        "shoal.agent=pi",
    ];
    assert!(calls.iter().any(|call| call == &labels), "{calls:?}");

    // An agent waiting for its first prompt attaches this terminal, which
    // detaching returns to the caller's shell where it was.
    let directive = home.join("shell-directive");
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .args(["add", repo, "focused", "--agent", "pi"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let calls = zmx_calls(&fixture);
    let attach = calls.iter().rfind(|call| call[0] == "attach").unwrap();
    assert_eq!(attach[..2], ["attach", "--labels"]);
    assert_eq!(attach[3], "focused");
    assert_eq!(runs(), "background tracked go\nfocused tracked \n");
    assert!(!directive.exists(), "{transcript}");

    // Without an agent the caller's shell enters the workspace.
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .args(["add", repo, "shell-here", "--focus"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let workspace = fixture.ok(&["inspect", "shell-here"]);
    assert_eq!(
        fs::read_to_string(&directive).unwrap().trim(),
        workspace["workspace"]["path"].as_str().unwrap()
    );
}

#[test]
fn terminal_add_handing_codex_to_its_app_enters_the_workspace() {
    let fixture = Fixture::with_config(Some("[codex]\ndefault_mode = 'app'\n"));
    let home = fixture.root.path();
    install_test_script(
        home.join("bin/codex"),
        "#!/bin/sh\necho \"$@\" >> \"$HOME/codex-runs\"\n",
    );
    let directive = home.join("shell-directive");
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .args([
                "add",
                fixture.repo.to_str().unwrap(),
                "app",
                "--agent",
                "codex",
                "--focus",
            ]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let workspace = fixture.ok(&["inspect", "app"]);
    let path = workspace["workspace"]["path"].as_str().unwrap();
    assert_eq!(
        fs::read_to_string(home.join("codex-runs")).unwrap(),
        format!("app {path}\n")
    );
    // The app opens without a zmx session to attach.
    assert!(
        !zmx_calls(&fixture).iter().any(|call| call[0] == "attach"),
        "{transcript}"
    );
    assert_eq!(fs::read_to_string(&directive).unwrap().trim(), path);
}

#[test]
fn terminal_add_decides_where_the_shell_goes_after_the_post_setup_hook() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    install_test_script(home.join("bin/codex"), "#!/bin/sh\nexit 0\n");
    // The hook turns the app handoff the repository asks for into a CLI launch.
    install_test_script(
        fixture.repo.join("post-setup.sh"),
        "#!/bin/sh\nprintf \"[codex]\\ndefault_mode = 'cli'\\n\" > .shoal.toml\n",
    );
    commit_resource_config(
        &fixture.repo,
        "post_setup_cmd = 'post-setup.sh'\n[codex]\ndefault_mode = 'app'\n",
    );
    let directive = home.join("shell-directive");
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .args([
                "add",
                fixture.repo.to_str().unwrap(),
                "switched",
                "--agent",
                "codex",
                "--focus",
            ]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let attach = zmx_calls(&fixture)
        .into_iter()
        .rfind(|call| call[0] == "attach");
    assert_eq!(attach.unwrap()[3], "switched", "{transcript}");
    assert!(!directive.exists(), "{transcript}");
}

#[test]
fn background_add_without_an_agent_opens_the_workspace_shell_in_its_session() {
    let fixture = Fixture::with_config(Some("focus = false\n"));
    let home = fixture.root.path();
    let shell = home.join("bin/test-shell");
    install_test_script(&shell, "#!/bin/sh\npwd > \"$HOME/shell-cwd\"\n");
    let (output, transcript) = fixture.terminal_command(
        fixture.command().env("SHELL", &shell).args([
            "add",
            fixture.repo.to_str().unwrap(),
            "shell-only",
        ]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert!(transcript.contains("detached zmx session"), "{transcript}");
    let workspace = fixture.ok(&["inspect", "shell-only"]);
    let log = fs::read_to_string(home.join("zmx-shell-only.log")).unwrap_or_default();
    assert_eq!(
        fs::read_to_string(home.join("shell-cwd"))
            .unwrap_or_default()
            .trim(),
        fs::canonicalize(workspace["workspace"]["path"].as_str().unwrap())
            .unwrap()
            .to_str()
            .unwrap(),
        "{log}"
    );
    let id = workspace["workspace"]["id"].as_str().unwrap();
    let labels = ["set", "shell-only", &format!("shoal.workspace={id}")];
    assert!(zmx_calls(&fixture).iter().any(|call| call == &labels));
}

#[test]
fn terminal_resume_all_starts_each_agent_in_a_detached_session() {
    let fixture = Fixture::new();
    let mut expected = Vec::new();
    for name in ["first", "second"] {
        let workspace = fixture.add(name);
        let id = workspace["id"].as_str().unwrap().to_owned();
        let state = fixture.root.path().join("state/workspaces").join(&id);
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join(format!("{name}-agent.recovery.json")),
            r#"{"agent":"claude"}"#,
        )
        .unwrap();
        expected.push((name, id, format!("{name}-agent")));
    }
    // The sessions cannot resume without a configured restore command; the
    // test observes what each was asked to run.
    let shell = fixture.root.path().join("bin/test-shell");
    install_test_script(&shell, "#!/bin/sh\necho \"$@\" >> \"$HOME/login-shells\"\n");
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .env("SHELL", &shell)
            .args(["resume", "--all"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert!(
        transcript.contains("Resuming 2 agents in detached zmx sessions"),
        "{transcript}"
    );
    assert!(!transcript.contains("in its own terminal"), "{transcript}");
    let calls = zmx_calls(&fixture);
    for (name, id, execution) in &expected {
        assert!(
            calls.contains(&vec![
                "run".into(),
                name.to_string(),
                "-d".into(),
                "true".into()
            ]),
            "{calls:?}"
        );
        let send = calls
            .iter()
            .find(|call| call.starts_with(&["send".into(), name.to_string()]))
            .unwrap();
        assert!(
            send[2].ends_with(&format!(
                " resume {id} --execution {execution} && exit; exec \"$SHELL\" -l\r"
            )),
            "{send:?}"
        );
    }
    // Each failed start leaves the user's own login shell to show why.
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("login-shells")).unwrap_or_default(),
        "-l\n-l\n"
    );
}

#[test]
fn a_detached_session_ends_with_its_agent_whatever_its_exit() {
    let fixture = Fixture::with_config(Some(
        "[commands]\npi = ['sh', '-c', 'echo \"${SHOAL_ENDS_SESSION-unset}\" > \"$HOME/agent-env\"; exit 3']\n",
    ));
    let home = fixture.root.path();
    let shell = home.join("bin/test-shell");
    install_test_script(&shell, "#!/bin/sh\necho \"$@\" >> \"$HOME/login-shells\"\n");
    let (output, transcript) = fixture.terminal_command(
        fixture.command().env("SHELL", &shell).args([
            "add",
            fixture.repo.to_str().unwrap(),
            "failing",
            "--agent",
            "pi",
            "--",
            "go",
        ]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let log = fs::read_to_string(home.join("zmx-failing.log")).unwrap_or_default();
    assert_eq!(
        fs::read_to_string(home.join("agent-env")).unwrap_or_default(),
        "unset\n",
        "{log}"
    );
    assert!(!home.join("login-shells").exists(), "{log}");
}

#[test]
fn an_idle_codex_with_updates_is_prompted_in_its_zmx_session() {
    let fixture = Fixture::new();
    fixture.add("prompted");
    fixture.ok(&["message", "Rebase onto main", "--workspace", "prompted"]);
    let home = fixture.root.path();
    // Codex's Stop hook reports the finished turn; it then waits, with a
    // generous hang guard, for the prompt Shoal types into its session.
    install_test_script(
        home.join("bin/codex"),
        format!(
            "#!/bin/sh\n'{}' internal agent-state idle >/dev/null || exit 3\n\
             for _ in $(seq 600); do [ -e \"$HOME/zmx-sent\" ] && exit 0; /bin/sleep 0.1; done\n\
             exit 4\n",
            env!("CARGO_BIN_EXE_shoal")
        ),
    );
    let (output, transcript) =
        fixture.terminal_command(fixture.command().args(["codex", "prompted"]), "", true);
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let sent: Vec<_> = zmx_calls(&fixture)
        .into_iter()
        .filter(|call| call[0] == "send")
        .collect();
    assert_eq!(
        sent,
        [[
            "send",
            "prompted",
            "\x1b[200~Shoal: new messages or activity on this workspace's issues and PRs.\x1b[201~\r"
        ]]
    );
}

#[test]
fn zmx_agent_exit_resets_input_modes_for_the_calling_shell() {
    let fixture = Fixture::new();
    fixture.add("persist");
    install_test_script(
        fixture.root.path().join("bin/claude"),
        "#!/bin/sh\nexit 0\n",
    );
    let mut command = fixture.command();
    command
        .env("TERM", "xterm-256color")
        .env("ZMX_TEST_LEAK_INPUT_MODE", "1")
        .args(["claude", "persist"]);
    let (output, transcript) = fixture.terminal_command(&mut command, "", true);
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let enabled = transcript.rfind("\x1b[?1000h").unwrap();
    let disabled = transcript.rfind("\x1b[?1000l").unwrap();
    assert!(disabled > enabled, "zmx leaked mouse reporting mode");
}

#[test]
fn attach_selects_a_session_of_the_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.add("persist");
    let other = fixture.add("other");
    let home = fixture.root.path();
    let attached = || {
        zmx_calls(&fixture)
            .into_iter()
            .rfind(|call| call[0] == "attach")
            .unwrap()
    };
    let (output, transcript) = fixture.interactive(&["attach", "persist"], "");
    assert!(!output.status.success());
    assert!(
        transcript.contains("persist has no agent sessions"),
        "{transcript}"
    );
    // Without a session anywhere, there is nothing to pick.
    let (output, transcript) = fixture.interactive(&["attach"], "");
    assert!(!output.status.success());
    assert!(
        transcript.contains("no workspace has an agent session"),
        "{transcript}"
    );

    let session = |name: &str, workspace: &Value| {
        format!(
            "  name={name}\tpid=1\tclients=0\tshoal.agent=claude\tshoal.workspace={}\n",
            workspace["id"].as_str().unwrap()
        )
    };
    fs::write(
        home.join("zmx-sessions"),
        session("persist", &workspace) + &session("other", &other),
    )
    .unwrap();
    let (output, transcript) = fixture.interactive(&["attach", "persist"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(attached(), ["attach", "persist"]);
    // The picker leaves out a workspace without a session.
    let idle = fixture.add("idle");
    let (output, rows) = fixture.pick(&["attach"], workspace["id"].as_str().unwrap(), "");
    assert!(output.status.success(), "{output:?}");
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let idle = idle["id"].as_str().unwrap();
    assert!(!rows.iter().any(|row| row.starts_with(idle)), "{rows:?}");
    assert_eq!(attached(), ["attach", "persist"]);
    // The fake session is still listed afterwards, as after a detach.
    assert!(
        transcript.contains("detached from persist; run `shoal attach persist` to return"),
        "{transcript}"
    );

    fs::write(
        home.join("zmx-sessions"),
        session("persist", &workspace) + &session("persist-2", &workspace),
    )
    .unwrap();
    let (output, rows) = fixture.pick(&["attach", "persist"], "1", "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains("persist-2  claude"), "{rows}");
    assert_eq!(attached(), ["attach", "persist-2"]);
    let (output, transcript) =
        fixture.interactive(&["attach", "persist", "--session", "persist"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(attached(), ["attach", "persist"]);
    let (output, transcript) =
        fixture.interactive(&["attach", "persist", "--session", "other"], "");
    assert!(!output.status.success());
    assert!(
        transcript.contains("persist has no session other"),
        "{transcript}"
    );

    let output = fixture.run(&["attach", "persist"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("attach requires a terminal"));
}

#[test]
fn status_lists_workspace_sessions_and_survives_a_failed_listing() {
    let fixture = Fixture::new();
    let workspace = fixture.add("persist");
    let other = fixture.add("other");
    let home = fixture.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    fs::write(
        home.join("zmx-sessions"),
        format!(
            "  name=persist\tpid=1\tclients=1\tshoal.agent=claude\tshoal.workspace={}\n  \
             name=other\tpid=2\tclients=0\tshoal.workspace={}\n",
            workspace["id"].as_str().unwrap(),
            other["id"].as_str().unwrap()
        ),
    )
    .unwrap();
    let status = fixture.ok(&["status", "persist"]);
    assert_eq!(
        status["sessions"],
        serde_json::json!([{"name": "persist", "agent": "claude", "clients": 1}])
    );
    let output = fixture.run(&["status", "persist"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("Sessions:      1\n  persist  claude  attached\n"),
        "{text}"
    );

    install_test_script(home.join("bin/zmx"), "#!/bin/sh\necho broken >&2\nexit 1\n");
    let status = fixture.ok(&["status", "persist"]);
    assert_eq!(status["sessions"], serde_json::json!([]));
    assert!(
        status["sessions_error"]
            .as_str()
            .unwrap()
            .contains("list zmx sessions"),
        "{status}"
    );
}

#[test]
fn terminal_agent_launches_refuse_a_missing_zmx_before_creating_anything() {
    let fixture = Fixture::new();
    install_fake_agents(&fixture, &["claude"]);
    let path = fixture.interactive_path();
    fs::remove_file(fixture.root.path().join("runner-bin/zmx")).unwrap();
    let repo = fixture.repo.to_str().unwrap();
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .args(["add", repo, "no-zmx", "--agent", "claude"]),
        "",
        true,
    );
    assert!(!output.status.success(), "{transcript}");
    assert!(
        transcript.contains("cannot start claude: zmx is not installed or not on PATH"),
        "{transcript}"
    );
    assert!(!fixture.run(&["inspect", "no-zmx"]).status.success());

    // Without a terminal the agent runs in place and needs no zmx.
    let output = fixture
        .command()
        .args(["add", repo, "no-zmx", "--agent", "claude"])
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn add_queues_agents_past_max_agents_until_removed() {
    let fixture = Fixture::with_config(Some(
        "max_agents = 1\n[overload.disk]\nenabled = false\n[commands]\nwaiter = ['sh', '-c', 'trap \"exit 0\" TERM; while :; do sleep 1; done']\n",
    ));
    let repo = fixture.repo.to_str().unwrap();
    let mut first = fixture
        .command()
        .args(["add", repo, "first", "--agent", "waiter"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Setup runs first, so an agent has started once its entry leaves the queue.
    let started = |name: &str| {
        fixture
            .ok(&["work"])
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == name && workspace["running"] == true)
            && fixture.ok(&["work", "--queue"]) == serde_json::json!([])
    };
    wait_until("the first agent start", || started("first"));

    let queued = fixture.ok(&["add", repo, "second", "--agent", "waiter"]);
    assert_eq!(queued["state"], "queued", "{queued}");
    assert_eq!(queued["position"], 1, "{queued}");
    let entries = fixture.ok(&["work", "--queue"]);
    assert_eq!(entries.as_array().unwrap().len(), 1, "{entries}");
    assert_eq!(entries[0]["name"], "second");
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "other\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    assert_eq!(
        fixture.ok(&["work", "--queue", "--repo", "other"]),
        serde_json::json!([])
    );
    let listed = String::from_utf8(fixture.run(&["work"]).stdout).unwrap();
    assert!(listed.contains("queued, position 1"), "{listed}");
    // A queued entry creates nothing until it starts.
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);

    let removed = fixture.ok(&["rm", "second"]);
    assert_eq!(removed["name"], "second", "{removed}");
    assert_eq!(fixture.ok(&["work", "--queue"]), serde_json::json!([]));

    // A focused launch is one the user waits for, so it skips the queue.
    let mut focused = fixture
        .command()
        .args(["add", repo, "focused", "--agent", "waiter", "--focus"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the focused agent start", || started("focused"));
    fixture.ok(&["stop", "focused"]);
    focused.wait().unwrap();
    fixture.ok(&["rm", "focused", "--yes"]);

    // A queued agent named like a workspace is removed only by its ID.
    let shared = fixture.ok(&["add", repo, "first", "--agent", "waiter"]);
    assert_eq!(shared["state"], "queued", "{shared}");
    let ambiguous = fixture.run(&["rm", "first", "--yes"]);
    assert!(!ambiguous.status.success());
    let message = String::from_utf8_lossy(&ambiguous.stderr);
    assert!(
        message.contains("names both a workspace and a queued agent"),
        "{message}"
    );
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
    fixture.ok(&["rm", shared["id"].as_str().unwrap()]);
    assert_eq!(fixture.ok(&["work", "--queue"]), serde_json::json!([]));
    fixture.ok(&["stop", "first"]);
    first.wait().unwrap();
}

/// Replace the managed daemon's executable while a request and an event
/// stream are active: the request completes once, the daemon restarts under
/// the same PID, and the stream continues after its cursor.
#[test]
fn managed_update_completes_active_requests_and_continues_event_streams() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::fs::symlink,
        sync::mpsc,
    };
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let repo = init_repo(root.path(), "repo", &[("tracked", "committed\n")]);
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let install = root.path().join("install");
    fs::create_dir(&install).unwrap();
    let launcher = install.join("shoal");
    let replacement = install.join("replacement");
    let handed_off = root.path().join("handed-off");
    symlink(env!("CARGO_BIN_EXE_shoal"), &launcher).unwrap();
    install_test_script(
        &replacement,
        "#!/bin/sh\nprintf started > \"$SHOAL_TEST_HANDED_OFF\"\nexec \"$SHOAL_TEST_REAL_BINARY\" \"$@\"\n",
    );
    // The hook holds `shoal add` in the daemon until the test opens the gate.
    let (hook_started, gate) = (root.path().join("hook-started"), root.path().join("gate"));
    let hook = root.path().join("before.sh");
    install_test_script(
        &hook,
        format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.05; done\n",
            hook_started.display(),
            gate.display()
        ),
    );
    fs::create_dir_all(root.path().join(".config/shoal")).unwrap();
    fs::write(
        root.path().join(".config/shoal/config.toml"),
        format!("pre_setup_cmd = '{}'\n", hook.display()),
    )
    .unwrap();
    let mut launch = support::isolated(root.path(), &launcher);
    launch
        .args(["daemon", "run", "--managed"])
        .env("SHOAL_TEST_HANDED_OFF", &handed_off)
        .env("SHOAL_TEST_REAL_BINARY", env!("CARGO_BIN_EXE_shoal"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut daemon = DaemonGuard {
        child: launch.spawn().unwrap(),
        stderr: None,
    };
    let shoal = || cli(root.path());
    wait_until("daemon startup", || {
        assert!(daemon.child.try_wait().unwrap().is_none(), "daemon exited");
        shoal()
            .args(["daemon", "status"])
            .output()
            .unwrap()
            .status
            .success()
    });
    let added = shoal()
        .args(["--json", "repo", "add", repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(added.status.success(), "{added:?}");

    let mut follower = shoal()
        .args(["--json", "internal", "events", "--follow"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (events, received) = mpsc::channel();
    let stdout = follower.stdout.take().unwrap();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let event: Value = serde_json::from_str(&line.unwrap()).unwrap();
            if events.send(event).is_err() {
                return;
            }
        }
    });
    let receive_until_ready = |name: &str| {
        let mut seen = Vec::new();
        loop {
            let event = received
                .recv_timeout(Duration::from_secs(120))
                .expect("event stream stalled");
            let ready = event["kind"] == "ready" && event["name"] == name;
            seen.push(event);
            if ready {
                return seen;
            }
        }
    };

    let request = shoal()
        .args(["--json", "add", repo.to_str().unwrap(), "before"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("pre-setup hook", || hook_started.exists());
    let next = install.join("shoal-next");
    symlink(&replacement, &next).unwrap();
    fs::rename(next, &launcher).unwrap();
    fs::write(&gate, "").unwrap();
    let request = request.wait_with_output().unwrap();
    assert!(request.status.success(), "{request:?}");
    let workspace: Value = serde_json::from_slice(&request.stdout).unwrap();
    assert_eq!(workspace["state"], "ready");
    let mut seen = receive_until_ready("before");

    wait_until("daemon handoff", || {
        assert!(daemon.child.try_wait().unwrap().is_none(), "daemon exited");
        handed_off.exists()
    });
    let status = shoal()
        .args(["--json", "daemon", "status"])
        .output()
        .unwrap();
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["daemon"]["pid"], daemon.child.id());
    let after = shoal()
        .args(["--json", "add", repo.to_str().unwrap(), "after"])
        .output()
        .unwrap();
    assert!(after.status.success(), "{after:?}");
    seen.extend(receive_until_ready("after"));
    follower.kill().unwrap();
    follower.wait().unwrap();

    let ids: Vec<_> = seen
        .iter()
        .map(|event| event["id"].as_i64().unwrap())
        .collect();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    let created: Vec<_> = seen
        .iter()
        .filter(|event| event["kind"] == "created")
        .map(|event| event["name"].as_str().unwrap())
        .collect();
    assert_eq!(created, ["before", "after"]);
}
