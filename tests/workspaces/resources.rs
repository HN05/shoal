//! Ports, resource pools and read-write locks: leases, capacity, scopes and
//! repository resources.
use super::approvals::pending_approval;
use super::*;

#[test]
fn ports_are_named_idempotent_exported_and_released_with_the_workspace() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    fixture.add("second");
    let web = fixture.ok(&[
        "acquire",
        "port",
        "web",
        "--workspace",
        "first",
        "--reason",
        "Frontend dev server",
    ]);
    assert_eq!(web["env_var"], "SHOAL_PORT_WEB");
    assert_eq!(
        fixture.ok(&["acquire", "port", "web", "--workspace", "first"]),
        web
    );
    let port = web["port"].to_string();
    assert!(
        !fixture
            .run(&[
                "acquire",
                "port",
                "web",
                "--workspace",
                "second",
                "--port",
                &port
            ])
            .status
            .success()
    );
    let api = fixture.ok(&[
        "acquire",
        "port",
        "api",
        "--workspace",
        "first",
        "--env",
        "API_PORT",
    ]);
    let output = fixture.run(&[
        "run",
        "--workspace",
        "first",
        "--",
        "sh",
        "-c",
        "printf '%s:%s' \"$SHOAL_PORT_WEB\" \"$API_PORT\"",
    ]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}:{}", web["port"], api["port"])
    );
    assert_eq!(
        fixture.ok(&["status", "first"])["ports"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let nested = fixture.run(&[
        "run",
        "--workspace",
        "first",
        "--",
        support::shoal_binary(),
        "run",
        "--workspace",
        "second",
        "--",
        "sh",
        "-c",
        "test -z \"${SHOAL_PORT_WEB:-}\" && test -z \"${API_PORT:-}\"",
    ]);
    assert!(
        !nested.status.success(),
        "cross-workspace execution should be denied: {}",
        String::from_utf8_lossy(&nested.stderr)
    );
    let path = Path::new(first["path"].as_str().unwrap());
    fs::write(path.join("dirty"), "keep").unwrap();
    assert!(!fixture.run(&["rm", "--wait", "first"]).status.success());
    assert_eq!(
        fixture.ok(&["leases", "port", "--workspace", "first"])["reserved"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    fixture.ok(&["rm", "--wait", "first", "--yes"]);
    assert_eq!(
        fixture.ok(&["leases", "port", "--all"])[0]["reserved"],
        serde_json::json!([])
    );
    fixture.ok(&[
        "acquire",
        "port",
        "web",
        "--workspace",
        "second",
        "--port",
        &port,
    ]);
    fixture.ok(&["release", "port", "web", "--workspace", "second"]);
    assert_eq!(
        fixture.ok(&["leases", "port", "--workspace", "second"])["reserved"],
        serde_json::json!([])
    );
    fixture.ok(&["rm", "--wait", "second"]);
}

#[test]
fn ports_avoid_listeners_and_concurrent_allocations_are_unique_and_persistent() {
    let mut fixture = Fixture::new();
    fixture.add("ports");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let occupied = listener.local_addr().unwrap().port().to_string();
    assert!(
        !fixture
            .run(&[
                "acquire",
                "port",
                "occupied",
                "--workspace",
                "ports",
                "--port",
                &occupied
            ])
            .status
            .success()
    );
    let mut children = Vec::new();
    // Cross the small-list membership threshold while claims compete.
    for i in 0..40 {
        children.push(
            fixture
                .command()
                .args([
                    "--json",
                    "acquire",
                    "port",
                    &format!("server{i}"),
                    "--workspace",
                    "ports",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let mut numbers = std::collections::HashSet::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reservation: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(numbers.insert(reservation["port"].as_u64().unwrap()));
    }
    let before = fixture.ok(&["leases", "port", "--workspace", "ports"]);
    assert!(fixture.run(&["daemon", "stop"]).status.success());
    fixture.daemon.child.wait().unwrap();
    fixture.restart();
    assert_eq!(
        fixture.ok(&["leases", "port", "--workspace", "ports"]),
        before
    );
    fixture.ok(&["rm", "--wait", "ports"]);
}

#[test]
fn repository_config_sets_the_automatic_port_range() {
    let fixture = Fixture::new();
    let workspace = fixture.add("ranged");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let [first, second] = [(); 2].map(|()| ports::hold()).map(|held| held.port);
    fs::write(
        path.join(".shoal.toml"),
        format!("[ports]\nstart={first}\nend={first}\n"),
    )
    .unwrap();
    assert_eq!(
        fixture.ok(&["acquire", "port", "web", "--workspace", "ranged"])["port"],
        first
    );
    assert!(
        !fixture
            .run(&["acquire", "port", "api", "--workspace", "ranged"])
            .status
            .success()
    );
    // The saved config is the top layer, bound by bound.
    let saved = fixture.root.path().join("saved.toml");
    let save = |text: &str| {
        fs::write(&saved, text).unwrap();
        fixture.save_local_config(fixture.repo.to_str().unwrap(), &saved);
    };
    save(&format!("[ports]\nstart={second}\nend={second}\n"));
    assert_eq!(
        fixture.ok(&["acquire", "port", "api", "--workspace", "ranged"])["port"],
        second
    );
    // The layered range must stay nonempty: `end = 1` under the file's `start`.
    save("[ports]\nend=1\n");
    let output = fixture.run(&["acquire", "port", "db", "--workspace", "ranged"]);
    assert!(!output.status.success());
}

#[test]
fn configured_port_range_exhaustion_and_release() {
    // Another process on a shared runner can bind the port after the test
    // releases it, so a taken port starts the check over on a fresh one.
    for _ in 0..5 {
        if port_range_exhaustion_and_release() {
            return;
        }
    }
    panic!("other processes took every released test port");
}

/// False when another process bound the configured port once the test
/// stopped holding it.
fn port_range_exhaustion_and_release() -> bool {
    let held = ports::hold();
    let number = held.port;
    let fixture = Fixture::with_config(Some(&format!("[ports]\nstart={number}\nend={number}\n")));
    fixture.add("limited");
    let acquire =
        |name: &str| fixture.run(&["--json", "acquire", "port", name, "--workspace", "limited"]);
    assert!(!acquire("web").status.success());
    drop(held);
    let Some(lease) = port_lease(acquire("web")) else {
        return false;
    };
    assert_eq!(lease["port"], number);
    assert!(!acquire("api").status.success());
    fixture.ok(&["release", "port", "web", "--workspace", "limited"]);
    let Some(lease) = port_lease(acquire("api")) else {
        return false;
    };
    assert_eq!(lease["port"], number);
    fixture.ok(&["rm", "--wait", "limited"]);
    true
}

/// The granted lease, or `None` when no port in the range was free to bind.
fn port_lease(output: Output) -> Option<Value> {
    if output.status.success() {
        return Some(serde_json::from_slice(&output.stdout).unwrap());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no available TCP ports"), "{stderr}");
    None
}

#[test]
fn resource_overviews_preserve_results_after_workspace_errors() {
    let fixture = Fixture::new();
    let broken = fixture.add("broken");
    fixture.add("healthy");
    // A config file that cannot be read fails its workspace's reads.
    fs::create_dir(Path::new(broken["path"].as_str().unwrap()).join(".shoal.toml")).unwrap();
    // The daemon lists by name, so a failure must not short-circuit the query.
    let workspaces = fixture.ok(&["list"]);
    assert_eq!(workspaces[0]["name"], "broken");
    assert_eq!(workspaces[1]["name"], "healthy");
    // Port reservations read no configuration, so only resources can fail here.
    let noun = "resource";
    let mut expected = fixture.ok(&["leases", noun, "--workspace", "healthy"]);
    expected["workspace"] = workspaces[1].clone();
    let args = ["leases", noun, "--all"];
    let output = fixture.command().arg("--json").args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let overviews: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(overviews.as_array().unwrap().len(), 2);
    assert_eq!(overviews[0]["workspace"], workspaces[0]);
    let error = overviews[0]["error"].as_str().unwrap();
    assert!(!error.is_empty());
    assert_eq!(overviews[0].as_object().unwrap().len(), 2);
    assert_eq!(overviews[1], expected);

    let human = fixture.run(&args);
    assert_eq!(human.status.code(), Some(1));
    assert!(human.stderr.is_empty());
    assert!(
        String::from_utf8(human.stdout)
            .unwrap()
            .starts_with(&format!("broken: {error}\nhealthy\n"))
    );
    // Once every workspace is healthy, the same overview succeeds.
    fs::remove_dir(Path::new(broken["path"].as_str().unwrap()).join(".shoal.toml")).unwrap();
    assert_eq!(
        fixture
            .ok(&["leases", "resource", "--all"])
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn catalog_lists_configuration_where_simulators_are_unavailable() {
    let fixture = Fixture::with_config(Some(
        "[machine.simulators.profiles.phone]\ndevice = 'Phone'\n[resources.signing]\ncapacity = 1\n",
    ));
    // A host without Xcode: Linux refuses before running it, macOS runs this.
    install_test_script(
        fixture.root.path().join("bin").join("xcrun"),
        "#!/bin/sh\necho 'xcrun: unable to find simctl' >&2\nexit 1\n",
    );
    fixture.add("worker");
    let catalog = fixture.ok(&["catalog", "--workspace", "worker"]);
    assert!(catalog["simulators"]["policy"]["profiles"]["phone"].is_object());
    assert!(catalog["simulators"]["inventory_error"].is_string());
    assert_eq!(catalog["simulators"]["runtimes"], serde_json::json!([]));
    assert_eq!(catalog["resources"]["pools"][0]["name"], "signing");
    assert!(
        fixture
            .run(&["catalog", "sim", "--workspace", "worker"])
            .status
            .success()
    );
}

#[test]
fn configured_ports_are_lazy_and_conflicts_require_acceptance() {
    let fixture = Fixture::new();
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let preferred = occupied.local_addr().unwrap().port();
    fs::write(
        fixture.repo.join(".shoal.toml"),
        format!("[ports.web]\nport = {preferred}\nenv = \"PORT\"\nreason = \"Web server\"\n")
            .replace("\\\"", "\""),
    )
    .unwrap();
    git(&fixture.repo, &["add", ".shoal.toml"]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "config",
        ],
    );
    let workspace = fixture.add("configured");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let overview = fixture
        .command()
        .current_dir(path)
        .args(["--json", "leases", "port"])
        .output()
        .unwrap();
    assert!(overview.status.success());
    let overview: Value = serde_json::from_slice(&overview.stdout).unwrap();
    assert_eq!(overview["reserved"], serde_json::json!([]));
    assert_eq!(
        fixture.ok(&["leases", "port", "--workspace", "configured"]),
        overview
    );
    let catalog = fixture
        .command()
        .current_dir(path)
        .args(["--json", "catalog", "port"])
        .output()
        .unwrap();
    assert!(catalog.status.success());
    let catalog: Value = serde_json::from_slice(&catalog.stdout).unwrap();
    assert_eq!(catalog["configured"]["web"]["port"], preferred);
    let proposal = fixture.run(&[
        "--json",
        "acquire",
        "port",
        "web",
        "--workspace",
        "configured",
    ]);
    assert_eq!(proposal.status.code(), Some(2));
    let proposal: Value = serde_json::from_slice(&proposal.stdout).unwrap();
    assert_eq!(proposal["reserved"], false);
    assert_eq!(
        fixture.ok(&["leases", "port", "--workspace", "configured"])["reserved"],
        serde_json::json!([])
    );
    let accepted = fixture.ok(&[
        "acquire",
        "port",
        "web",
        "--workspace",
        "configured",
        "--port",
        &proposal["suggested_port"].to_string(),
    ]);
    assert_eq!(accepted["env_var"], "PORT");
    assert_eq!(
        fixture.ok(&["acquire", "port", "web", "--workspace", "configured"]),
        accepted
    );
    fixture.ok(&["release", "port", "web", "--workspace", "configured"]);
    let automatic = fixture.ok(&[
        "acquire",
        "port",
        "web",
        "--workspace",
        "configured",
        "--on-conflict",
        "auto",
    ]);
    assert_ne!(automatic["port"], preferred);
    assert_eq!(
        fixture.ok(&["acquire", "port", "web", "--workspace", "configured"]),
        automatic
    );
    fs::create_dir(path.join(".shoal")).unwrap();
    fs::rename(path.join(".shoal.toml"), path.join(".shoal/config.toml")).unwrap();
    fixture.ok(&["catalog", "port", "--workspace", "configured"]);
    // Both together are rejected, so neither names the port, and the
    // workspace's config says why.
    fs::write(path.join(".shoal.toml"), "").unwrap();
    let catalog = fixture.ok(&["catalog", "port", "--workspace", "configured"]);
    assert!(catalog["configured"].get("web").is_none(), "{catalog}");
    let show = fixture.run(&["config", "show", "configured"]);
    assert!(show.status.success());
    let warning = String::from_utf8(show.stderr).unwrap();
    assert!(
        warning.contains("keep only one repository config"),
        "{warning}"
    );
}

pub(crate) const RESOURCE_CONFIG: &str = r#"
[resources.signing]
capacity = 1
reason = "Signing service"
[resource_pools.devices]
capacity = 2
[resource_pools.devices.resources.alpha]
capacity = 1
reason = "Alpha test device"
[resource_pools.devices.resources.beta]
capacity = 2
"#;

#[test]
fn acquire_selects_the_lease_kind() {
    let fixture = Fixture::with_config(Some(RESOURCE_CONFIG));
    fixture.add("first");
    fixture.ok(&["acquire", "port", "web", "--workspace", "first"]);
    let lease = fixture.ok(&[
        "acquire",
        "resource",
        "devices",
        "--member",
        "beta",
        "--name",
        "tests",
        "--workspace",
        "first",
    ]);
    assert_eq!(lease["resource"], "beta");
    // A repo acquisition never selects another kind of member.
    let repo = fixture.run(&["acquire", "repo", "signing", "--workspace", "first"]);
    assert!(!repo.status.success());
    let leases = fixture.ok(&["leases", "--workspace", "first"]);
    assert_eq!(leases["ports"]["reserved"][0]["name"], "web");
    assert_eq!(leases["resources"]["leases"].as_array().unwrap().len(), 1);
    assert_eq!(leases["resources"]["leases"][0]["name"], "tests");
    let count = |args: &[&str], key: &str| {
        fixture.ok(&[&["leases"], args, &["--workspace", "first"]].concat())[key]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(count(&["resource"], "pools"), 2);
    assert_eq!(count(&["repo"], "pools"), 0);
    assert_eq!(count(&["repo"], "leases"), 0);
    assert_eq!(
        fixture.ok(&["leases", "--all"]).as_array().unwrap().len(),
        1
    );
    assert_eq!(
        fixture.ok(&["leases", "--all"])[0]["workspace"]["name"],
        "first"
    );
}

#[test]
fn release_narrows_from_every_lease_to_one_kind_or_item() {
    let fixture = Fixture::with_config(Some(RESOURCE_CONFIG));
    fixture.add("first");
    for args in [
        vec!["acquire", "port", "web"],
        vec!["acquire", "resource", "devices", "--name", "a"],
        vec!["acquire", "resource", "devices", "--name", "b"],
        vec!["acquire", "resource", "signing"],
    ] {
        fixture.ok(&[args, vec!["--workspace", "first"]].concat());
    }
    let released = |args: &[&str]| {
        fixture.ok(&[&["release", "--workspace", "first"], args].concat())["released"].clone()
    };
    // A named lease must match the requested kind.
    let mismatch = fixture.run(&[
        "release",
        "repo",
        "devices",
        "--name",
        "a",
        "--workspace",
        "first",
    ]);
    assert!(!mismatch.status.success());
    assert_eq!(
        released(&["resource", "devices", "--name", "a"]),
        serde_json::json!([{"kind": "resource", "pool": "devices", "name": "a"}])
    );
    assert_eq!(
        released(&["resource", "devices"]),
        serde_json::json!([{"kind": "resource", "pool": "devices", "name": "b"}])
    );
    assert_eq!(
        released(&["port"]),
        serde_json::json!([{"kind": "port", "name": "web"}])
    );
    assert_eq!(
        released(&[]),
        serde_json::json!([{"kind": "resource", "pool": "signing", "name": "default"}])
    );
    assert!(
        !fixture
            .run(&["release", "--workspace", "first"])
            .status
            .success()
    );
    let inspection = fixture.ok(&["status", "first"]);
    assert!(inspection["ports"].as_array().unwrap().is_empty());
    assert!(inspection["resources"].as_array().unwrap().is_empty());
}

#[test]
fn resources_enforce_pool_and_member_capacity_and_named_permits() {
    let fixture = Fixture::with_config(Some(RESOURCE_CONFIG));
    fixture.add("first");
    fixture.add("second");
    fixture.add("third");
    let first = fixture.ok(&["acquire", "resource", "devices", "--workspace", "first"]);
    assert_eq!(first["resource"], "alpha");
    assert_eq!(
        fixture.ok(&["acquire", "resource", "devices", "--workspace", "first"]),
        first
    );
    assert_eq!(
        fixture
            .run(&[
                "acquire",
                "resource",
                "devices",
                "--workspace",
                "second",
                "--member",
                "alpha",
            ])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&[
        "acquire",
        "resource",
        "devices",
        "--workspace",
        "second",
        "--member",
        "beta",
    ]);
    let busy = fixture.run(&[
        "--json",
        "acquire",
        "resource",
        "devices",
        "--workspace",
        "third",
        "--member",
        "beta",
    ]);
    assert_eq!(busy.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&busy.stdout).unwrap()["acquired"],
        false
    );
    fixture.ok(&[
        "release",
        "resource",
        "devices",
        "--workspace",
        "first",
        "--name",
        "default",
    ]);
    fixture.ok(&[
        "acquire",
        "resource",
        "devices",
        "--workspace",
        "third",
        "--member",
        "beta",
    ]);
    let overview = fixture.ok(&["leases", "resource", "--workspace", "third"]);
    let pool = overview["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "devices")
        .unwrap();
    assert_eq!(pool["used"], 2);
    assert_eq!(pool["available"], 0);
    let beta = pool["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "beta")
        .unwrap();
    assert_eq!(beta["used"], 2);
    fixture.ok(&[
        "release",
        "resource",
        "devices",
        "--workspace",
        "second",
        "--name",
        "default",
    ]);
    fixture.ok(&[
        "acquire",
        "resource",
        "devices",
        "--workspace",
        "third",
        "--member",
        "beta",
        "--name",
        "parallel",
        "--reason",
        "Parallel job",
    ]);
    assert_eq!(
        fixture.ok(&["status", "third"])["resources"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let lock = fixture.ok(&["acquire", "resource", "signing", "--workspace", "first"]);
    assert_eq!(lock["resource"], "signing");
    assert_eq!(lock["scope"], "global");
    assert_eq!(
        fixture
            .run(&["acquire", "resource", "signing", "--workspace", "second"])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&["rm", "--wait", "first"]);
    fixture.ok(&["acquire", "resource", "signing", "--workspace", "second"]);
    fixture.ok(&["rm", "--wait", "third"]);
    let overviews = fixture.ok(&["leases", "resource", "--all"]);
    let mut device_leases = overviews
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|overview| overview["leases"].as_array().unwrap());
    assert!(device_leases.all(|l| l["pool"] != "devices"));
}

#[test]
fn resource_claims_are_atomic_persistent_and_wait_for_release() {
    let mut fixture = Fixture::with_config(Some("[resources.workers]\ncapacity=3\n"));
    let workspace = fixture.add("worker");
    let children: Vec<_> = (0..10)
        .map(|i| {
            fixture
                .command()
                .args([
                    "--json",
                    "acquire",
                    "resource",
                    "workers",
                    "--workspace",
                    "worker",
                    "--name",
                    &format!("job-{i}"),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut successes = vec![];
    for child in children {
        let output = child.wait_with_output().unwrap();
        if output.status.success() {
            successes.push(serde_json::from_slice::<Value>(&output.stdout).unwrap());
        } else {
            assert_eq!(
                output.status.code(),
                Some(2),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    assert_eq!(successes.len(), 3);
    let leases = fixture.ok(&["leases", "resource", "--all"]);
    assert_eq!(leases[0]["leases"].as_array().unwrap().len(), 3);
    fixture.daemon.kill();
    fixture.restart();
    assert_eq!(fixture.ok(&["leases", "resource", "--all"]), leases);
    fixture.ok(&["alerts"]);
    let mut waiter = fixture
        .command()
        .args([
            "--json",
            "acquire",
            "resource",
            "workers",
            "--workspace",
            "worker",
            "--name",
            "waiter",
            "--queue",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_busy(&fixture, "worker");
    assert!(waiter.try_wait().unwrap().is_none());
    fixture.ok(&[
        "release",
        "resource",
        "workers",
        "--workspace",
        "worker",
        "--name",
        successes[0]["name"].as_str().unwrap(),
    ]);
    assert!(waiter.wait_with_output().unwrap().status.success());
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join("dirty"),
        "retain",
    )
    .unwrap();
    assert!(!fixture.run(&["rm", "--wait", "worker"]).status.success());
    assert_eq!(
        fixture.ok(&["leases", "resource", "--all"])[0]["leases"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    fixture.ok(&["rm", "--wait", "worker", "--yes"]);
    assert!(
        fixture
            .ok(&["leases", "resource", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .all(|overview| overview["leases"] == serde_json::json!([]))
    );
}

#[test]
fn repo_resources_share_across_branches_and_refuse_conflicting_definitions() {
    let fixture = Fixture::new();
    commit_resource_config(&fixture.repo, RESOURCE_CONFIG);
    let first = fixture.add("first");
    let second = fixture.add("second");
    let first_path = Path::new(first["path"].as_str().unwrap());
    let second_path = Path::new(second["path"].as_str().unwrap());
    let lease = fixture.ok(&["acquire", "resource", "signing", "--workspace", "first"]);
    assert!(lease["scope"].as_str().unwrap().starts_with("repo/"));
    assert_eq!(
        fixture
            .run(&["acquire", "resource", "signing", "--workspace", "second"])
            .status
            .code(),
        Some(2)
    );
    fs::write(
        second_path.join(".shoal.toml"),
        RESOURCE_CONFIG.replace(
            "capacity = 1\nreason = \"Signing service\"",
            "capacity = 2\nreason = \"Signing service\"",
        ),
    )
    .unwrap();
    let conflict = fixture.run(&["acquire", "resource", "signing", "--workspace", "second"]);
    assert!(!conflict.status.success());
    let overview = fixture.ok(&["leases", "resource", "--workspace", "second"]);
    let signing = overview["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "signing")
        .unwrap();
    assert_eq!(signing["configuration_matches"], false);
    fs::copy(
        second_path.join(".shoal.toml"),
        first_path.join(".shoal.toml"),
    )
    .unwrap();
    let renewed = fixture.ok(&[
        "acquire",
        "resource",
        "signing",
        "--workspace",
        "first",
        "--reason",
        "still signing",
    ]);
    assert_eq!(renewed["id"], lease["id"]);
    assert_eq!(renewed["created_at"], lease["created_at"]);
    assert_eq!(renewed["reason"], "still signing");
    assert_eq!(fixture.ok(&["status", "first"])["resources"][0], renewed);
    fs::remove_file(first_path.join(".shoal.toml")).unwrap();
    fixture.ok(&[
        "release",
        "resource",
        "signing",
        "--workspace",
        "first",
        "--name",
        "default",
    ]);
    fixture.ok(&["acquire", "resource", "signing", "--workspace", "second"]);
    fixture.ok(&[
        "acquire",
        "resource",
        "signing",
        "--workspace",
        "second",
        "--name",
        "another",
    ]);
    let output = fixture
        .command()
        .current_dir(second_path)
        .args(["--json", "leases", "resource"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["leases"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn resource_scopes_separate_repos_share_global_capacity_and_limit_agents() {
    let fixture = Fixture::with_config(Some("[resources.machine]\ncapacity=1\n"));
    commit_resource_config(&fixture.repo, "[resources.local]\ncapacity=1\n");
    fixture.add("first");
    let other = fixture.root.path().join("other");
    fs::create_dir(&other).unwrap();
    git(&other, &["init", "-b", "main"]);
    commit_resource_config(&other, "[resources.local]\ncapacity=1\n");
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let second = fixture.ok(&["start", other.to_str().unwrap(), "second"]);
    let local_first = fixture.ok(&["acquire", "resource", "local", "--workspace", "first"]);
    let local_second = fixture.ok(&["acquire", "resource", "local", "--workspace", "second"]);
    assert_ne!(local_first["scope"], local_second["scope"]);
    fixture.ok(&["acquire", "resource", "machine", "--workspace", "first"]);
    assert_eq!(
        fixture
            .run(&["acquire", "resource", "machine", "--workspace", "second"])
            .status
            .code(),
        Some(2)
    );
    let scoped = |args: &[&str]| {
        fixture
            .command()
            .args(["run", "--workspace", "first", "--", support::shoal_binary()])
            .args(args)
            .output()
            .unwrap()
    };
    assert!(scoped(&["acquire", "resource", "local"]).status.success());
    assert!(
        !scoped(&[
            "release",
            "resource",
            "local",
            "--workspace",
            "second",
            "--name",
            "default"
        ])
        .status
        .success()
    );
    assert!(
        !scoped(&["leases", "resource", "--workspace", "second"])
            .status
            .success()
    );
    let listed: Value =
        serde_json::from_slice(&scoped(&["--json", "leases", "resource", "--all"]).stdout).unwrap();
    let leases = listed[0]["leases"].as_array().unwrap();
    assert_eq!(leases.len(), 2);
    assert!(
        leases
            .iter()
            .all(|l| l["workspace_id"] == local_first["workspace_id"])
    );
    fs::write(
        Path::new(second["path"].as_str().unwrap()).join(".shoal.toml"),
        "[resources.machine]\ncapacity=10\n",
    )
    .unwrap();
    let conflict = fixture.run(&["acquire", "resource", "machine", "--workspace", "second"]);
    assert!(!conflict.status.success());
    fixture.ok(&[
        "release",
        "resource",
        "local",
        "--workspace",
        "second",
        "--name",
        "default",
    ]);
}

#[test]
fn rwlock_readers_share_one_slot_and_writers_exclude_everyone() {
    let fixture = Fixture::with_config(Some("[resources.cache]\nkind='rwlock'\n"));
    fixture.add("first");
    fixture.add("second");
    let first = fixture.ok(&[
        "acquire",
        "resource",
        "cache",
        "--workspace",
        "first",
        "--mode",
        "read",
    ]);
    assert_eq!(first["mode"], "read");
    assert_eq!(
        fixture.ok(&["acquire", "resource", "cache", "--workspace", "first"]),
        first
    );
    assert!(
        !fixture
            .run(&[
                "acquire",
                "resource",
                "cache",
                "--workspace",
                "first",
                "--mode",
                "write"
            ])
            .status
            .success()
    );
    let children: Vec<_> = (0..16)
        .map(|i| {
            fixture
                .command()
                .args([
                    "--json",
                    "acquire",
                    "resource",
                    "cache",
                    "--workspace",
                    "second",
                    "--mode",
                    "read",
                    "--name",
                    &format!("reader-{i}"),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let overview = fixture.ok(&["leases", "resource", "--workspace", "first"]);
    let pool = &overview["pools"][0];
    assert_eq!(pool["used"], 1);
    assert_eq!(pool["available"], 0);
    assert_eq!(pool["resources"][0]["readers"], 17);
    assert_eq!(pool["resources"][0]["read_available"], true);
    assert_eq!(pool["resources"][0]["write_available"], false);
    assert_eq!(
        fixture
            .run(&[
                "acquire",
                "resource",
                "cache",
                "--workspace",
                "second",
                "--mode",
                "write"
            ])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&[
        "release",
        "resource",
        "cache",
        "--workspace",
        "first",
        "--name",
        "default",
    ]);
    // The final reader, not the first release, makes a writer possible.
    for i in 0..15 {
        fixture.ok(&[
            "release",
            "resource",
            "cache",
            "--workspace",
            "second",
            "--name",
            &format!("reader-{i}"),
        ]);
    }
    assert_eq!(
        fixture
            .run(&["acquire", "resource", "cache", "--workspace", "first"])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&[
        "release",
        "resource",
        "cache",
        "--workspace",
        "second",
        "--name",
        "reader-15",
    ]);
    let writer = fixture.ok(&["acquire", "resource", "cache", "--workspace", "first"]);
    assert_eq!(writer["mode"], "write");
    for mode in ["read", "write"] {
        assert_eq!(
            fixture
                .run(&[
                    "acquire",
                    "resource",
                    "cache",
                    "--workspace",
                    "second",
                    "--mode",
                    mode
                ])
                .status
                .code(),
            Some(2)
        );
    }
    let status = fixture.ok(&["leases", "resource", "--workspace", "second"]);
    assert_eq!(status["pools"][0]["resources"][0]["writers"], 1);
    assert_eq!(status["pools"][0]["resources"][0]["read_available"], false);
    fixture.ok(&["rm", "--wait", "first"]);
    fixture.ok(&[
        "acquire",
        "resource",
        "cache",
        "--workspace",
        "second",
        "--mode",
        "read",
    ]);
}

#[test]
fn rwlock_mixed_pools_apply_capacity_to_occupied_members_and_filter_modes() {
    let fixture = Fixture::with_config(Some(
        "[resource_pools.mixed]\ncapacity=2\n[resource_pools.mixed.resources.a]\nkind='rwlock'\n[resource_pools.mixed.resources.b]\nkind='rwlock'\n[resource_pools.mixed.resources.worker]\ncapacity=2\n",
    ));
    fixture.add("owner");
    fixture.ok(&[
        "acquire",
        "resource",
        "mixed",
        "--workspace",
        "owner",
        "--member",
        "worker",
        "--name",
        "job",
    ]);
    let reader = fixture.ok(&[
        "acquire",
        "resource",
        "mixed",
        "--workspace",
        "owner",
        "--mode",
        "read",
        "--name",
        "read-one",
    ]);
    assert_eq!(reader["resource"], "a");
    let more = fixture.ok(&[
        "acquire",
        "resource",
        "mixed",
        "--workspace",
        "owner",
        "--mode",
        "read",
        "--name",
        "read-two",
    ]);
    assert_eq!(more["resource"], "a");
    for resource in ["b", "worker"] {
        assert_eq!(
            fixture
                .run(&[
                    "acquire",
                    "resource",
                    "mixed",
                    "--workspace",
                    "owner",
                    "--member",
                    resource,
                    "--name",
                    "extra",
                ])
                .status
                .code(),
            Some(2)
        );
    }
    for (resource, mode) in [("worker", "read"), ("worker", "write"), ("a", "permit")] {
        let output = fixture.run(&[
            "acquire",
            "resource",
            "mixed",
            "--workspace",
            "owner",
            "--member",
            resource,
            "--mode",
            mode,
            "--name",
            "invalid",
        ]);
        assert_eq!(output.status.code(), Some(1));
    }
    fixture.ok(&[
        "release",
        "resource",
        "mixed",
        "--workspace",
        "owner",
        "--name",
        "job",
    ]);
    let writer = fixture.ok(&[
        "acquire",
        "resource",
        "mixed",
        "--workspace",
        "owner",
        "--mode",
        "write",
        "--name",
        "writer",
    ]);
    assert_eq!(writer["resource"], "b");
    let overview = fixture.ok(&["leases", "resource", "--workspace", "owner"]);
    assert_eq!(overview["pools"][0]["used"], 2);
    assert_eq!(overview["pools"][0]["resources"][0]["read_available"], true);
    assert_eq!(
        overview["pools"][0]["resources"][1]["read_available"],
        false
    );
}

#[test]
fn rwlock_claims_are_atomic_across_readers_and_writers() {
    let fixture = Fixture::with_config(Some("[resources.cache]\nkind='rwlock'\n"));
    fixture.add("owner");
    let children: Vec<_> = (0..20)
        .map(|i| {
            fixture
                .command()
                .args([
                    "--json",
                    "acquire",
                    "resource",
                    "cache",
                    "--workspace",
                    "owner",
                    "--mode",
                    if i % 2 == 0 { "write" } else { "read" },
                    "--name",
                    &format!("lease-{i}"),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut reads = 0;
    let mut writes = 0;
    for child in children {
        let output = child.wait_with_output().unwrap();
        if output.status.success() {
            let lease: Value = serde_json::from_slice(&output.stdout).unwrap();
            if lease["mode"] == "read" {
                reads += 1;
            } else {
                writes += 1;
            }
        } else {
            assert_eq!(
                output.status.code(),
                Some(2),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    assert!((reads == 10 && writes == 0) || (writes == 1 && reads == 0));
}

#[test]
fn rwlock_modes_survive_restart_wait_and_failed_removal() {
    let mut fixture = Fixture::with_config(Some(
        "[resources.cache]\nkind='rwlock'\n[resources.index]\nkind='rwlock'\n",
    ));
    let workspace = fixture.add("reader");
    fixture.add("writer");
    let lease = fixture.ok(&[
        "acquire",
        "resource",
        "cache",
        "--workspace",
        "reader",
        "--mode",
        "read",
    ]);
    let writer = fixture.ok(&[
        "acquire",
        "resource",
        "index",
        "--workspace",
        "writer",
        "--mode",
        "write",
    ]);
    fixture.daemon.kill();
    fixture.restart();
    assert_eq!(
        fixture.ok(&[
            "acquire",
            "resource",
            "cache",
            "--workspace",
            "reader",
            "--mode",
            "read"
        ]),
        lease
    );
    assert_eq!(
        fixture.ok(&[
            "acquire",
            "resource",
            "index",
            "--workspace",
            "writer",
            "--mode",
            "write"
        ]),
        writer
    );
    assert_eq!(
        fixture
            .run(&[
                "acquire",
                "resource",
                "index",
                "--workspace",
                "reader",
                "--mode",
                "read"
            ])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&["alerts"]);
    let mut waiter = fixture
        .command()
        .args([
            "--json",
            "acquire",
            "resource",
            "cache",
            "--workspace",
            "writer",
            "--mode",
            "write",
            "--queue",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_busy(&fixture, "writer");
    assert!(waiter.try_wait().unwrap().is_none());
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("unfinished"), "retain").unwrap();
    assert!(!fixture.run(&["rm", "--wait", "reader"]).status.success());
    assert_eq!(
        fixture.ok(&["leases", "resource", "--workspace", "reader"])["leases"][0],
        lease
    );
    fixture.ok(&[
        "release",
        "resource",
        "cache",
        "--workspace",
        "reader",
        "--name",
        "default",
    ]);
    let output = waiter.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["mode"],
        "write"
    );
}

#[test]
fn resource_renewals_reject_scope_drift_before_allocating() {
    let config = "[resources.cache]\nkind='rwlock'\n";
    let mut fixture = Fixture::with_config(Some(config));
    let workspace = fixture.add("owner");
    let args = [
        "acquire",
        "resource",
        "cache",
        "--workspace",
        "owner",
        "--mode",
        "read",
    ];
    let original = fixture.ok(&args);
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join(".shoal.toml"),
        config,
    )
    .unwrap();
    fs::write(fixture.root.path().join(".config/shoal/config.toml"), "").unwrap();
    fixture.restart();
    let rejected = fixture.run(&args);
    assert!(!rejected.status.success());
    assert_eq!(
        fixture.ok(&["leases", "resource", "--workspace", "owner"])["leases"],
        serde_json::json!([original])
    );
    fixture.ok(&[
        "release",
        "resource",
        "cache",
        "--workspace",
        "owner",
        "--name",
        "default",
    ]);
    let local = fixture.ok(&args);
    assert_ne!(local["scope"], original["scope"]);
    assert_eq!(fixture.ok(&args), local);
    fs::write(
        fixture.root.path().join(".config/shoal/config.toml"),
        config,
    )
    .unwrap();
    fixture.restart();
    assert!(!fixture.run(&args).status.success());
    assert_eq!(
        fixture.ok(&["leases", "resource", "--workspace", "owner"])["leases"],
        serde_json::json!([local])
    );
}

#[test]
fn rwlock_scope_and_kind_drift_preserve_active_leases() {
    let fixture = Fixture::new();
    commit_resource_config(&fixture.repo, "[resources.cache]\nkind='rwlock'\n");
    let first = fixture.add("first");
    let second = fixture.add("second");
    let binary = support::shoal_binary();
    let output = fixture.run(&[
        "run",
        "--workspace",
        "first",
        "--",
        binary,
        "--json",
        "acquire",
        "resource",
        "cache",
        "--mode",
        "read",
    ]);
    assert!(output.status.success());
    let lease: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(lease["mode"], "read");
    assert!(
        !fixture
            .run(&[
                "run",
                "--workspace",
                "second",
                "--",
                binary,
                "release",
                "resource",
                "cache",
                "--workspace",
                "first",
                "--name",
                "default",
            ])
            .status
            .success()
    );
    let path = Path::new(second["path"].as_str().unwrap());
    fs::write(path.join(".shoal.toml"), "[resources.cache]\ncapacity=2\n").unwrap();
    assert!(
        !fixture
            .run(&["acquire", "resource", "cache", "--workspace", "second"])
            .status
            .success()
    );
    assert_eq!(
        fixture.ok(&["leases", "resource", "--workspace", "second"])["pools"][0]["configuration_matches"],
        false
    );
    // Release still works when its own definition is removed entirely.
    fs::remove_file(Path::new(first["path"].as_str().unwrap()).join(".shoal.toml")).unwrap();
    fixture.ok(&[
        "release",
        "resource",
        "cache",
        "--workspace",
        "first",
        "--name",
        "default",
    ]);
    assert_eq!(
        fixture.ok(&["acquire", "resource", "cache", "--workspace", "second"])["mode"],
        "permit"
    );
}

pub(crate) fn wait_for_busy(fixture: &Fixture, workspace: &str) {
    wait_until("blocked acquisition notification", || {
        fixture
            .ok(&["alerts"])
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "resource_busy" && event["workspace"] == workspace)
    });
}

#[test]
fn repository_resources_return_checkouts_and_preserve_them_on_cleanup() {
    let mut fixture = Fixture::new();
    let related = init_repo(
        fixture.root.path(),
        "server",
        &[("DESIGN.MD", "server design\n")],
    );
    let registration = fixture.ok(&["repo", "add", related.to_str().unwrap(), "--name", "server"]);
    set_repository_toml(&fixture, "[resources.server]\nkind='repo'\nrepo='server'\n");
    fixture.add("consumer");
    fixture.add("other-consumer");
    let catalog = fixture.ok(&["catalog", "repo", "--workspace", "consumer"]);
    assert_eq!(catalog["pools"][0]["resources"]["server"]["repo"], "server");
    assert_eq!(
        fixture.ok(&["catalog", "resource", "--workspace", "consumer"]),
        serde_json::json!({"pools": []})
    );
    assert!(
        fixture.ok(&["leases", "repo", "--workspace", "consumer"])["leases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let output = scoped_command(&fixture, "consumer", &["acquire", "repo", "server"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lease: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(lease["mode"], "read");
    assert_eq!(lease["repository"]["id"], registration["id"]);
    assert_eq!(lease["repository"]["path"], related.to_str().unwrap());
    for args in [
        vec!["acquire", "repo", "server", "--workspace", "consumer"],
        vec!["leases", "repo", "--workspace", "consumer"],
        vec!["status", "consumer"],
    ] {
        let rendered = fixture.run(&args);
        assert!(rendered.status.success());
        assert!(String::from_utf8_lossy(&rendered.stdout).contains(related.to_str().unwrap()));
    }
    assert_eq!(
        fs::read_to_string(related.join("DESIGN.MD")).unwrap(),
        "server design\n"
    );
    fixture.ok(&["acquire", "repo", "server", "--workspace", "other-consumer"]);
    let overview = fixture.ok(&["leases", "repo", "--workspace", "consumer"]);
    assert_eq!(overview["pools"][0]["used"], 1);
    assert_eq!(overview["pools"][0]["resources"][0]["readers"], 2);
    assert_eq!(
        overview["pools"][0]["resources"][0]["write_available"],
        false
    );
    let write = fixture.run(&[
        "acquire",
        "resource",
        "server",
        "--workspace",
        "consumer",
        "--mode",
        "write",
        "--name",
        "writer",
    ]);
    assert!(!write.status.success());
    fixture.restart();
    assert_eq!(
        fixture.ok(&["acquire", "repo", "server", "--workspace", "consumer"]),
        lease
    );
    let removal = fixture.run(&["repo", "rm", "server", "--yes"]);
    assert!(!removal.status.success());
    assert!(related.join("DESIGN.MD").exists());
    // Definition edits do not change the recorded checkout on renewal or release.
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='missing'\n",
    );
    assert_eq!(
        fixture.ok(&["acquire", "repo", "server", "--workspace", "consumer"]),
        lease
    );
    set_repository_toml(&fixture, "[resources.server]\nkind='repo'\nrepo='server'\n");
    let before = git(&related, &["status", "--porcelain"]);
    let head = git(&related, &["rev-parse", "HEAD"]);
    let worktrees = git(&related, &["worktree", "list", "--porcelain"]);
    fixture.ok(&["rm", "--wait", "consumer", "--yes"]);
    assert_eq!(git(&related, &["status", "--porcelain"]), before);
    assert_eq!(git(&related, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(&related, &["worktree", "list", "--porcelain"]),
        worktrees
    );
    fixture.ok(&[
        "release",
        "repo",
        "server",
        "--workspace",
        "other-consumer",
        "--name",
        "default",
    ]);
    fixture.ok(&["repo", "rm", "server", "--yes"]);
    assert!(!related.exists());
}

#[test]
fn acquire_picks_an_omitted_resource_of_its_kind() {
    let fixture = Fixture::with_config(Some("[resources.lock]\n[resources.workers]\ncapacity=2\n"));
    let related = init_repo(fixture.root.path(), "server", &[("tracked", "server\n")]);
    fixture.ok(&["repo", "add", related.to_str().unwrap(), "--name", "server"]);
    set_repository_toml(&fixture, "[resources.server]\nkind='repo'\nrepo='server'\n");
    fixture.add("consumer");
    let args = ["acquire", "resource", "--workspace", "consumer"];
    let (output, rows) = fixture.pick(&args, "workers", "");
    assert!(output.status.success(), "{output:?}");
    assert!(
        rows.contains("lock\t") && rows.contains("workers\t"),
        "{rows:?}"
    );
    assert!(!rows.contains("server\t"), "{rows:?}");
    let (output, rows) = fixture.pick(
        &["acquire", "repo", "--workspace", "consumer"],
        "server",
        "",
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rows.lines().count(), 1, "{rows:?}");
    let pools: Vec<_> = fixture.ok(&["status", "consumer"])["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|lease| lease["pool"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(pools.len(), 2, "{pools:?}");
    assert!(pools.contains(&"workers".into()) && pools.contains(&"server".into()));
}

#[test]
fn acquire_port_picks_an_omitted_configured_port() {
    let fixture = Fixture::new();
    fixture.add("server");
    let none = fixture.run(&["acquire", "port", "--workspace", "server"]);
    assert!(!none.status.success(), "{none:?}");
    set_repository_toml(&fixture, "[ports.web]\n[ports.api]\n");
    let (output, rows) = fixture.pick(&["acquire", "port", "--workspace", "server"], "web", "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains("api\t") && rows.contains("web\t"), "{rows:?}");
    let reserved = fixture.ok(&["leases", "port", "--workspace", "server"])["reserved"].clone();
    assert_eq!(reserved.as_array().unwrap().len(), 1, "{reserved}");
    assert_eq!(reserved[0]["name"], "web");
}

#[test]
fn repository_resources_select_registrations_by_remote_url() {
    let fixture = Fixture::new();
    let related = init_repo(fixture.root.path(), "server", &[("tracked", "server\n")]);
    git(
        &related,
        &[
            "remote",
            "add",
            "origin",
            "https://example.test/team/server.git",
        ],
    );
    let registration = fixture.ok(&["repo", "add", related.to_str().unwrap(), "--name", "api"]);
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='git@example.test:team/server.git'\n",
    );
    fixture.add("consumer");
    let lease = fixture.ok(&["acquire", "repo", "server", "--workspace", "consumer"]);
    assert_eq!(lease["repository"]["id"], registration["id"]);
    assert_eq!(lease["repository"]["path"], related.to_str().unwrap());
    let repos = fixture.ok(&["leases", "repo", "--workspace", "consumer"]);
    assert_eq!(repos["pools"][0]["name"], "server");
    assert_eq!(repos["leases"][0]["id"], lease["id"]);
    fixture.ok(&[
        "release",
        "repo",
        "server",
        "--workspace",
        "consumer",
        "--name",
        "default",
    ]);
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='https://example.test/team/other.git'\n",
    );
    let missing = fixture.run(&["acquire", "repo", "server", "--workspace", "consumer"]);
    assert!(!missing.status.success());
}

#[test]
fn repository_resources_validate_targets_before_claiming_and_bind_approvals() {
    let fixture = Fixture::new();
    let related = init_repo(fixture.root.path(), "server", &[("tracked", "server\n")]);
    fixture.ok(&["repo", "add", related.to_str().unwrap(), "--name", "server"]);
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='server'\nrequires_approval=true\n",
    );
    fixture.add("consumer");
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='missing'\n",
    );
    let missing = fixture.run(&["acquire", "repo", "server", "--workspace", "consumer"]);
    assert!(!missing.status.success());
    assert!(
        fixture.ok(&["status", "consumer"])["resources"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    set_repository_toml(
        &fixture,
        "[resources.server]\nkind='repo'\nrepo='server'\nrequires_approval=true\n",
    );
    let moved = fixture.root.path().join("moved-server");
    fs::rename(&related, &moved).unwrap();
    let unavailable = fixture.run(&["acquire", "repo", "server", "--workspace", "consumer"]);
    assert!(!unavailable.status.success());
    fs::rename(&moved, &related).unwrap();
    let pending = pending_approval(scoped_command(
        &fixture,
        "consumer",
        &[
            "acquire",
            "repo",
            "server",
            "--reason",
            "Read server contract",
        ],
    ));
    assert_eq!(
        pending["specification"]["repository"]["path"],
        related.to_str().unwrap()
    );
    fixture.ok(&[
        "approvals",
        "--approve",
        pending["number"].as_str().unwrap(),
    ]);
    // Reusing the configured name for another registration cannot reuse approval.
    fixture.ok(&["repo", "rm", "server", "--yes"]);
    let replacement = init_repo(
        fixture.root.path(),
        "replacement",
        &[("tracked", "other\n")],
    );
    fixture.ok(&[
        "repo",
        "add",
        replacement.to_str().unwrap(),
        "--name",
        "server",
    ]);
    let changed = scoped_command(&fixture, "consumer", &["acquire", "repo", "server"]);
    assert!(!changed.status.success());
    fixture.ok(&[
        "release",
        "repo",
        "server",
        "--workspace",
        "consumer",
        "--name",
        "default",
    ]);
    let pending = pending_approval(scoped_command(
        &fixture,
        "consumer",
        &[
            "acquire",
            "repo",
            "server",
            "--reason",
            "Read replacement contract",
        ],
    ));
    fixture.ok(&[
        "approvals",
        "--approve",
        pending["number"].as_str().unwrap(),
    ]);
    let granted = scoped_command(&fixture, "consumer", &["acquire", "repo", "server"]);
    assert!(
        granted.status.success(),
        "{}",
        String::from_utf8_lossy(&granted.stderr)
    );
    let lease: Value = serde_json::from_slice(&granted.stdout).unwrap();
    assert_eq!(lease["repository"]["path"], replacement.to_str().unwrap());
}
