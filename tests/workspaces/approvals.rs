#[cfg(target_os = "macos")]
use super::simulators::SIM_CONFIG;
use crate::support::{Fixture, commit_resource_config};
use serde_json::Value;
use std::{fs, path::Path, process::Output};
#[cfg(target_os = "macos")]
use std::{
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

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

fn pending_access(output: Output) -> Value {
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
fn resource_approvals_require_unscoped_decisions_and_preserve_capacity() {
    let mut fixture = Fixture::with_config(Some("[resources.signing]\nrequires_approval=true\n"));
    fixture.add("agent");
    fixture.add("human");
    let args = ["resource", "acquire", "signing", "--reason", "sign build"];
    let pending = pending_access(scoped_command(&fixture, "agent", &args));
    let id = pending["id"].as_str().unwrap();
    assert_eq!(pending["workspace"], "agent");
    assert_eq!(
        fixture.ok(&["resource", "agent"])["pools"][0]["resources"][0]["requires_approval"],
        true
    );
    assert_eq!(
        pending_access(scoped_command(&fixture, "agent", &args))["id"],
        id
    );
    assert_eq!(fixture.ok(&["access"])[0]["id"], id);
    assert!(
        fixture.ok(&["resource", "agent"])["leases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !scoped_command(&fixture, "agent", &["access", "approve", id])
            .status
            .success()
    );
    assert!(
        !scoped_command(&fixture, "agent", &["access", "deny", id])
            .status
            .success()
    );
    assert!(
        !scoped_command(&fixture, "human", &["access", "list", "agent"])
            .status
            .success()
    );
    let other: Value =
        serde_json::from_slice(&scoped_command(&fixture, "human", &["access"]).stdout).unwrap();
    assert!(other.as_array().unwrap().is_empty());
    fixture.ok(&["resource", "acquire", "signing", "human"]);
    fixture.ok(&["access", "approve", id]);
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
    let next = pending_access(scoped_command(&fixture, "agent", &args));
    assert_ne!(next["id"], id);
    fixture.ok(&["access", "deny", next["id"].as_str().unwrap()]);
    let denied: Value =
        serde_json::from_slice(&scoped_command(&fixture, "agent", &args).stdout).unwrap();
    assert_eq!(denied["code"], "approval_denied");
    fixture.ok(&["resource", "release", "signing", "agent"]);
    assert!(fixture.ok(&["access"]).as_array().unwrap().is_empty());
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
    let pending = pending_access(scoped_command(&fixture, "agent", &args));
    fixture.ok(&["access", "approve", pending["id"].as_str().unwrap()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["resource", "release", "cache", "agent"]);
    fixture.restart();
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["resource", "release", "cache", "agent"]);
    let write = pending_access(scoped_command(
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
    assert!(fixture.ok(&["access"]).as_array().unwrap().is_empty());
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
        let pending = pending_access(scoped_command(&fixture, "agent", &args));
        assert!(
            fixture.ok(&["port", "agent"])["reserved"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let id = pending["id"].as_str().unwrap();
        fixture.ok(&["access", "approve", id]);
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
            assert_ne!(pending_access(next)["id"], id);
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
    let pending = pending_access(scoped_command(&fixture, "agent", &args));
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
        pending_access(scoped_command(&fixture, "agent", &explicit))["id"],
        pending["id"]
    );
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    assert!(
        events
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).unwrap()[0] == "list")
    );
    assert!(!fixture.root.path().join("sim-devices.json").exists());
    fixture.ok(&["access", "approve", pending["id"].as_str().unwrap()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["sim", "release", "default", "agent"]);
    fixture.restart();
    assert!(
        scoped_command(&fixture, "agent", &explicit)
            .status
            .success()
    );
    fixture.ok(&["sim", "release", "default", "agent"]);
    let clean = pending_access(scoped_command(
        &fixture,
        "agent",
        &["sim", "acquire", "--clean", "--reason", "isolate app state"],
    ));
    assert_ne!(clean["id"], pending["id"]);
    fixture.ok(&["access", "deny", clean["id"].as_str().unwrap()]);
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
            "--wait",
            "10",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let id = loop {
        let requests = fixture.ok(&["access"]);
        if let Some(request) = requests
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["status"] == "pending")
        {
            break request["id"].as_str().unwrap().to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "waiting acquisition did not request approval"
        );
        thread::sleep(Duration::from_millis(20));
    };
    fixture.ok(&["access", "approve", &id]);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let history = fixture.ok(&["sim", "history", "agent"]);
    assert_eq!(history[0]["status"], "acquired");
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
    let pending = pending_access(scoped_command(&fixture, "agent", &args));
    assert_eq!(pending["lifetime"], "lease");
    fixture.ok(&["access", "approve", pending["id"].as_str().unwrap()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["sim", "release", "default", "agent"]);
    assert_ne!(
        pending_access(scoped_command(&fixture, "agent", &args))["id"],
        pending["id"]
    );
    fixture.ok(&["sim", "release", "default", "agent"]);
}

#[test]
fn releasing_resource_names_clears_requests_from_previous_pool_scopes() {
    let config = "[resources.signing]\nrequires_approval=true\n";
    let mut fixture = Fixture::with_config(Some(config));
    let workspace = fixture.add("agent");
    let args = ["resource", "acquire", "signing", "--reason", "sign build"];
    let previous = pending_access(scoped_command(&fixture, "agent", &args));
    fs::write(fixture.root.path().join(".config/shoal/config.toml"), "").unwrap();
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join(".shoal.toml"),
        config,
    )
    .unwrap();
    fixture.restart();
    let current = pending_access(scoped_command(&fixture, "agent", &args));
    assert_ne!(previous["target"], current["target"]);
    fixture.ok(&["access", "approve", current["id"].as_str().unwrap()]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&["resource", "release", "signing", "agent"]);
    assert!(fixture.ok(&["access"]).as_array().unwrap().is_empty());
    assert_ne!(
        pending_access(scoped_command(&fixture, "agent", &args))["id"],
        current["id"]
    );
}

#[test]
fn tampered_resource_approvals_fail_instead_of_selecting_a_member() {
    let fixture = Fixture::with_config(Some("[resources.signing]\nrequires_approval=true\n"));
    fixture.add("agent");
    let args = ["resource", "acquire", "signing", "--reason", "sign build"];
    let pending = pending_access(scoped_command(&fixture, "agent", &args));
    fixture.ok(&["access", "approve", pending["id"].as_str().unwrap()]);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let mut record = pending.clone();
    record["status"] = "approved".into();
    record["specification"] = serde_json::json!({
        "preferred": null, "env": "PORT_WEB", "on_conflict": "suggest", "range": [3000, 3100]
    });
    let tamper = |record: &Value| {
        db.execute(
            "UPDATE access_requests SET record=?2 WHERE id=?1",
            rusqlite::params![pending["id"].as_str().unwrap(), record.to_string()],
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
