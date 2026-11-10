//! The `Fixture` every test starts from, an isolated home, repository and
//! daemon with the commands and checks tests run against it, and the helpers
//! tests of several areas share.
use super::*;

pub(crate) struct Fixture {
    // Fields drop in declaration order: the commands stop while the daemon
    // still answers them, and what left their group stops before the
    // directory goes.
    pub(crate) commands: CommandGroup,
    pub(crate) daemon: DaemonGuard,
    pub(crate) _detached: Detached,
    pub(crate) root: TempDir,
    pub(crate) repo: PathBuf,
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

/// Global config without overload warnings, which would add a message to
/// each running agent's workspace whenever the runner is short of disk space,
/// memory or CPU. The daemon's unit tests cover the warnings.
pub(crate) const NO_OVERLOAD_WARNINGS: &str = "machine.overload.warning.enabled = false\n";

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
        static WORKTRUNK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        assert!(
            *WORKTRUNK.get_or_init(|| Command::new("wt").arg("--version").output().is_ok()),
            "workspace integration tests require Worktrunk (wt)"
        );
        let root = tempfile::tempdir_in("/tmp").unwrap();
        // Debug builds only open existing native ownership locks. Prepare the
        // isolated zmx namespace for terminal launch and cleanup tests.
        fs::write(root.path().join("zmx.shoal-ownership.lock"), "")
            .expect("prepare isolated zmx ownership lock");
        // Remove eligible workspaces on their first sweep unless a test sets
        // its own grace period.
        fs::create_dir_all(root.path().join(".config/shoal")).unwrap();
        fs::write(
            root.path().join(".config/shoal/config.toml"),
            format!(
                "{NO_GRACE}{UNSETTLED}{FORGEJO_HOST}{NO_OVERLOAD_WARNINGS}{NO_MEMORY_OVERLOAD}{}",
                config.unwrap_or_default()
            ),
        )
        .unwrap();
        if fake_sim {
            let script = root.path().join("bin/xcrun");
            install_test_script(&script, include_str!("../fixtures/simctl.py"));
        }
        // Forge requests reach only the scripted forge, which answers none
        // until a test scripts it.
        fs::create_dir_all(root.path().join("forge")).unwrap();
        fs::create_dir_all(root.path().join("bin")).unwrap();
        fs::write(root.path().join("forge/responses.json"), "{}").unwrap();
        install_test_script(
            root.path().join("forge.py"),
            include_str!("../fixtures/forge.py"),
        );
        std::os::unix::fs::symlink(root.path().join("forge.py"), root.path().join("bin/curl"))
            .unwrap();
        let repo = init_repo(
            root.path(),
            "repo with ' quotes & $literal",
            &[("tracked", "committed\n"), (".gitignore", "ignored/\n")],
        );
        // Model a starting commit already present on a remote, without network I/O.
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        // Fetches over SSH, as the daemon's background sync of a GitHub
        // origin a test adds, fail here without reaching the network; a test
        // that serves SSH itself sets its own command.
        git(&repo, &["config", "core.sshCommand", "false"]);
        let daemon = DaemonGuard::start(root.path(), &mut daemon_launch(root.path()));
        let fixture = Self {
            commands: CommandGroup::new(),
            _detached: Detached::new(root.path().join("state")),
            root,
            repo,
            daemon,
        };
        fixture.ok(&["repo", "add", fixture.repo.to_str().unwrap()]);
        fixture
    }

    // Exercise persisted/manual acknowledgement compatibility without retaining
    // the removed CLI spelling.
    pub(crate) fn acknowledge(&self, workspace: &str) -> Value {
        self.request(serde_json::json!({"set_pr":{"space":workspace,"clear":false}}))
    }

    pub(crate) fn request(&self, method: Value) -> Value {
        self.request_with_id(1, method)
    }

    pub(crate) fn request_with_id(&self, id: u64, method: Value) -> Value {
        let protocol = self.protocol();
        self.raw_request(protocol, id, method)
    }

    pub(crate) fn protocol(&self) -> u64 {
        support::daemon_client::protocol(self.root.path())
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

    pub(crate) fn raw_request(&self, protocol: u64, id: u64, method: Value) -> Value {
        support::daemon_client::exchange(
            self.root.path(),
            &serde_json::json!({"protocol": protocol, "id": id, "method": method}),
        )
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
        fs::write(
            self.root.path().join("forge/responses.json"),
            Value::from(responses.clone()).to_string(),
        )
        .unwrap();
    }

    pub(crate) fn restart(&mut self) {
        self.daemon
            .restart(self.root.path(), &mut daemon_launch(self.root.path()));
    }

    pub(crate) fn command(&self) -> Command {
        let mut command = cli(self.root.path());
        self.commands.join(&mut command);
        command
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
        self.ok(&["start", self.repo.to_str().unwrap(), name])
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
    /// so they see only the fixture's `bin`, the runner's Git and Worktrunk, and
    /// system directories.
    pub(crate) fn interactive_path(&self) -> String {
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
    pub(crate) fn restricted_path(&self, bin: &Path) -> String {
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

/// How fixtures start their daemon: as the CLI runs, with a `zmx` on the
/// daemon's PATH when the runner has none.
fn daemon_launch(root: &Path) -> Command {
    let mut launch = cli(root);
    daemon_fixture::provide_zmx(root, &mut launch);
    launch
}

pub(crate) fn wait_registered_execution(fixture: &Fixture, workspace: &str) -> Value {
    wait_for("registered execution with a child", || {
        let inspection = fixture.ok(&["status", workspace]);
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
    let mut child = fixture
        .command()
        .args(["run", "--workspace", workspace, "--"])
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
    fixture
        .command()
        .args([
            "run",
            "--workspace",
            workspace,
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

/// Runs a session's command in place of a zmx session, recording each call
/// and serving the sessions listed in `$HOME/zmx-sessions`. A detached
/// session keeps the environment it was created with and runs sent text as
/// its shell would, logging the output to `$HOME/zmx-<name>.log`, and stays
/// listed, with the labels set on it, once `$HOME/zmx-keep-sessions` exists.
/// `$HOME/zmx-<name>.running` exists while the sent text runs.
/// An attached session ends once `$HOME/zmx-attach-ends-session` exists, and
/// one an attach creates stays listed after its command, as after a detach,
/// once `$HOME/zmx-attach-detaches` exists.
/// Calls from a place watcher go to `$HOME/zmx-watch-calls`, out of the
/// recorded actions.
pub(crate) const PASSTHROUGH_ZMX: &str = r#"#!/usr/bin/env python3
import fcntl, json, os, subprocess, sys, time
from pathlib import Path
root = Path(os.environ.get('ZMX_TEST_ROOT', os.environ['HOME']))
args = sys.argv[1:]
def rewrite_sessions(lines):
    # Replace the listing whole, as zmx's server answers: a list run
    # meanwhile must never find it truncated, which reads as no sessions.
    sessions = root / 'zmx-sessions'
    staged = sessions.with_name(f'zmx-sessions.{os.getpid()}')
    staged.write_text(''.join(line + '\n' for line in lines))
    os.replace(staged, sessions)
if args[:1] == ['worker']:
    # Keep a readable parent for macOS's hidden shell environment, so other
    # fixtures can attribute it while verifying their setup processes.
    try:
        sys.exit(subprocess.run(['sh', '-c', args[1]]).returncode)
    finally:
        (root / f"zmx-{os.environ['ZMX_SESSION']}.running").unlink(missing_ok=True)
parent = subprocess.run(['ps', '-o', 'args=', '-p', str(os.getppid())],
    capture_output=True, text=True).stdout
log = 'zmx-watch-calls' if ' place-watch ' in parent else 'zmx-calls'
with (root / log).open('a') as calls:
    calls.write(json.dumps(args) + '\n')
if args[:1] == ['set'] and (root / 'zmx-label-fails').exists():
    sys.exit('ownership update failed')
if args[:1] == ['set'] and (root / 'zmx-keep-sessions').exists():
    sessions = root / 'zmx-sessions'
    lines = []
    found = False
    for line in sessions.read_text().splitlines():
        fields = dict(field.split('=', 1) for field in line.strip().split('\t'))
        if fields.get('name') == args[1]:
            found = True
            for assignment in args[2:]:
                key, value = assignment.split('=', 1)
                if value:
                    fields[key] = value
                else:
                    fields.pop(key, None)
            line = '\t'.join(f'{key}={value}' for key, value in fields.items())
        lines.append(line)
    if not found:
        sys.exit('session not found')
    rewrite_sessions(lines)
elif args[:1] == ['kill'] and (root / 'zmx-close-removes-session').exists():
    rewrite_sessions(line for line in (root / 'zmx-sessions').read_text().splitlines()
        if not any(field == f'name={args[1]}' for field in line.strip().split('\t')))
elif args[:1] in (['set'], ['kill']):
    pass
elif args[:1] == ['run']:
    (root / f'zmx-{args[1]}.env').write_text(json.dumps(dict(os.environ)))
    if (root / 'zmx-keep-sessions').exists():
        with (root / 'zmx-sessions').open('a') as sessions:
            sessions.write(f'  name={args[1]}\tpid=1\n')
elif args[:1] == ['send'] and not (root / f'zmx-{args[1]}.env').exists():
    # An attached session's agent reads what is typed; record that it was.
    (root / 'zmx-sent').touch()
elif args[:1] == ['send']:
    env = json.loads((root / f'zmx-{args[1]}.env').read_text())
    env['ZMX_SESSION'] = args[1]
    with (root / f'zmx-{args[1]}.log').open('a') as log:
        # Native send returns after writing input, before the worker labels
        # its session. Waiting here would deadlock on the handoff's lock.
        (root / f'zmx-{args[1]}.running').touch()
        subprocess.Popen([sys.executable, __file__, 'worker', args[2].rstrip('\r')], env=env,
            start_new_session=True,
            stdin=subprocess.DEVNULL, stdout=log, stderr=log)
elif args[:1] == ['version']:
    (root / 'zmx-version-called').touch()
    print(f'socket_dir\t{root / "zmx"}')
elif args[:1] == ['list']:
    sessions = root / 'zmx-sessions'
    listed = sessions.read_text() if sessions.exists() else 'no sessions found'
    if (root / 'zmx-pause-owner-check').exists():
        with (root / 'zmx.shoal-ownership.lock').open() as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                (root / 'zmx-pause-owner-check').unlink()
                (root / 'zmx-owner-check-paused').touch()
                deadline = time.monotonic() + 120
                while not (root / 'zmx-owner-check-release').exists():
                    if time.monotonic() >= deadline:
                        sys.exit('ownership check hang guard')
                    time.sleep(0.01)
    print(listed)
elif args[:1] == ['attach']:
    args = args[1:]
    if args[:1] == ['--labels']:
        args = args[2:]
    if (root / 'zmx-attach-ends-session').exists():
        rewrite_sessions(line for line in (root / 'zmx-sessions').read_text().splitlines()
            if not any(field == f'name={args[0]}' for field in line.strip().split('\t')))
    if len(args) > 1 and (root / 'zmx-attach-detaches').exists():
        with (root / 'zmx-sessions').open('a') as sessions:
            sessions.write(f'  name={args[0]}\tpid=1\n')
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

/// Agent launches need their executables on PATH, which interactive commands
/// limit to the fixture's `bin`.
pub(crate) fn install_fake_agents(fixture: &Fixture, agents: &[&str]) {
    for agent in agents {
        install_test_script(fixture.root.path().join("bin").join(agent), "#!/bin/sh\n");
    }
}
