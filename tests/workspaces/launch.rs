//! Commands a test starts in a workspace and waits on, reporting what each
//! printed when one never starts.
use super::*;

/// A command started in a test's workspace, which writes its stderr beside
/// the fixture and touches `marker` once it runs.
pub(crate) struct Launch {
    name: &'static str,
    /// The agent its execution records, `None` for a plain command.
    agent: Option<&'static str>,
    child: Child,
    stderr: PathBuf,
    marker: PathBuf,
}

impl Launch {
    fn spawn(
        fixture: &Fixture,
        name: &'static str,
        agent: Option<&'static str>,
        args: &[&str],
        marker: PathBuf,
    ) -> Self {
        let stderr = fixture.root.path().join(format!("{name}.stderr"));
        let child = fixture
            .command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        Self {
            name,
            agent,
            child,
            stderr,
            marker,
        }
    }

    /// Start the workspace's own `claude` agent in `path`, whose fake agent
    /// touches `agent-started`, and at once the command `sh -c script`,
    /// which touches `command-started`, and wait until both run.
    pub(crate) fn agent_and_command(
        fixture: &Fixture,
        workspace: &str,
        path: &Path,
        script: &str,
    ) -> [Self; 2] {
        let mut launches = [
            Self::spawn(
                fixture,
                "agent",
                Some("claude"),
                &["start", "--in", workspace, "--here", "--agent", "claude"],
                path.join("agent-started"),
            ),
            Self::spawn(
                fixture,
                "command",
                None,
                &["run", "--workspace", workspace, "--", "sh", "-c", script],
                path.join("command-started"),
            ),
        ];
        Self::wait_started(fixture, workspace, &mut launches);
        launches
    }

    /// Wait until the daemon records every launch's execution with its child
    /// and each has touched its marker, the state a restart or removal of the
    /// workspace acts on.
    /// A launch that exits first fails the test at once, and one that never
    /// starts fails it at the hang guard, naming it with what each printed.
    fn wait_started(fixture: &Fixture, workspace: &str, launches: &mut [Self]) {
        let mut executions = Value::Null;
        let started = try_wait_for(|| {
            if launches.iter_mut().any(|launch| launch.exited().is_some()) {
                return Some(false);
            }
            executions = fixture.ok(&["status", workspace])["executions"].clone();
            launches
                .iter()
                .all(|launch| launch.started(&executions))
                .then_some(true)
        });
        if started == Some(true) {
            return;
        }
        let report: Vec<_> = launches
            .iter_mut()
            .map(|launch| launch.report(&executions))
            .collect();
        for launch in launches {
            let _ = launch.child.kill();
        }
        panic!(
            "executions did not start; recorded: {executions}\n{}",
            report.join("\n")
        );
    }

    /// Whether the daemon records this launch's execution with its child and
    /// the launch touched its marker.
    fn started(&self, executions: &Value) -> bool {
        self.recorded(executions) && self.marker.exists()
    }

    fn recorded(&self, executions: &Value) -> bool {
        executions.as_array().is_some_and(|executions| {
            executions.iter().any(|execution| {
                execution.get("agent").and_then(Value::as_str) == self.agent
                    && !execution["child"].is_null()
            })
        })
    }

    pub(crate) fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().unwrap()
    }

    /// Whether this launch started, how it ended if it did, and its stderr.
    fn report(&mut self, executions: &Value) -> String {
        let exit = match self.exited() {
            Some(status) => format!("exited with {status}"),
            None => "still running".to_owned(),
        };
        format!(
            "{}: recorded {}, marker {}, {exit}; stderr:\n{}",
            self.name,
            self.recorded(executions),
            self.marker.exists(),
            self.printed(),
        )
    }

    /// What the launch wrote to stderr so far.
    pub(crate) fn printed(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// Wait for the launch to end, failing at the hang guard with what it
    /// printed.
    pub(crate) fn wait_exited(&mut self) -> std::process::ExitStatus {
        try_wait_for(|| self.exited()).unwrap_or_else(|| {
            let _ = self.child.kill();
            panic!("{} did not exit; stderr:\n{}", self.name, self.printed())
        })
    }
}
