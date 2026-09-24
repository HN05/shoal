use crate::support::{Fixture, commit_resource_config, git};
use serde_json::Value;
use std::{fs, path::Path, process::Stdio, thread, time::Duration};

pub(super) const RESOURCE_CONFIG: &str = r#"
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
fn resources_enforce_pool_and_member_capacity_and_named_permits() {
    let fixture = Fixture::with_config(Some(RESOURCE_CONFIG));
    fixture.add("first");
    fixture.add("second");
    fixture.add("third");
    assert!(
        fixture
            .ok(&["resource", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .all(|overview| overview["leases"] == serde_json::json!([]))
    );
    let first = fixture.ok(&["resource", "acquire", "devices", "first"]);
    assert_eq!(first["resource"], "alpha");
    assert_eq!(first["reason"], "Alpha test device");
    assert_eq!(
        fixture.ok(&["resource", "acquire", "devices", "first"]),
        first
    );
    assert_eq!(
        fixture
            .run(&[
                "resource",
                "acquire",
                "devices",
                "second",
                "--resource",
                "alpha"
            ])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&[
        "resource",
        "acquire",
        "devices",
        "second",
        "--resource",
        "beta",
    ]);
    let busy = fixture.run(&[
        "--json",
        "resource",
        "acquire",
        "devices",
        "third",
        "--resource",
        "beta",
    ]);
    assert_eq!(busy.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&busy.stdout).unwrap()["acquired"],
        false
    );
    fixture.ok(&["resource", "release", "devices", "first"]);
    fixture.ok(&[
        "resource",
        "acquire",
        "devices",
        "third",
        "--resource",
        "beta",
    ]);
    let overview = fixture.ok(&["resource", "third"]);
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
    fixture.ok(&["resource", "release", "devices", "second"]);
    fixture.ok(&[
        "resource",
        "acquire",
        "devices",
        "third",
        "--resource",
        "beta",
        "--name",
        "parallel",
        "--reason",
        "Parallel job",
    ]);
    assert_eq!(
        fixture.ok(&["inspect", "third"])["resources"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let lock = fixture.ok(&["resource", "acquire", "signing", "first"]);
    assert_eq!(lock["resource"], "signing");
    assert_eq!(lock["scope"], "global");
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "signing", "second"])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&["rm", "first"]);
    fixture.ok(&["resource", "acquire", "signing", "second"]);
    fixture.ok(&["rm", "third"]);
    let overviews = fixture.ok(&["resource", "--all"]);
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
                    "resource",
                    "acquire",
                    "workers",
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
    let leases = fixture.ok(&["resource", "--all"]);
    assert_eq!(leases[0]["leases"].as_array().unwrap().len(), 3);
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
    assert_eq!(fixture.ok(&["resource", "--all"]), leases);
    let mut waiter = fixture
        .command()
        .args([
            "--json", "resource", "acquire", "workers", "worker", "--name", "waiter", "--wait",
            "10",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(150));
    assert!(waiter.try_wait().unwrap().is_none());
    fixture.ok(&[
        "resource",
        "release",
        "workers",
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
    assert!(!fixture.run(&["rm", "worker"]).status.success());
    assert_eq!(
        fixture.ok(&["resource", "--all"])[0]["leases"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    fixture.ok(&["rm", "worker", "--yes", "--keep-branch"]);
    assert!(
        fixture
            .ok(&["resource", "--all"])
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
    let lease = fixture.ok(&["resource", "acquire", "signing", "first"]);
    assert!(lease["scope"].as_str().unwrap().starts_with("repo/"));
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "signing", "second"])
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
    let conflict = fixture.run(&["resource", "acquire", "signing", "second"]);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("definition changed"));
    let overview = fixture.ok(&["resource", "second"]);
    let signing = overview["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "signing")
        .unwrap();
    assert_eq!(signing["configuration_matches"], false);
    fs::remove_file(first_path.join(".shoal.toml")).unwrap();
    fixture.ok(&["resource", "release", "signing", "first"]);
    fixture.ok(&["resource", "acquire", "signing", "second"]);
    fixture.ok(&[
        "resource", "acquire", "signing", "second", "--name", "another",
    ]);
    let output = fixture
        .command()
        .current_dir(second_path)
        .args(["--json", "resource"])
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
    let second = fixture.ok(&["add", other.to_str().unwrap(), "second"]);
    let local_first = fixture.ok(&["resource", "acquire", "local", "first"]);
    let local_second = fixture.ok(&["resource", "acquire", "local", "second"]);
    assert_ne!(local_first["scope"], local_second["scope"]);
    fixture.ok(&["resource", "acquire", "machine", "first"]);
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "machine", "second"])
            .status
            .code(),
        Some(2)
    );
    let scoped = |args: &[&str]| {
        fixture
            .command()
            .args(["exec", "first", "--", env!("CARGO_BIN_EXE_shoal")])
            .args(args)
            .output()
            .unwrap()
    };
    assert!(scoped(&["resource", "acquire", "local"]).status.success());
    assert!(
        !scoped(&["resource", "release", "local", "second"])
            .status
            .success()
    );
    assert!(!scoped(&["resource", "second"]).status.success());
    let listed: Value =
        serde_json::from_slice(&scoped(&["--json", "resource", "--all"]).stdout).unwrap();
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
    let conflict = fixture.run(&["resource", "acquire", "machine", "second"]);
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("conflicts with the global"));
    fixture.ok(&["resource", "release", "local", "second"]);
}

#[test]
fn rwlock_readers_share_one_slot_and_writers_exclude_everyone() {
    let fixture = Fixture::with_config(Some("[resources.cache]\nkind='rwlock'\n"));
    fixture.add("first");
    fixture.add("second");
    let first = fixture.ok(&["resource", "acquire", "cache", "first", "--mode", "read"]);
    assert_eq!(first["mode"], "read");
    assert_eq!(
        fixture.ok(&["resource", "acquire", "cache", "first"]),
        first
    );
    assert!(
        !fixture
            .run(&["resource", "acquire", "cache", "first", "--mode", "write"])
            .status
            .success()
    );
    let children: Vec<_> = (0..16)
        .map(|i| {
            fixture
                .command()
                .args([
                    "--json",
                    "resource",
                    "acquire",
                    "cache",
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
    let overview = fixture.ok(&["resource", "first"]);
    let pool = &overview["pools"][0];
    assert_eq!(pool["used"], 1);
    assert_eq!(pool["available"], 0);
    assert_eq!(pool["resources"][0]["readers"], 17);
    assert_eq!(pool["resources"][0]["read_available"], true);
    assert_eq!(pool["resources"][0]["write_available"], false);
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "cache", "second", "--mode", "write"])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&["resource", "release", "cache", "first"]);
    // The final reader, not the first release, makes a writer possible.
    for i in 0..15 {
        fixture.ok(&[
            "resource",
            "release",
            "cache",
            "second",
            "--name",
            &format!("reader-{i}"),
        ]);
    }
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "cache", "first"])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&[
        "resource",
        "release",
        "cache",
        "second",
        "--name",
        "reader-15",
    ]);
    let writer = fixture.ok(&["resource", "acquire", "cache", "first"]);
    assert_eq!(writer["mode"], "write");
    for mode in ["read", "write"] {
        assert_eq!(
            fixture
                .run(&["resource", "acquire", "cache", "second", "--mode", mode])
                .status
                .code(),
            Some(2)
        );
    }
    let status = fixture.ok(&["resource", "second"]);
    assert_eq!(status["pools"][0]["resources"][0]["writers"], 1);
    assert_eq!(status["pools"][0]["resources"][0]["read_available"], false);
    fixture.ok(&["rm", "first"]);
    fixture.ok(&["resource", "acquire", "cache", "second", "--mode", "read"]);
}

#[test]
fn rwlock_mixed_pools_apply_capacity_to_occupied_members_and_filter_modes() {
    let fixture = Fixture::with_config(Some(
        "[resource_pools.mixed]\ncapacity=2\n[resource_pools.mixed.resources.a]\nkind='rwlock'\n[resource_pools.mixed.resources.b]\nkind='rwlock'\n[resource_pools.mixed.resources.worker]\ncapacity=2\n",
    ));
    fixture.add("owner");
    fixture.ok(&[
        "resource",
        "acquire",
        "mixed",
        "owner",
        "--resource",
        "worker",
        "--name",
        "job",
    ]);
    let reader = fixture.ok(&[
        "resource", "acquire", "mixed", "owner", "--mode", "read", "--name", "read-one",
    ]);
    assert_eq!(reader["resource"], "a");
    let more = fixture.ok(&[
        "resource", "acquire", "mixed", "owner", "--mode", "read", "--name", "read-two",
    ]);
    assert_eq!(more["resource"], "a");
    for resource in ["b", "worker"] {
        assert_eq!(
            fixture
                .run(&[
                    "resource",
                    "acquire",
                    "mixed",
                    "owner",
                    "--resource",
                    resource,
                    "--name",
                    "extra"
                ])
                .status
                .code(),
            Some(2)
        );
    }
    for (resource, mode) in [("worker", "read"), ("worker", "write"), ("a", "permit")] {
        let output = fixture.run(&[
            "resource",
            "acquire",
            "mixed",
            "owner",
            "--resource",
            resource,
            "--mode",
            mode,
            "--name",
            "invalid",
        ]);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("incompatible"));
    }
    fixture.ok(&["resource", "release", "mixed", "owner", "--name", "job"]);
    let writer = fixture.ok(&[
        "resource", "acquire", "mixed", "owner", "--mode", "write", "--name", "writer",
    ]);
    assert_eq!(writer["resource"], "b");
    let overview = fixture.ok(&["resource", "owner"]);
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
                    "resource",
                    "acquire",
                    "cache",
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
    let lease = fixture.ok(&["resource", "acquire", "cache", "reader", "--mode", "read"]);
    let writer = fixture.ok(&["resource", "acquire", "index", "writer", "--mode", "write"]);
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
    assert_eq!(
        fixture.ok(&["resource", "acquire", "cache", "reader", "--mode", "read"]),
        lease
    );
    assert_eq!(
        fixture.ok(&["resource", "acquire", "index", "writer", "--mode", "write"]),
        writer
    );
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "index", "reader", "--mode", "read"])
            .status
            .code(),
        Some(2)
    );
    let mut waiter = fixture
        .command()
        .args([
            "--json", "resource", "acquire", "cache", "writer", "--mode", "write", "--wait", "10",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(150));
    assert!(waiter.try_wait().unwrap().is_none());
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("unfinished"), "retain").unwrap();
    assert!(!fixture.run(&["rm", "reader"]).status.success());
    assert_eq!(fixture.ok(&["resource", "reader"])["leases"][0], lease);
    fixture.ok(&["resource", "release", "cache", "reader"]);
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
fn rwlock_scope_and_kind_drift_preserve_active_leases() {
    let fixture = Fixture::new();
    commit_resource_config(&fixture.repo, "[resources.cache]\nkind='rwlock'\n");
    let first = fixture.add("first");
    let second = fixture.add("second");
    let binary = env!("CARGO_BIN_EXE_shoal");
    let output = fixture.run(&[
        "exec", "first", "--", binary, "--json", "resource", "acquire", "cache", "--mode", "read",
    ]);
    assert!(output.status.success());
    let lease: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(lease["mode"], "read");
    assert!(
        !fixture
            .run(&[
                "exec", "second", "--", binary, "resource", "release", "cache", "first"
            ])
            .status
            .success()
    );
    let path = Path::new(second["path"].as_str().unwrap());
    fs::write(path.join(".shoal.toml"), "[resources.cache]\ncapacity=2\n").unwrap();
    assert!(
        !fixture
            .run(&["resource", "acquire", "cache", "second"])
            .status
            .success()
    );
    assert_eq!(
        fixture.ok(&["resource", "second"])["pools"][0]["configuration_matches"],
        false
    );
    // Release still works when its own definition is removed entirely.
    fs::remove_file(Path::new(first["path"].as_str().unwrap()).join(".shoal.toml")).unwrap();
    fixture.ok(&["resource", "release", "cache", "first"]);
    assert_eq!(
        fixture.ok(&["resource", "acquire", "cache", "second"])["mode"],
        "permit"
    );
}
