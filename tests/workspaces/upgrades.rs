//! An upgrade replaces the binary that running agents' wrappers started from.
use super::*;

#[test]
fn restart_in_place_continues_an_upgraded_wrapper_from_the_installed_binary() {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', './agent.sh']\nresume_command = ['sh', './resume.sh']\n",
    ));
    let workspace = fixture.add("upgraded");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(
        path.join("agent.sh"),
        "trap 'exit 0' TERM\ntouch agent-started\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::write(
        path.join("resume.sh"),
        "trap 'exit 0' TERM\ntouch restored\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    // The wrapper starts from an installed link to an earlier release, a copy
    // of the daemon's binary in the target directory, where it may be cloned.
    // A child copies it, so parallel spawns cannot inherit its writable
    // descriptor and cause ETXTBSY when the wrapper executes it.
    let target = Path::new(support::shoal_binary()).parent().unwrap();
    let releases = tempfile::tempdir_in(target).unwrap();
    let earlier = releases.path().join("shoal");
    let copied = support::isolated(releases.path(), "cp")
        .arg(support::shoal_binary())
        .arg(&earlier)
        .output()
        .unwrap();
    assert!(
        copied.status.success(),
        "copy shoal: {}",
        String::from_utf8_lossy(&copied.stderr)
    );
    let installed = fixture.root.path().join("installed/shoal");
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&earlier, &installed).unwrap();
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
    // The upgrade links the release the daemon runs in its place.
    let upgrade = installed.with_file_name("shoal.new");
    std::os::unix::fs::symlink(support::shoal_binary(), &upgrade).unwrap();
    fs::rename(&upgrade, &installed).unwrap();
    fixture.ok(&["restart", "upgraded", "--in-place"]);
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
    symlink(support::shoal_binary(), &launcher).unwrap();
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
        format!("hooks.pre_setup = '{}'\n", hook.display()),
    )
    .unwrap();
    let mut launch = support::isolated(root.path(), &launcher);
    launch
        .args(["daemon", "run", "--managed"])
        .env("SHOAL_TEST_HANDED_OFF", &handed_off)
        .env("SHOAL_TEST_REAL_BINARY", support::shoal_binary())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut daemon = DaemonGuard {
        child: launch.spawn().unwrap(),
        stderr: None,
    };
    let shoal = || cli(root.path());
    wait_until("daemon startup", || {
        assert!(daemon.child.try_wait().unwrap().is_none(), "daemon exited");
        shoal().args(["status"]).output().unwrap().status.success()
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
        .args(["--json", "start", repo.to_str().unwrap(), "before"])
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
    let status = shoal().args(["--json", "status"]).output().unwrap();
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["daemon"]["pid"], daemon.child.id());
    let after = shoal()
        .args(["--json", "start", repo.to_str().unwrap(), "after"])
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
