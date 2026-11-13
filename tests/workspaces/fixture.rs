//! The `Fixture` every test starts from, an isolated home, repository and
//! daemon with the commands and checks tests run against it, and the helpers
//! tests of several areas share.
use super::*;

pub(crate) struct Fixture(Base);

impl std::ops::Deref for Fixture {
    type Target = Base;

    fn deref(&self) -> &Base {
        &self.0
    }
}

impl std::ops::DerefMut for Fixture {
    fn deref_mut(&mut self) -> &mut Base {
        &mut self.0
    }
}

/// Global config without a cleanup grace period, at the root so the rest of a
/// test's config follows unchanged.
pub(crate) const NO_GRACE: &str = "cleanup.grace_minutes = 0\n";

/// Global config delivering each agent message at once, so a test reads it
/// without waiting for a run of them to settle.
pub(crate) const UNSETTLED: &str = "messages.settle_seconds = 0\n";

/// The scripted forges serve Forgejo's API without answering which one they
/// serve; `tests/forge.rs` covers asking.
pub(crate) const FORGEJO_HOST: &str = "machine.forges.\"forge.example\".type = \"forgejo\"\n";

/// Global config without memory overload protection, which stops a running
/// agent as soon as the machine running the tests reports critical memory
/// pressure. The daemon's unit tests cover the protection.
const NO_MEMORY_OVERLOAD: &str = "machine.overload.memory.enabled = false\n";

/// A long-running command whose environment stays readable once launchd adopts
/// it. macOS hides an orphaned `sleep`'s environment, which fails concurrent
/// tests' setup verification.
pub(crate) const READABLE_SLEEP: [&str; 3] = ["python3", "-c", "import time; time.sleep(60)"];

impl Fixture {
    pub(crate) fn new() -> Self {
        Self::with_config(None)
    }

    pub(crate) fn with_config(config: Option<&str>) -> Self {
        Self::with_tools(config, false)
    }

    pub(crate) fn with_tools(config: Option<&str>, fake_sim: bool) -> Self {
        let root = support::fixture::home();
        // Debug builds only open existing native ownership locks. Prepare the
        // isolated zmx namespace for terminal launch and cleanup tests.
        fs::write(root.path().join("zmx.shoal-ownership.lock"), "")
            .expect("prepare isolated zmx ownership lock");
        if fake_sim {
            let script = root.path().join("bin/xcrun");
            install_test_script(
                &script,
                include_str!("../../crates/shoal-daemon/tests/fixtures/simctl.py"),
            );
        }
        // Remove eligible workspaces on their first sweep unless a test sets
        // its own grace period.
        let config = format!(
            "{NO_GRACE}{UNSETTLED}{FORGEJO_HOST}{NO_MEMORY_OVERLOAD}{}",
            config.unwrap_or_default()
        );
        Self(Base::start(
            root,
            &config,
            "repo with ' quotes & $literal",
            &[("tracked", "committed\n"), (".gitignore", "ignored/\n")],
        ))
    }

    // Exercise persisted/manual acknowledgement compatibility without retaining
    // the removed CLI spelling.
    pub(crate) fn acknowledge(&self, workspace: &str) -> Value {
        let space = self.id(workspace);
        self.request(serde_json::json!({"set_pr":{"space":space,"clear":false}}))
    }

    pub(crate) fn request_with_id(&self, id: u64, method: Value) -> Value {
        let protocol = self.protocol();
        self.raw_request(protocol, id, method)
    }

    pub(crate) fn protocol(&self) -> u64 {
        support::daemon_client::protocol(self.root.path())
    }

    pub(crate) fn raw_request(&self, protocol: u64, id: u64, method: Value) -> Value {
        support::daemon_client::exchange(
            self.root.path(),
            &serde_json::json!({"protocol": protocol, "id": id, "method": method}),
        )
    }

    /// The daemon as an app reaches it through `shoal_client`, as the user.
    pub(crate) fn connector(&self) -> shoal_client::connection::Connector {
        let state = self.root.path().join("state");
        shoal_client::connection::Connector {
            paths: shoal_client::paths::Paths {
                home: self.root.path().into(),
                socket: state.join("daemon.sock"),
                state,
            },
            scope: None,
        }
    }

    /// Write `file`'s contents as repository `repository`'s local config, as
    /// a user edits it, and return what `repo config` then shows.
    pub(crate) fn save_local_config(&self, repository: &str, file: &Path) -> Value {
        let path = PathBuf::from(
            self.ok(&["repo", "config", repository])["path"]
                .as_str()
                .unwrap(),
        );
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy(file, &path).unwrap();
        self.ok(&["repo", "config", repository])
    }

    /// Remove repository `repository`'s local config file.
    pub(crate) fn clear_local_config(&self, repository: &str) -> Value {
        let shown = self.ok(&["repo", "config", repository]);
        let path = Path::new(shown["path"].as_str().unwrap());
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
        self.ok(&["repo", "config", repository])
    }

    pub(crate) fn rename(&self, workspace: &str, branch: &str) -> Value {
        let workspace = self.id(workspace);
        self.request(
            serde_json::json!({"rename_workspace":{"workspace":workspace,"branch":branch}}),
        )
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

    /// Answer forge requests through the scripted `curl` with `responses`,
    /// as tests/forge.rs does; later calls replace the responses.
    pub(crate) fn script_github(&self, responses: &serde_json::Map<String, Value>) {
        support::forge::respond(self.root.path(), &Value::from(responses.clone()));
    }

    pub(crate) fn add(&self, name: &str) -> Value {
        self.ok(&["start", self.repo.to_str().unwrap(), "--branch", name])
    }

    /// The ID of the agent queue's entry named `name`, which queue commands
    /// select it by, since a name never does.
    pub(crate) fn queued(&self, name: &str) -> String {
        let entries = self.ok(&["queue"]);
        let entry = (entries.as_array().unwrap().iter()).find(|entry| entry["name"] == name);
        entry.map_or_else(
            || name.to_owned(),
            |entry| entry["id"].as_str().unwrap().to_owned(),
        )
    }

    /// `~/shoal`, the default parent of every repository directory.
    pub(crate) fn shoal_dir(&self) -> PathBuf {
        fs::canonicalize(self.root.path().join("shoal")).unwrap()
    }

    pub(crate) fn interactive(&self, args: &[&str], answer: &str) -> (Output, String) {
        self.interactive_command(self.command().args(args), answer)
    }

    pub(crate) fn interactive_command(
        &self,
        command: &mut Command,
        answer: &str,
    ) -> (Output, String) {
        self.terminal_command(command, answer, false)
    }

    /// Run `command` as `interactive_command` does, with stdout on the
    /// terminal too when `terminal_stdout` is set.
    pub(crate) fn terminal_command(
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
    /// the rows whose IDs `choice` lists, separated by spaces, then type
    /// `answer` into later prompts.
    pub(crate) fn pick(&self, args: &[&str], choice: &str, answer: &str) -> (Output, String) {
        self.pick_in(self.root.path(), args, choice, answer)
    }

    /// `pick` run from `cwd`.
    pub(crate) fn pick_in(
        &self,
        cwd: &Path,
        args: &[&str],
        choice: &str,
        answer: &str,
    ) -> (Output, String) {
        let bin = self.root.path().join("bin");
        let fzf = bin.join("fzf");
        install_test_script(
            &fzf,
            "#!/bin/sh\ncat > \"$SHOAL_TEST_PICK_INPUT\"\ncase \"$*\" in *--expect=*) printf '\\n';; esac\nawk -F '\\t' -v ids=\" $SHOAL_TEST_PICK_ID \" 'index(ids, \" \" $1 \" \") { print }' \"$SHOAL_TEST_PICK_INPUT\"\n",
        );
        let rows = self.root.path().join("picker-rows");
        let _ = fs::remove_file(&rows);
        let (output, _) = self.terminal_command(
            self.command()
                .current_dir(cwd)
                .env("SHOAL_TEST_PICK_INPUT", &rows)
                .env("SHOAL_TEST_PICK_ID", choice)
                .args(args),
            answer,
            matches!(args.first(), Some(&"approvals" | &"questions")),
        );
        (output, fs::read_to_string(&rows).unwrap_or_default())
    }

    /// Save an interrupted command for `shoal resume`, as `shoal stop` does.
    pub(crate) fn save_stopped_command(&self, workspace: &Value) {
        let state = self
            .root
            .path()
            .join("state/workspaces")
            .join(workspace["id"].as_str().unwrap());
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("saved.command.json"), r#"{"argv":["true"]}"#).unwrap();
    }

    /// Interactive commands open pickers for whatever agents and fzf they find,
    /// so they see only the fixture's `bin`, the runner's Git, and system
    /// directories.
    pub(crate) fn interactive_path(&self) -> String {
        let tools = self.root.path().join("runner-bin");
        if !tools.exists() {
            fs::create_dir(&tools).unwrap();
            link_runner_program(&tools, "git");
            install_test_script(tools.join("zmx"), PASSTHROUGH_ZMX);
        }
        format!(
            "{}:{}:{}",
            self.root.path().join("bin").display(),
            tools.display(),
            self.system_path()
        )
    }

    /// `bin`, the runner's Git, which setup uses to copy ignored files, and
    /// system directories.
    pub(crate) fn restricted_path(&self, bin: &Path) -> String {
        let tools = self.root.path().join("git-bin");
        if !tools.exists() {
            fs::create_dir(&tools).unwrap();
            link_runner_program(&tools, "git");
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
    pub(crate) fn system_path(&self) -> String {
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
pub(crate) fn link_runner_program(tools: &Path, program: &str) {
    let path = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join(program))
        .find(|path| {
            path.metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .unwrap_or_else(|| panic!("{program} is not on PATH"));
    std::os::unix::fs::symlink(path, tools.join(program)).unwrap();
}

/// Agent limits under which a start or a queued restore runs at once, whatever
/// the load on the machine running the tests, with the fixture's
/// [`NO_MEMORY_OVERLOAD`].
pub(crate) const UNLIMITED_AGENTS: &str =
    "machine.max_agents = 'none'\n[machine.overload.disk]\nenabled = false\n";

/// The lines `stream` carries, as a reader thread receives them.
pub(crate) fn lines(
    stream: impl std::io::Read + Send + 'static,
) -> std::sync::mpsc::Receiver<String> {
    let (send, receive) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for line in std::io::BufRead::lines(std::io::BufReader::new(stream)).map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    receive
}

pub(crate) fn commit_resource_config(repo: &Path, config: &str) {
    fs::write(repo.join(".shoal.toml"), config).unwrap();
    git(repo, &["add", "."]);
    git_with_identity(repo, &["commit", "-m", "resources"]);
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
    git_with_identity(&author, &["commit", "-m", "upstream change"]);
    git(&author, &["push", "origin", branch]);
    author
}

pub(crate) fn wait_registered_execution(fixture: &Fixture, workspace: &str) -> Value {
    let workspace = fixture.id(workspace);
    wait_for("registered execution with a child", || {
        let inspection = fixture.ok(&["status", &workspace]);
        inspection["executions"]
            .as_array()
            .unwrap()
            .first()
            .filter(|execution| !execution["child"].is_null())
            .cloned()
    })
}

pub(crate) fn commit_file(path: &Path, file: &str, contents: &str) {
    fs::write(path.join(file), contents).unwrap();
    git(path, &["add", file]);
    git_with_identity(path, &["commit", "-m", file]);
}

/// Whether `pid` runs: an orphan's zombie counts as ended, since a
/// container's init may never reap it.
pub(crate) fn process_running(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&output.stdout);
    !(status.trim().is_empty() || status.trim().starts_with('Z'))
}

/// Run Shoal's `internal <command>` hook scoped to `workspace` with `input`
/// on stdin, as an agent's hook runs, and return what it printed.
pub(crate) fn scoped_hook(
    fixture: &Fixture,
    workspace: &str,
    command: &str,
    input: Value,
) -> String {
    let workspace = fixture.id(workspace);
    let mut child = fixture
        .command()
        .args(["run", "--workspace", &workspace, "--"])
        .args([support::shoal_binary(), "internal", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{command}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

pub(crate) fn scoped_command(fixture: &Fixture, workspace: &str, args: &[&str]) -> Output {
    let workspace = fixture.id(workspace);
    fixture
        .command()
        .args([
            "run",
            "--workspace",
            &workspace,
            "--",
            support::shoal_binary(),
            "--json",
        ])
        .args(args)
        .output()
        .unwrap()
}

pub(crate) fn set_repository_toml(fixture: &Fixture, text: &str) {
    let config = fixture.root.path().join("repository.toml");
    fs::write(&config, text).unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &config);
}

/// Agent launches need their executables on PATH: interactive commands limit
/// theirs to the fixture's `bin`, and the daemon, which checks a launch's
/// agents before it starts, has that `bin` but not a test's own stubs.
pub(crate) fn install_fake_agents(fixture: &Fixture, agents: &[&str]) {
    for agent in agents {
        install_test_script(fixture.root.path().join("bin").join(agent), "#!/bin/sh\n");
    }
}
