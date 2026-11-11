//! Simulator leases: allocation, policies, clean simulators and their audit
//! trail.
use super::chats::journal;
use super::resources::wait_for_busy;
use super::*;

pub(crate) const SIM_CONFIG: &str = r#"
[machine.simulators]
max_booted = 1
max_devices = 2
idle_seconds = 120
default = "phone"
[machine.simulators.profiles.phone]
device = "Phone"
runtime = "iOS Test"
[machine.simulators.profiles.tablet]
device = "Tablet"
runtime = "iOS Test"
"#;

#[test]
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
    let first = fixture.ok(&["acquire", "sim", "--workspace", "worker"]);
    assert_eq!(
        first["runtime"],
        "com.apple.CoreSimulator.SimRuntime.iOS-26-10"
    );
    set_runtimes(true);
    assert_eq!(
        fixture.ok(&["acquire", "sim", "--workspace", "worker"]),
        first
    );
    assert_eq!(
        fixture.ok(&[
            "acquire",
            "sim",
            "--workspace",
            "worker",
            "--device",
            "Phone"
        ]),
        first
    );
    assert_eq!(
        fixture.ok(&[
            "acquire",
            "sim",
            "--workspace",
            "worker",
            "--profile",
            "phone"
        ]),
        first
    );
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "worker",
    ]);
    let next = fixture.ok(&[
        "acquire",
        "sim",
        "--workspace",
        "worker",
        "--device",
        "Phone",
    ]);
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
fn simulator_exclusivity_wait_reuse_scope_and_removal() {
    let fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    fixture.add("first");
    fixture.add("second");
    let first = fixture.ok(&["acquire", "sim", "--workspace", "first"]);
    assert_eq!(first["state"], "leased");
    assert_eq!(fixture.ok(&["status", "first"])["simulators"][0], first);
    assert_eq!(
        fixture.ok(&["acquire", "sim", "--workspace", "first"]),
        first
    );
    let busy = fixture.run(&["--json", "acquire", "sim", "--workspace", "second"]);
    assert_eq!(busy.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&busy.stdout).unwrap()["acquired"],
        false
    );
    fixture.ok(&["alerts"]);
    let mut waiting = fixture
        .command()
        .args([
            "--json",
            "acquire",
            "sim",
            "--workspace",
            "second",
            "--queue",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_busy(&fixture, "second");
    assert!(waiting.try_wait().unwrap().is_none());
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "first",
    ]);
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
        "run",
        "--workspace",
        "first",
        "--",
        support::shoal_binary(),
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "second",
    ]);
    assert!(!scoped.status.success());
    let listed = fixture.run(&[
        "run",
        "--workspace",
        "first",
        "--",
        support::shoal_binary(),
        "--json",
        "leases",
        "sim",
        "--all",
    ]);
    assert_eq!(
        serde_json::from_slice::<Value>(&listed.stdout).unwrap()["simulators"],
        serde_json::json!([])
    );
    fixture.ok(&["rm", "--wait", "first"]);
    assert_eq!(
        fixture.ok(&["leases", "sim", "--all"])["simulators"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.ok(&["rm", "--wait", "second"]);
    assert_eq!(
        fixture.ok(&["leases", "sim", "--all"])["simulators"],
        serde_json::json!([])
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("sim-devices.json")).unwrap(),
        "[]"
    );
}

#[test]
fn simulator_leases_journal_acquisition_and_release() {
    let fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    fixture.add("first");
    let sim = fixture.ok(&["acquire", "sim", "--workspace", "first"]);
    fixture.ok(&["acquire", "sim", "--workspace", "first"]);
    fixture.ok(&["release", "sim", "--workspace", "first"]);
    fixture.ok(&["acquire", "sim", "--workspace", "first"]);
    // Removal deletes the leased device, which releases the lease.
    fixture.ok(&["rm", "--wait", "first"]);
    let events: Vec<_> = journal(&fixture)
        .into_iter()
        .filter(|event| event["simulator"].is_object())
        .collect();
    assert_eq!(
        events
            .iter()
            .map(|event| event["kind"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        [
            "simulator_acquired",
            "simulator_released",
            "simulator_acquired",
            "simulator_released"
        ]
    );
    for event in &events {
        assert_eq!(event["name"], "first");
        assert_eq!(event["simulator"]["name"], "default");
        assert_eq!(event["simulator"]["udid"], sim["udid"]);
        assert_eq!(event["simulator"]["device"], sim["device"]);
    }
}

#[test]
fn simulator_failures_retain_claims_and_restart_never_reassigns_them() {
    let mut fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    fixture.add("first");
    fixture.add("second");
    fs::write(fixture.root.path().join("sim-fail"), "bootstatus").unwrap();
    assert!(
        !fixture
            .run(&["acquire", "sim", "--workspace", "first"])
            .status
            .success()
    );
    assert_eq!(
        fixture.ok(&["leases", "sim", "--workspace", "first"])["simulators"][0]["state"],
        "failed"
    );
    fs::remove_file(fixture.root.path().join("sim-fail")).unwrap();
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "first",
    ]);
    let lease = fixture.ok(&["acquire", "sim", "--workspace", "first"]);
    fixture.daemon.kill();
    fixture.restart();
    assert_eq!(
        fixture.ok(&["acquire", "sim", "--workspace", "first"]),
        lease
    );
    assert_eq!(
        fixture
            .run(&["acquire", "sim", "--workspace", "second"])
            .status
            .code(),
        Some(2)
    );
    fs::write(fixture.root.path().join("sim-fail"), "delete").unwrap();
    assert!(!fixture.run(&["rm", "--wait", "first"]).status.success());
    assert_eq!(
        fixture.ok(&["leases", "sim", "--workspace", "first"])["simulators"][0]["udid"],
        lease["udid"]
    );
    fs::remove_file(fixture.root.path().join("sim-fail")).unwrap();
    fixture.ok(&["rm", "--wait", "first"]);
    assert_eq!(
        fixture.ok(&["leases", "sim", "--all"])["simulators"],
        serde_json::json!([])
    );
}

#[test]
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
        let output = fixture.run(&["acquire", "sim", "--workspace", "worker"]);
        assert!(!output.status.success());
        let failed =
            fixture.ok(&["leases", "sim", "--workspace", "worker"])["simulators"][0].clone();
        assert_eq!(failed["state"], "failed");

        fs::write(
            &overrides,
            serde_json::json!({"shutdown": native}).to_string(),
        )
        .unwrap();
        let output = fixture.run(&[
            "release",
            "sim",
            "--name",
            "default",
            "--workspace",
            "worker",
        ]);
        assert!(!output.status.success());
        assert_eq!(
            fixture.ok(&["leases", "sim", "--workspace", "worker"])["simulators"][0]["udid"],
            failed["udid"]
        );
        let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
        assert!(!events.contains(&format!("[\"delete\", {}]", failed["udid"])));
        fs::remove_file(&overrides).unwrap();
        fixture.ok(&[
            "release",
            "sim",
            "--name",
            "default",
            "--workspace",
            "worker",
        ]);
    }

    let lease = fixture.ok(&["acquire", "sim", "--workspace", "worker"]);
    let devices_path = fixture.root.path().join("sim-devices.json");
    let mut devices: Value =
        serde_json::from_str(&fs::read_to_string(&devices_path).unwrap()).unwrap();
    for native in ["Shutdown", "Booting", "Shutting Down", "Future State"] {
        devices[0]["state"] = serde_json::json!(native);
        fs::write(&devices_path, devices.to_string()).unwrap();
        let output = fixture.run(&["acquire", "sim", "--workspace", "worker"]);
        assert!(!output.status.success());
        assert_eq!(
            fixture.ok(&["leases", "sim", "--workspace", "worker"])["simulators"][0],
            lease
        );
    }
    devices[0]["state"] = serde_json::json!("Booted");
    fs::write(&devices_path, devices.to_string()).unwrap();
    assert_eq!(
        fixture.ok(&["acquire", "sim", "--workspace", "worker"]),
        lease
    );
}

#[test]
fn simulator_policy_capacity_reclamation_and_external_devices() {
    let fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    let workspace = fixture.add("worker");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[simulators]\npreferred = [\"tablet\"]\n",
    )
    .unwrap();
    let tablet = fixture.ok(&["acquire", "sim", "--workspace", "worker"]);
    assert_eq!(tablet["device"], "type.Tablet");
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "worker",
    ]);
    let phone = fixture.ok(&[
        "acquire",
        "sim",
        "--workspace",
        "worker",
        "--profile",
        "phone",
    ]);
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
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "worker",
    ]);
    let unavailable = fixture.run(&[
        "acquire",
        "sim",
        "--workspace",
        "worker",
        "--device",
        "Phone",
        "--runtime",
        "Missing",
    ]);
    assert!(!unavailable.status.success());
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
            fixture
                .run(&["acquire", "sim", "--workspace", "worker"])
                .status
                .code(),
            Some(2),
            "external device in state {native} must occupy capacity"
        );
    }
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    assert!(!events.contains("external"));
    fixture.ok(&["rm", "--wait", "worker", "--yes"]);
}

#[test]
fn simulator_any_policy_pool_limit_and_interrupted_creation_cleanup() {
    let config = SIM_CONFIG.replace("max_devices = 2", "max_devices = 1\nallow_any = true");
    let fixture = Fixture::with_tools(Some(&config), true);
    fixture.add("worker");
    let phone = fixture.ok(&["acquire", "sim", "--workspace", "worker"]);
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "worker",
    ]);
    assert!(
        !fixture
            .run(&[
                "acquire",
                "sim",
                "--workspace",
                "worker",
                "--device",
                "Watch",
                "--runtime",
                "iOS Test",
            ])
            .status
            .success()
    );
    let watch = fixture.ok(&[
        "acquire",
        "sim",
        "--workspace",
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
        fixture.ok(&["leases", "sim", "--all"])["simulators"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "worker",
    ]);
    fs::write(fixture.root.path().join("sim-lost-create-response"), "1").unwrap();
    assert!(
        !fixture
            .run(&["acquire", "sim", "--workspace", "worker"])
            .status
            .success()
    );
    let record = fixture.ok(&["leases", "sim", "--workspace", "worker"]);
    assert_eq!(record["simulators"][0]["state"], "failed");
    assert!(record["simulators"][0]["udid"].is_null());
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "worker",
    ]);
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("sim-devices.json")).unwrap(),
        "[]"
    );
}

#[test]
fn simulators_allocate_concurrently_and_idle_expiry_keeps_active_leases() {
    let config = SIM_CONFIG
        .replace("max_booted = 1", "max_booted = 2")
        .replace("idle_seconds = 120", "idle_seconds = 0");
    let fixture = Fixture::with_tools(Some(&config), true);
    fixture.add("first");
    fixture.add("second");
    let first = fixture
        .command()
        .args(["--json", "acquire", "sim", "--workspace", "first"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let second = fixture
        .command()
        .args(["--json", "acquire", "sim", "--workspace", "second"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    assert!(first.status.success() && second.status.success());
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_ne!(first["udid"], second["udid"]);
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "first",
    ]);
    let sims = wait_for("idle simulator deletion", || {
        let sims = fixture.ok(&["leases", "sim", "--all"]);
        (sims["simulators"].as_array().unwrap().len() == 1).then_some(sims)
    });
    assert_eq!(sims["simulators"][0]["udid"], second["udid"]);
    fixture.ok(&["rm", "--wait", "first"]);
    fixture.ok(&["rm", "--wait", "second"]);
}

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
fn clean_simulator_requires_reason_minimizes_erasure_and_keeps_audit_after_removal() {
    let config = SIM_CONFIG.replace("max_booted = 1", "max_booted = 2");
    let mut fixture = Fixture::with_tools(Some(&config), true);
    fixture.add("first");
    fixture.add("second");
    fixture.add("third");
    let first = fixture.ok(&["acquire", "sim", "--workspace", "first"]);
    let second = fixture.ok(&["acquire", "sim", "--workspace", "second"]);
    set_sim_apps(&fixture, &first["udid"], 5);
    set_sim_apps(&fixture, &second["udid"], 1);
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "first",
    ]);
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "second",
    ]);
    assert!(
        !fixture
            .run(&["acquire", "sim", "--workspace", "third", "--clean"])
            .status
            .success()
    );
    let clean = fixture.run(&[
        "run",
        "--workspace",
        "third",
        "--",
        support::shoal_binary(),
        "--json",
        "acquire",
        "sim",
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
    let history = fixture.ok(&["history", "sim", "--all"]);
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
                "acquire",
                "sim",
                "--workspace",
                "third",
                "--clean",
                "--reason",
                "Accidental duplicate",
            ])
            .status
            .success()
    );
    let history = fixture.ok(&["history", "sim", "--all"]);
    assert_eq!(history[0]["status"], "failed");
    assert!(history[0]["action"].is_null());
    let page = fixture.ok(&[
        "history",
        "sim",
        "--all",
        "--before",
        &history[0]["id"].to_string(),
        "--limit",
        "1",
    ]);
    assert_eq!(page[0]["status"], "acquired");
    let visible = fixture.run(&[
        "run",
        "--workspace",
        "first",
        "--",
        support::shoal_binary(),
        "--json",
        "history",
        "sim",
        "--all",
    ]);
    assert_eq!(
        serde_json::from_slice::<Value>(&visible.stdout).unwrap(),
        serde_json::json!([])
    );
    fixture.ok(&["rm", "--wait", "third"]);
    assert_eq!(fixture.ok(&["history", "sim", "--all"]), history);
    fixture.daemon.kill();
    fixture.restart();
    assert_eq!(fixture.ok(&["history", "sim", "--all"]), history);
    let devices: Value = serde_json::from_str(
        &fs::read_to_string(fixture.root.path().join("sim-devices.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(devices[0]["user_apps"], 5);
}

#[test]
fn clean_simulator_prefers_fresh_capacity_and_logs_invalid_reasons() {
    let fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    fixture.add("first");
    fixture.add("second");
    let first = fixture.ok(&["acquire", "sim", "--workspace", "first"]);
    set_sim_apps(&fixture, &first["udid"], 3);
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "first",
    ]);
    assert!(
        !fixture
            .run(&[
                "acquire",
                "sim",
                "--workspace",
                "second",
                "--clean",
                "--reason",
                "   "
            ])
            .status
            .success()
    );
    let second = fixture.ok(&[
        "acquire",
        "sim",
        "--workspace",
        "second",
        "--clean",
        "--reason",
        "Verify clean system settings",
    ]);
    assert_ne!(first["udid"], second["udid"]);
    let history = fixture.ok(&["history", "sim", "--all"]);
    assert_eq!(history[0]["action"], "create");
    assert_eq!(history[1]["status"], "failed");
    assert!(
        !fs::read_to_string(fixture.root.path().join("sim-events"))
            .unwrap()
            .contains("erase")
    );
}

#[test]
fn clean_simulator_wait_retries_share_one_audit_entry() {
    let config = SIM_CONFIG.replace("max_devices = 2", "max_devices = 1");
    let fixture = Fixture::with_tools(Some(&config), true);
    fixture.add("first");
    fixture.add("second");
    fixture.ok(&["acquire", "sim", "--workspace", "first"]);
    let waiting = fixture
        .command()
        .args([
            "--json",
            "acquire",
            "sim",
            "--workspace",
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
        let entries = fixture.ok(&["history", "sim", "--all"]);
        entries.as_array().unwrap().len() == 1 && entries[0]["status"] == "busy"
    });
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "first",
    ]);
    let output = waiting.wait_with_output().unwrap();
    assert!(output.status.success());
    let entries = fixture.ok(&["history", "sim", "--all"]);
    assert_eq!(entries.as_array().unwrap().len(), 1);
    assert!(entries[0]["attempts"].as_u64().unwrap() >= 2);
    assert_eq!(entries[0]["status"], "acquired");
    assert_eq!(entries[0]["erase_completed"], true);
}

#[test]
fn clean_simulator_can_replace_an_empty_incompatible_device_to_preserve_apps() {
    let config = SIM_CONFIG.replace("max_booted = 1", "max_booted = 2");
    let fixture = Fixture::with_tools(Some(&config), true);
    fixture.add("phone");
    fixture.add("tablet");
    fixture.add("requester");
    let phone = fixture.ok(&["acquire", "sim", "--workspace", "phone"]);
    let tablet = fixture.ok(&[
        "acquire",
        "sim",
        "--workspace",
        "tablet",
        "--profile",
        "tablet",
    ]);
    set_sim_apps(&fixture, &phone["udid"], 5);
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "phone",
    ]);
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "tablet",
    ]);
    let clean = fixture.ok(&[
        "acquire",
        "sim",
        "--workspace",
        "requester",
        "--clean",
        "--reason",
        "Check default OS settings",
    ]);
    assert_ne!(clean["udid"], phone["udid"]);
    assert_ne!(clean["udid"], tablet["udid"]);
    let history = fixture.ok(&["history", "sim", "--all"]);
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
fn clean_simulator_daemon_requires_reason_and_records_erase_failures() {
    let config = SIM_CONFIG.replace("max_devices = 2", "max_devices = 1");
    let fixture = Fixture::with_tools(Some(&config), true);
    let workspace = fixture.add("worker");
    let response = fixture.request_with_id(
        2,
        serde_json::json!({"sim_acquire":{"workspace":workspace["id"],"request":{"request_id":uuid::Uuid::new_v4().to_string(),"clean":true,"name":"default","profile":null,"device":null,"runtime":null,"reason":null}}}),
    );
    assert_eq!(response["type"], "error");
    assert_eq!(
        fixture.ok(&["history", "sim", "--all"])[0]["status"],
        "failed"
    );
    assert!(!fixture.root.path().join("sim-devices.json").exists());
    fixture.ok(&["acquire", "sim", "--workspace", "worker"]);
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "worker",
    ]);
    fs::write(fixture.root.path().join("sim-fail"), "erase").unwrap();
    assert!(
        !fixture
            .run(&[
                "acquire",
                "sim",
                "--workspace",
                "worker",
                "--clean",
                "--reason",
                "Reset permission state",
            ])
            .status
            .success()
    );
    let entry = &fixture.ok(&["history", "sim", "--all"])[0];
    assert_eq!(entry["status"], "failed");
    assert_eq!(entry["action"], "erase");
    assert_eq!(entry["erase_completed"], false);
    assert!(entry["error"].as_str().unwrap().contains("injected"));
}

#[test]
fn a_failed_simulator_creation_retains_its_audit_and_ownership() {
    simulator_mutations_observe_persisted_audits_and_failure_retains_ownership("create");
}

#[test]
fn a_failed_simulator_shutdown_retains_its_audit_and_ownership() {
    simulator_mutations_observe_persisted_audits_and_failure_retains_ownership("shutdown");
}

#[test]
fn a_failed_simulator_erase_retains_its_audit_and_ownership() {
    simulator_mutations_observe_persisted_audits_and_failure_retains_ownership("erase");
}

#[test]
fn a_failed_simulator_boot_retains_its_audit_and_ownership() {
    simulator_mutations_observe_persisted_audits_and_failure_retains_ownership("bootstatus");
}

#[test]
fn a_failed_simulator_eviction_retains_its_audit_and_ownership() {
    simulator_mutations_observe_persisted_audits_and_failure_retains_ownership("delete");
}

/// Fail the simulator command `failure` during a clean acquisition and check
/// the audit and ownership the database held when the command ran.
fn simulator_mutations_observe_persisted_audits_and_failure_retains_ownership(failure: &str) {
    let config = SIM_CONFIG.replace("max_devices = 2", "max_devices = 1");
    let mut fixture = Fixture::with_tools(Some(&config), true);
    let workspace = fixture.add("worker");
    let previous = if failure == "create" {
        None
    } else {
        let sim = fixture.ok(&["acquire", "sim", "--workspace", "worker"]);
        fixture.ok(&[
            "release",
            "sim",
            "--name",
            "default",
            "--workspace",
            "worker",
        ]);
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
        "acquire",
        "sim",
        "--workspace",
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
    let events = fs::read_to_string(fixture.root.path().join("sim-persistence-events")).unwrap();
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
    let retained = fixture.ok(&["leases", "sim", "--all"])["simulators"][0].clone();
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
        fixture.ok(&["history", "sim", "--all"])[0]["status"],
        "failed"
    );
    fixture.restart();
    assert_eq!(
        fixture.ok(&["leases", "sim", "--all"])["simulators"][0],
        retained
    );
}

#[test]
fn local_repository_config_selects_simulator_preferences() {
    let fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    fixture.add("worker");
    let input = fixture.root.path().join("local.toml");
    fs::write(&input, "[simulators]\npreferred=['tablet']\n").unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &input);
    let sim = fixture.ok(&["acquire", "sim", "--workspace", "worker"]);
    assert_eq!(sim["device"], "type.Tablet");
}
