//! `shoal restart` starts an agent's session again in a new wrapper, which the
//! agent queue starts where the agent ran.
use super::*;

/// The old wrapper's process tree cannot start the agent, as one that lost
/// access to the workspace's volume cannot. The new wrapper, which the agent
/// queue starts in a detached zmx session, can, and the old one finishes.
#[test]
fn restart_starts_the_agent_in_a_new_wrapper_where_the_old_one_could_not() {
    let fixture = Fixture::with_config(Some(&format!(
        "{UNLIMITED_AGENTS}[agents.claude]\ncommand = ['sh', './agent.sh']\nresume_command = ['sh', './resume.sh']\n"
    )));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    let workspace = fixture.add("renewed");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(
        path.join("agent.sh"),
        "trap 'exit 0' TERM\ntouch agent-started\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::write(
        path.join("resume.sh"),
        "if [ -n \"$BROKEN_TREE\" ]; then exit 1; fi\n\
         trap 'exit 0' TERM\ntouch restored\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    let mut agent = Running {
        fixture: &fixture,
        agent: fixture
            .command()
            .env("BROKEN_TREE", "1")
            .args(["claude", "renewed"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    };
    wait_until("agent to start", || path.join("agent-started").exists());
    let old = wait_registered_execution(&fixture, "renewed")["id"].clone();

    let restarted = fixture.ok(&["restart", "renewed"]);
    assert_eq!(
        restarted["restarted"],
        serde_json::json!([{"workspace": "renewed", "agent": "claude"}]),
        "{restarted}"
    );
    wait_until("agent to restore", || path.join("restored").exists());
    // The old wrapper finished, and a new one runs the agent.
    assert!(
        try_wait_for(|| agent.agent.try_wait().unwrap()).is_some(),
        "the old wrapper still runs"
    );
    let new = wait_registered_execution(&fixture, "renewed")["id"].clone();
    assert_ne!(new, old);
    let alerts = fixture.ok(&["alerts", "--all"]);
    assert!(!alerts.to_string().contains("exited"), "{alerts}");
    fixture.ok(&["stop", "renewed"]);
    wait_zmx_sessions_ended(fixture.root.path());
}

/// The first wrapper of the workspace `renewed`, which the daemon stops when
/// the test ends, so a test that fails first leaves nothing running.
struct Running<'a> {
    fixture: &'a Fixture,
    agent: Child,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let _ = self.fixture.command().args(["stop", "renewed"]).output();
        if try_wait_for(|| self.agent.try_wait().ok().flatten()).is_none() {
            let _ = self.agent.kill();
            let _ = self.agent.wait();
        }
    }
}
