//! An upgrade replaces the binary that running agents' wrappers started from.
use super::*;

#[test]
fn restart_continues_an_upgraded_wrapper_from_the_installed_binary() {
    let fixture = Fixture::with_config(Some(
        "[commands]\nclaude = ['sh', './agent.sh']\n[agent_resume]\nclaude = ['sh', './resume.sh']\n",
    ));
    let workspace = fixture.add("upgraded");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(
        path.join("agent.sh"),
        "touch agent-started\ntrap 'exit 0' TERM\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::write(
        path.join("resume.sh"),
        "touch restored\ntrap 'exit 0' TERM\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    // The wrapper starts from an installed path, as Homebrew's link is.
    let installed = fixture.root.path().join("installed/shoal");
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_shoal"), &installed).unwrap();
    let mut agent = Stopped {
        fixture: &fixture,
        agent: support::isolated(fixture.root.path(), &installed)
            .args(["claude", "upgraded"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    };
    wait_until("agent to start", || path.join("agent-started").exists());
    // The upgrade puts another file at the path, here one that runs the same
    // release.
    let upgrade = installed.with_file_name("shoal.new");
    fs::write(
        &upgrade,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", env!("CARGO_BIN_EXE_shoal")),
    )
    .unwrap();
    fs::set_permissions(&upgrade, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&upgrade, &installed).unwrap();
    fixture.ok(&["restart", "upgraded"]);
    wait_until("agent to restore", || path.join("restored").exists());
    // The wrapper's process runs the installed binary, which registered the
    // execution again and runs the restored agent.
    let wrapper = Command::new("ps")
        .args(["-o", "command=", "-p", &agent.agent.id().to_string()])
        .output()
        .unwrap();
    let wrapper = String::from_utf8_lossy(&wrapper.stdout);
    assert!(wrapper.contains(" internal take-over "), "{wrapper}");
    // The daemon still stops it as the execution's wrapper.
    fixture.ok(&["stop", "upgraded"]);
    assert!(agent.agent.wait().unwrap().success());
}

/// The wrapper of the workspace `upgraded`, which the daemon stops when the
/// test ends, so a test that fails first leaves nothing running.
struct Stopped<'a> {
    fixture: &'a Fixture,
    agent: Child,
}

impl Drop for Stopped<'_> {
    fn drop(&mut self) {
        // The wrapper ignores SIGTERM; the daemon stops it and its agent.
        let _ = self.fixture.command().args(["stop", "upgraded"]).output();
        if try_wait_for(|| self.agent.try_wait().ok().flatten()).is_none() {
            let _ = self.agent.kill();
            let _ = self.agent.wait();
        }
    }
}
