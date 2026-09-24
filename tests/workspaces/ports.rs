use crate::support::{Fixture, git};
use serde_json::Value;
use std::{fs, path::Path, process::Stdio};

#[test]
fn ports_are_named_idempotent_exported_and_released_with_the_workspace() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    fixture.add("second");
    let web = fixture.ok(&[
        "port",
        "acquire",
        "web",
        "first",
        "--reason",
        "Frontend dev server",
    ]);
    assert_eq!(web["env_var"], "SHOAL_PORT_WEB");
    assert_eq!(web["reason"], "Frontend dev server");
    assert_eq!(fixture.ok(&["port", "acquire", "web", "first"]), web);
    let port = web["port"].to_string();
    assert!(
        !fixture
            .run(&["port", "acquire", "web", "second", "--port", &port])
            .status
            .success()
    );
    let api = fixture.ok(&["port", "acquire", "api", "first", "--env", "API_PORT"]);
    let output = fixture.run(&[
        "exec",
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
        fixture.ok(&["inspect", "first"])["ports"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let nested = fixture.run(&[
        "exec",
        "first",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "exec",
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
    assert!(!fixture.run(&["rm", "first"]).status.success());
    assert_eq!(
        fixture.ok(&["port", "first"])["reserved"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    fixture.ok(&["rm", "first", "--yes", "--keep-branch"]);
    assert_eq!(
        fixture.ok(&["port", "--all"])[0]["reserved"],
        serde_json::json!([])
    );
    fixture.ok(&["port", "acquire", "web", "second", "--port", &port]);
    fixture.ok(&["port", "release", "web", "second"]);
    assert_eq!(
        fixture.ok(&["port", "second"])["reserved"],
        serde_json::json!([])
    );
    fixture.ok(&["rm", "second"]);
}

#[test]
fn ports_avoid_listeners_and_concurrent_allocations_are_unique_and_persistent() {
    let mut fixture = Fixture::new();
    fixture.add("ports");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let occupied = listener.local_addr().unwrap().port().to_string();
    assert!(
        !fixture
            .run(&["port", "acquire", "occupied", "ports", "--port", &occupied])
            .status
            .success()
    );
    let mut children = Vec::new();
    for i in 0..8 {
        children.push(
            fixture
                .command()
                .args(["--json", "port", "acquire", &format!("server{i}"), "ports"])
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
    let before = fixture.ok(&["port", "ports"]);
    assert!(fixture.run(&["daemon", "stop"]).status.success());
    fixture.daemon.wait().unwrap();
    fixture.daemon = fixture
        .command()
        .args(["daemon", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fixture.wait_ready();
    assert_eq!(fixture.ok(&["port", "ports"]), before);
    fixture.ok(&["rm", "ports"]);
}

#[test]
fn repository_config_sets_the_automatic_port_range() {
    let fixture = Fixture::new();
    let workspace = fixture.add("ranged");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let [first, second] = [(); 2].map(|()| {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    });
    fs::write(
        path.join(".shoal.toml"),
        format!("[ports]\nstart={first}\nend={first}\n"),
    )
    .unwrap();
    assert_eq!(
        fixture.ok(&["port", "acquire", "web", "ranged"])["port"],
        first
    );
    assert!(
        !fixture
            .run(&["port", "acquire", "api", "ranged"])
            .status
            .success()
    );
    // The saved config is the top layer, bound by bound.
    let saved = fixture.root.path().join("saved.toml");
    let save = |text: &str| {
        fs::write(&saved, text).unwrap();
        fixture.ok(&[
            "repo",
            "config",
            fixture.repo.to_str().unwrap(),
            "--file",
            saved.to_str().unwrap(),
        ]);
    };
    save(&format!("[ports]\nstart={second}\nend={second}\n"));
    assert_eq!(
        fixture.ok(&["port", "acquire", "api", "ranged"])["port"],
        second
    );
    // The layered range must stay nonempty: `end = 1` under the file's `start`.
    save("[ports]\nend=1\n");
    let output = fixture.run(&["port", "acquire", "db", "ranged"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("nonempty range"));
}

#[test]
fn configured_port_range_exhaustion_and_release() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let number = listener.local_addr().unwrap().port();
    let fixture = Fixture::with_config(Some(&format!("[ports]\nstart={number}\nend={number}\n")));
    fixture.add("limited");
    assert!(
        !fixture
            .run(&["port", "acquire", "web", "limited"])
            .status
            .success()
    );
    drop(listener);
    let lease = fixture.ok(&["port", "acquire", "web", "limited"]);
    assert_eq!(lease["port"], number);
    assert!(
        !fixture
            .run(&["port", "acquire", "api", "limited"])
            .status
            .success()
    );
    fixture.ok(&["port", "release", "web", "limited"]);
    assert_eq!(
        fixture.ok(&["port", "acquire", "api", "limited"])["port"],
        number
    );
    fixture.ok(&["rm", "limited"]);
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
    fixture.add("healthy");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let overview = fixture
        .command()
        .current_dir(path)
        .args(["--json", "port"])
        .output()
        .unwrap();
    assert!(overview.status.success());
    let overview: Value = serde_json::from_slice(&overview.stdout).unwrap();
    assert_eq!(overview["configured"]["web"]["port"], preferred);
    assert_eq!(overview["reserved"], serde_json::json!([]));
    assert_eq!(fixture.ok(&["port", "list", "configured"]), overview);
    let human = fixture.run(&["port", "configured"]);
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains(&format!(
        "web: not reserved (preferred: {preferred}; conflicts: suggest)"
    )));
    for old in [
        vec!["ports", "configured"],
        vec!["resources", "configured"],
        vec!["port", "reserve", "web", "configured"],
    ] {
        assert!(
            !fixture.run(&old).status.success(),
            "old command still works: {old:?}"
        );
    }
    let proposal = fixture.run(&["--json", "port", "acquire", "web", "configured"]);
    assert_eq!(proposal.status.code(), Some(2));
    let proposal: Value = serde_json::from_slice(&proposal.stdout).unwrap();
    assert_eq!(proposal["reserved"], false);
    assert_eq!(
        fixture.ok(&["port", "configured"])["reserved"],
        serde_json::json!([])
    );
    let accepted = fixture.ok(&[
        "port",
        "acquire",
        "web",
        "configured",
        "--port",
        &proposal["suggested_port"].to_string(),
    ]);
    assert_eq!(accepted["env_var"], "PORT");
    assert_eq!(
        fixture.ok(&["port", "acquire", "web", "configured"]),
        accepted
    );
    fixture.ok(&["port", "release", "web", "configured"]);
    let automatic = fixture.ok(&[
        "port",
        "acquire",
        "web",
        "configured",
        "--on-conflict",
        "auto",
    ]);
    assert_ne!(automatic["port"], preferred);
    assert_eq!(
        fixture.ok(&["port", "acquire", "web", "configured"]),
        automatic
    );
    fs::create_dir(path.join(".shoal")).unwrap();
    fs::rename(path.join(".shoal.toml"), path.join(".shoal/config.toml")).unwrap();
    fixture.ok(&["port", "configured"]);
    fs::write(path.join(".shoal.toml"), "").unwrap();
    assert!(!fixture.run(&["port", "configured"]).status.success());
    for noun in ["port", "resource"] {
        let output = fixture.run(&["--json", noun, "--all"]);
        assert_eq!(output.status.code(), Some(1));
        let overviews: Value = serde_json::from_slice(&output.stdout).unwrap();
        let overviews = overviews.as_array().unwrap();
        assert_eq!(overviews.len(), 2);
        assert!(overviews.iter().any(|overview| {
            overview["workspace"]["name"] == "configured" && overview["error"].is_string()
        }));
        assert!(overviews.iter().any(|overview| {
            overview["workspace"]["name"] == "healthy" && overview["error"].is_null()
        }));
    }
}
