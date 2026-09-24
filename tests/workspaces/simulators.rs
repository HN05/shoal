use crate::support::Fixture;
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::Path,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
pub(super) const SIM_CONFIG: &str = r#"
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
    let mut waiting = fixture
        .command()
        .args(["--json", "sim", "acquire", "second", "--wait", "10"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(150));
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
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let sims = fixture.ok(&["sim", "--all"]);
        if sims["simulators"].as_array().unwrap().len() == 1 {
            assert_eq!(sims["simulators"][0]["udid"], second["udid"]);
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
