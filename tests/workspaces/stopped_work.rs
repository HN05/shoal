//! What a stop saves of a workspace's agents and commands, and how
//! `shoal resume` restores it or `--discard` forgets it.
use super::*;

/// The workspace's agent runs `agent.sh`, and its restore `resume.sh` with
/// the handoff prompt.
const AGENT: &str = "[agents.claude]\ncommand = ['sh', './agent.sh']\nresume_command = ['sh', './resume.sh', '{prompt}']\n";

/// A tracked command that runs until stopped.
const COMMAND: &str = "trap 'exit 0' TERM; touch command-started; while :; do sleep 1; done";

/// A workspace whose agent reports the session its `session` file names,
/// if any, as Claude Code's hooks do, then runs until stopped or until
/// `exit` exists, and whose restore writes its prompt to `restored`.
struct Stopped {
    name: &'static str,
    path: PathBuf,
    /// The daemon's state directory for the workspace, which keeps what a
    /// stop saves.
    state: PathBuf,
}

impl Stopped {
    fn new(fixture: &Fixture, name: &'static str) -> Self {
        let workspace = fixture.add(name);
        let path = PathBuf::from(workspace["path"].as_str().unwrap());
        fs::write(
            path.join("agent.sh"),
            format!(
                "if [ -e session ]; then printf '{{\"session_id\":\"%s\"}}' \"$(cat session)\" | '{}' internal agent-state idle >/dev/null || exit 3; fi\n\
                 trap 'exit 0' TERM\ntouch started\nwhile [ ! -e exit ]; do sleep 1; done\n",
                support::shoal_binary()
            ),
        )
        .unwrap();
        fs::write(path.join("resume.sh"), "printf '%s' \"$1\" > restored\n").unwrap();
        let state = fixture
            .root
            .path()
            .join("state/workspaces")
            .join(workspace["id"].as_str().unwrap());
        Self { name, path, state }
    }

    /// Start the workspace's agent and wait until it runs.
    fn start_agent(&self, fixture: &Fixture) -> Child {
        let _ = fs::remove_file(self.path.join("started"));
        let agent = spawn(fixture.command().args(["claude", self.name]));
        self.wait_running(fixture, "started");
        agent
    }

    /// Start [`COMMAND`] in the workspace and wait until it runs.
    fn start_command(&self, fixture: &Fixture) -> Child {
        let command = spawn(fixture.command().args([
            "run",
            "--workspace",
            self.name,
            "--",
            "sh",
            "-c",
            COMMAND,
        ]));
        self.wait_running(fixture, "command-started");
        command
    }

    /// Wait until the child has written `marker`, so its TERM trap is set,
    /// and the daemon has recorded the process group of every execution. The
    /// marker alone can appear before the wrapper's start report is
    /// recorded, and a stop sent then reaches an execution whose launch is
    /// unproven.
    fn wait_running(&self, fixture: &Fixture, marker: &str) {
        wait_until(marker, || {
            self.path.join(marker).is_file()
                && self
                    .executions(fixture)
                    .iter()
                    .all(|execution| !execution["group_id"].is_null())
        });
    }

    fn executions(&self, fixture: &Fixture) -> Vec<Value> {
        let status = fixture.ok(&["status", self.name]);
        status["executions"].as_array().unwrap().clone()
    }

    /// The execution of the workspace's running agent.
    fn agent_execution(&self, fixture: &Fixture) -> String {
        let executions = self.executions(fixture);
        let agent = executions
            .iter()
            .find(|execution| execution["agent"] == "claude")
            .unwrap_or_else(|| panic!("no running agent: {executions:?}"));
        agent["id"].as_str().unwrap().to_owned()
    }

    /// Stop the workspace with `shoal stop` and wait for `children` to end.
    fn stop(&self, fixture: &Fixture, children: Vec<Child>) {
        fixture.ok(&["stop", self.name]);
        for child in children {
            child.wait_with_output().unwrap();
        }
    }

    /// Keep the agent `shoal stop` left as the last session of `execution`
    /// as stopped work, as a stop for `reason` other than `shoal stop`
    /// saves it.
    fn keep_as_stopped_work(&self, execution: &str, reason: &str) {
        fs::remove_file(self.state.join(format!("{execution}.last-session.json"))).unwrap();
        self.save_record(execution, reason);
    }

    /// Save the record of the agent of `execution`, stopped for `reason`.
    fn save_record(&self, execution: &str, reason: &str) {
        let record = serde_json::json!({
            "agent": "claude", "stop_reason": reason, "stage": "stopped", "mode": "worker",
        });
        fs::create_dir_all(&self.state).unwrap();
        fs::write(self.record(execution), record.to_string()).unwrap();
    }

    fn record(&self, execution: &str) -> PathBuf {
        self.state.join(format!("{execution}.recovery.json"))
    }

    /// The files of the saved work whose names end with `suffix`.
    fn saved(&self, suffix: &str) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&self.state) else {
            return Vec::new();
        };
        entries
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().ends_with(suffix))
            .collect()
    }

    /// Whether stopped agents or commands wait for `shoal resume`.
    fn pending(&self) -> bool {
        !self.saved(".recovery.json").is_empty() || !self.saved(".command.json").is_empty()
    }

    /// The agents' last sessions, as `(agent, session, moved)`.
    fn last_sessions(&self) -> Vec<(Value, Value, bool)> {
        self.saved(".last-session.json")
            .iter()
            .map(|path| {
                let last: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
                let moved = last["moved"].as_bool().unwrap_or(false);
                (last["agent"].clone(), last["session"].clone(), moved)
            })
            .collect()
    }

    /// Run `shoal resume` for the workspace in this process, with `args`.
    fn resume(&self, fixture: &Fixture, args: &[&str]) -> Output {
        fixture
            .command()
            .args(["resume", self.name])
            .args(args)
            .output()
            .unwrap()
    }

    /// The prompt the restore received.
    fn handoff(&self) -> String {
        fs::read_to_string(self.path.join("restored")).unwrap()
    }
}

fn spawn(command: &mut Command) -> Child {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// The handoff names the stopped [`COMMAND`] in a list of its own.
fn names_the_command(handoff: &str) -> bool {
    handoff.contains("\n- sh -c 'trap")
}

#[test]
fn stop_saves_agents_and_commands_preserving_work_and_leases() {
    let fixture = Fixture::with_config(Some(AGENT));
    let workspace = Stopped::new(&fixture, "stopped");
    fixture.ok(&["acquire", "port", "web", "--workspace", "stopped"]);
    fs::write(workspace.path.join("unfinished"), "work in progress").unwrap();
    let before = fixture.ok(&["status", "stopped"]);
    let agent = workspace.start_agent(&fixture);
    let command = workspace.start_command(&fixture);
    workspace.stop(&fixture, vec![agent, command]);

    let after = fixture.ok(&["status", "stopped"]);
    assert_eq!(after["workspace"]["state"], before["workspace"]["state"]);
    assert_eq!(after["ports"].as_array().unwrap().len(), 1, "{after}");
    assert!(
        after["executions"].as_array().unwrap().is_empty(),
        "{after}"
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("unfinished")).unwrap(),
        "work in progress"
    );
    let commands = workspace.saved(".command.json");
    let [command] = commands.as_slice() else {
        panic!("{commands:?}");
    };
    let command: Value = serde_json::from_slice(&fs::read(command).unwrap()).unwrap();
    assert_eq!(command["argv"][0], "sh", "{command}");
    assert!(workspace.pending());

    let output = workspace.resume(&fixture, &["--focus"]);
    assert!(output.status.success(), "{output:?}");
    let handoff = workspace.handoff();
    assert!(names_the_command(&handoff), "{handoff}");
    assert!(!workspace.pending());
}

#[test]
fn a_restored_stopped_agent_is_told_the_commands_stopped_with_it() {
    let fixture = Fixture::with_config(Some(AGENT));
    let workspace = Stopped::new(&fixture, "stopped");
    let agent = workspace.start_agent(&fixture);
    let command = workspace.start_command(&fixture);
    let execution = workspace.agent_execution(&fixture);
    workspace.stop(&fixture, vec![agent, command]);
    // A stop other than `shoal stop`, such as a daemon shutdown's, keeps the
    // agent as stopped work.
    workspace.keep_as_stopped_work(&execution, "the Shoal daemon is stopping");
    assert_eq!(
        (
            workspace.saved(".recovery.json").len(),
            workspace.saved(".command.json").len()
        ),
        (1, 1)
    );

    let output = workspace.resume(&fixture, &["--focus"]);
    assert!(output.status.success(), "{output:?}");
    let handoff = workspace.handoff();
    assert!(names_the_command(&handoff), "{handoff}");
    assert!(!workspace.pending());
}

#[test]
fn resume_without_an_agent_reports_stopped_commands_once() {
    let fixture = Fixture::with_config(Some(AGENT));
    let workspace = Stopped::new(&fixture, "stopped");
    let command = workspace.start_command(&fixture);
    workspace.stop(&fixture, vec![command]);
    assert!(workspace.pending());

    let output = workspace.resume(&fixture, &["--focus"]);
    assert!(output.status.success(), "{output:?}");
    assert!(!workspace.pending());
    assert!(!workspace.resume(&fixture, &["--focus"]).status.success());
}

#[test]
fn resume_continues_an_agent_that_exited_on_its_own() {
    let fixture = Fixture::with_config(Some(&format!("{AGENT}[cleanup.done]\nenabled = false\n")));
    let workspace = Stopped::new(&fixture, "stopped");
    // An agent `shoal stop` stops leaves its session as one that exits on its
    // own does.
    fs::write(workspace.path.join("session"), "stopped-session").unwrap();
    let agent = workspace.start_agent(&fixture);
    workspace.stop(&fixture, vec![agent]);
    assert_eq!(
        workspace.last_sessions(),
        [("claude".into(), "stopped-session".into(), false)]
    );
    assert!(!workspace.pending());

    // A new agent replaces it, and leaves its own session when it exits on
    // its own.
    fs::write(workspace.path.join("session"), "quit-session").unwrap();
    let agent = workspace.start_agent(&fixture);
    fs::write(workspace.path.join("exit"), "").unwrap();
    let output = agent.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        workspace.last_sessions(),
        [("claude".into(), "quit-session".into(), false)]
    );
    assert!(!workspace.pending());

    let output = workspace.resume(&fixture, &["--focus"]);
    assert!(output.status.success(), "{output:?}");
    assert!(workspace.path.join("restored").exists());
    // The restored agent exited on its own too; finishing the assignment
    // forgets it.
    assert!(!workspace.last_sessions().is_empty());
    fixture.ok(&["done", "stopped"]);
    assert_eq!(workspace.last_sessions(), []);
    assert!(workspace.path.is_dir());
    assert!(!workspace.resume(&fixture, &["--focus"]).status.success());
}

#[test]
fn discard_clears_pending_recovery_without_launching_an_agent() {
    let fixture = Fixture::with_config(Some(AGENT));
    let workspace = Stopped::new(&fixture, "stopped");
    let agent = workspace.start_agent(&fixture);
    let execution = workspace.agent_execution(&fixture);
    // Protection keeps the record of an agent it stopped while the wrapper
    // still runs; discard leaves an agent whose execution is active.
    workspace.save_record(&execution, "test memory pressure");
    assert!(!workspace.resume(&fixture, &["--discard"]).status.success());
    assert!(workspace.record(&execution).exists());
    fs::remove_file(workspace.record(&execution)).unwrap();
    workspace.stop(&fixture, vec![agent]);
    workspace.keep_as_stopped_work(&execution, "test memory pressure");
    // Discard does not depend on valid current restore configuration.
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agents.claude]\nresume_command = ['./missing', '{diff_base}']\n",
    )
    .unwrap();

    let output = workspace.resume(&fixture, &["--discard"]);
    assert!(output.status.success(), "{output:?}");
    assert!(!workspace.pending());
    assert!(!workspace.path.join("restored").exists());
    assert!(workspace.path.is_dir());
}

#[test]
fn manual_resume_consumes_the_record_on_start_even_if_other_commands_run_or_restore_fails() {
    let fixture = Fixture::with_config(Some(AGENT));
    let workspace = Stopped::new(&fixture, "stopped");
    let agent = workspace.start_agent(&fixture);
    let execution = workspace.agent_execution(&fixture);
    workspace.stop(&fixture, vec![agent]);
    workspace.keep_as_stopped_work(&execution, "test memory pressure");
    fs::write(
        workspace.path.join("resume.sh"),
        "printf changed > restored\nexit 7\n",
    )
    .unwrap();
    let unrelated = workspace.start_command(&fixture);

    let output = workspace.resume(&fixture, &["--focus"]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(workspace.handoff(), "changed");
    assert!(!workspace.record(&execution).exists());
    workspace.stop(&fixture, vec![unrelated]);
}
