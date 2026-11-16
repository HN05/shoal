//! Service containers: one Compose project per workspace on its reserved
//! ports, stopped on release and taken down with the workspace.
use super::*;
use serde_json::json;

const CONTAINERS: &str = "[containers]\ncompose = 'compose.yaml'\nports = ['db']\n";

/// A fixture whose repository runs `config`'s containers with the compose
/// fixture as Docker.
fn with_containers(config: &str) -> Fixture {
    let fixture = Fixture::new();
    install_test_script(
        fixture.root.path().join("bin/docker"),
        include_str!("../fixtures/compose.py"),
    );
    commit_resource_config(&fixture.repo, config);
    fixture
}

/// The `compose` calls the fixture runtime recorded, in order.
fn compose_events(fixture: &Fixture) -> Vec<Value> {
    let events = fs::read_to_string(fixture.root.path().join("compose-events"));
    (events.unwrap_or_default().lines())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The commands of the recorded calls on `project`, without the listings.
fn commands(fixture: &Fixture, project: &Value) -> Vec<String> {
    (compose_events(fixture).into_iter())
        .filter(|event| event["project"] == *project && event["command"] != "ps")
        .map(|event| event["command"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn each_workspace_runs_its_own_project_on_its_reserved_ports() {
    let fixture = with_containers(CONTAINERS);
    let workspace = fixture.add("first");
    fixture.add("second");
    let acquire = |name: &str| {
        let space = fixture.id(name);
        fixture.ok(&["acquire", "containers", "--space", &space, "--reason", "db"])
    };
    let first = acquire("first");
    let second = acquire("second");
    assert_ne!(first["project"], second["project"]);
    assert_eq!(first["state"], "running");
    assert_eq!(first["runtime"], "docker");
    assert_eq!(first["acquirer"]["kind"], "user");

    // Each project starts in its worktree from the file, with its port in
    // the port's variable, the port `acquire port` reserved.
    let port = fixture.ok(&["acquire", "port", "db", "--space", &fixture.id("first")]);
    let ups: Vec<Value> = (compose_events(&fixture).into_iter())
        .filter(|event| event["command"] == "up")
        .collect();
    assert_eq!(ups.len(), 2);
    assert_eq!(ups[0]["project"], first["project"]);
    assert_eq!(
        ups[0]["args"],
        json!(["-f", "compose.yaml", "up", "--detach", "--wait"])
    );
    let path = workspace["path"].clone();
    assert_eq!(
        fs::canonicalize(ups[0]["cwd"].as_str().unwrap()).unwrap(),
        fs::canonicalize(path.as_str().unwrap()).unwrap()
    );
    assert_eq!(
        ups[0]["ports"],
        json!({"SHOAL_PORT_DB": port["port"].to_string()})
    );
    assert_ne!(ups[1]["ports"], ups[0]["ports"]);
    // The services are the runtime's own listing.
    assert_eq!(first["services"][0]["service"], "db");
    assert_eq!(first["services"][0]["ports"][0]["published"], port["port"]);

    // Acquiring again returns the running project without starting it.
    assert_eq!(acquire("first")["project"], first["project"]);
    assert_eq!(commands(&fixture, &first["project"]), ["up"]);
}

#[test]
fn release_stops_the_project_and_keeps_its_ports() {
    let fixture = with_containers(CONTAINERS);
    fixture.add("worker");
    let space = fixture.id("worker");
    let started = fixture.ok(&["acquire", "containers", "--space", &space]);
    let port = started["services"][0]["ports"][0]["published"].clone();

    fixture.ok(&["release", "containers", "--space", &space]);
    let listed = fixture.ok(&["leases", "containers", "--space", &space]);
    assert_eq!(listed["projects"][0]["state"], "stopped");
    assert_eq!(commands(&fixture, &started["project"]), ["up", "stop"]);
    let ports = fixture.ok(&["leases", "port", "--space", &space]);
    assert_eq!(ports["reserved"][0]["port"], port);

    // Starting it again keeps the port.
    let restarted = fixture.ok(&["acquire", "containers", "--space", &space]);
    assert_eq!(restarted["state"], "running");
    assert_eq!(restarted["services"][0]["ports"][0]["published"], port);
}

#[test]
fn a_failed_start_is_recorded_and_retried_by_the_next_acquire() {
    let fixture = with_containers(CONTAINERS);
    fixture.add("worker");
    let space = fixture.id("worker");
    fs::write(fixture.root.path().join("compose-fail"), "up").unwrap();
    let failed = fixture.run(&["acquire", "containers", "--space", &space]);
    assert!(!failed.status.success(), "{}", printed(&failed));
    let listed = fixture.ok(&["leases", "containers", "--space", &space]);
    assert_eq!(listed["projects"][0]["state"], "failed");

    fs::remove_file(fixture.root.path().join("compose-fail")).unwrap();
    let started = fixture.ok(&["acquire", "containers", "--space", &space]);
    assert_eq!(started["state"], "running");
    assert_eq!(started["error"], Value::Null);
}

#[test]
fn the_catalog_names_the_configuration_and_the_runtime() {
    let fixture = with_containers(&format!("{CONTAINERS}start = 'setup'\n"));
    fixture.add("worker");
    let catalog = fixture.ok(&[
        "catalog",
        "containers",
        "--workspace",
        &fixture.id("worker"),
    ]);
    assert_eq!(catalog["compose"], "compose.yaml");
    assert_eq!(catalog["start"], "setup");
    assert_eq!(catalog["runtime"], "docker");
    assert_eq!(catalog.get("unavailable"), None);
}

#[test]
fn a_repository_without_a_compose_file_has_no_containers() {
    let fixture = with_containers("");
    fixture.add("worker");
    let refused = fixture.run(&["acquire", "containers", "--space", &fixture.id("worker")]);
    assert!(!refused.status.success());
    assert!(compose_events(&fixture).is_empty());
}
