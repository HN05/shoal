mod support;

use serde_json::{Value, json};
use std::{process::Output, thread};

// Exercise the CLI protocol on every host without allocating real resources or simulators.
fn acquire(kind: &str, queue: bool, json: bool, replies: Vec<Value>) -> (Output, Vec<Value>) {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let listener = support::fake_daemon::bind_fake_daemon(root.path());
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for reply in replies {
            let stream =
                support::wait::wait_for("acquisition attempt", || match listener.accept() {
                    Ok((stream, _)) => Some(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
                    Err(error) => panic!("accept: {error}"),
                });
            let request = support::fake_daemon::answer(stream, |_| reply);
            requests.push(request["method"].clone());
        }
        requests
    });
    let mut command = support::cli(root.path());
    if json {
        command.arg("--json");
    }
    command.args([kind, "acquire"]);
    if kind == "resource" {
        command.args(["workers", "worker", "--resource", "builder"]);
    } else {
        command.args(["worker", "--clean"]);
    }
    command.args(["--reason", "test allocation"]);
    if queue {
        command.arg("--queue");
    }
    let output = command.output().unwrap();
    (output, server.join().unwrap())
}

fn approval(kind: &str, status: &str) -> Value {
    let mut reply = json!({
        "type": "approval_request", "data": {
            "id": 1, "workspace_id": "workspace-id", "workspace": "worker",
            "target": "simulator", "name": "default", "reason": "test allocation",
            "specification": {"clean": true, "profile": {"device": "Phone", "runtime": "iOS Test",
                "requires_approval": true, "approval_lifetime": "lease"}},
            "lifetime": "lease", "status": status, "created_at": 123,
            "decided_at": if status == "denied" { Some(456) } else { None }, "active": true
        }
    });
    if kind == "resource" {
        reply["data"]["target"] = "resource/global/workers".into();
        reply["data"]["specification"] = json!({
            "definition": {"capacity": 1, "reason": null, "resources": {"builder": {
                "capacity": 1, "reason": null, "kind": "semaphore", "requires_approval": true,
                "approval_lifetime": "lease"
            }}}, "member": "builder", "mode": "permit"
        });
    }
    reply
}

fn busy() -> Value {
    json!({"type": "busy", "data": {"message": "capacity full"}})
}

fn value(output: &Output, exit: i32) -> Value {
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn acquisition_responses_keep_exit_codes_json_details_and_human_approval_text() {
    for kind in ["resource", "sim"] {
        for status in ["pending", "denied"] {
            let reply = approval(kind, status);
            let (output, _) = acquire(kind, false, true, vec![reply.clone()]);
            assert_eq!(
                value(&output, 2),
                json!({
                    "acquired": false, "code": format!("approval_{status}"), "request": reply["data"]
                })
            );
            let (output, _) = acquire(kind, false, false, vec![reply]);
            assert_eq!(output.status.code(), Some(2), "{output:?}");
            let target = if kind == "resource" {
                "resource workers"
            } else {
                "simulator"
            };
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("1 [{status}] {target} (lease) in workspace worker: test allocation\n")
            );
        }
        let (output, _) = acquire(kind, false, true, vec![busy()]);
        let mut expected =
            json!({"acquired": false, "code": "simulator_busy", "message": "capacity full"});
        if kind == "resource" {
            expected["code"] = "resource_busy".into();
            expected["pool"] = "workers".into();
            expected["resource"] = "builder".into();
        }
        assert_eq!(value(&output, 2), expected);
    }
}

#[test]
fn polling_renders_the_final_denial() {
    for kind in ["resource", "sim"] {
        let (output, _) = acquire(
            kind,
            true,
            true,
            vec![approval(kind, "pending"), approval(kind, "denied")],
        );
        let output = value(&output, 2);
        assert_eq!(output["code"], "approval_denied");
        assert_eq!(output["request"], approval(kind, "denied")["data"]);
    }
}

#[test]
fn simulator_retries_reuse_the_original_clean_request_through_acquisition() {
    let kind = "sim";
    let sim = json!({
        "id": "sim-id", "udid": "device-id", "device": "Phone", "runtime": "iOS Test",
        "workspace_id": "workspace-id", "last_workspace_id": null, "lease_name": "default",
        "reason": "test allocation", "state": "leased", "last_used": 456,
        "error": null, "installed_apps": null
    });
    let (output, requests) = acquire(
        "sim",
        true,
        true,
        vec![
            busy(),
            approval(kind, "pending"),
            json!({"type": "simulator", "data": sim}),
        ],
    );
    assert_eq!(value(&output, 0), sim);
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request == &requests[0]));
    let request = &requests[0]["sim_acquire"]["request"];
    assert_eq!(request["clean"], true);
    assert_eq!(request["queue"], true);
    assert_eq!(request["reason"], "test allocation");
    uuid::Uuid::parse_str(request["request_id"].as_str().unwrap()).unwrap();
}
