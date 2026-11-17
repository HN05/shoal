mod support;

use std::{fs, path::Path, process::Output};

use serde_json::{Value, json};
use support::{
    cli as command, daemon::DaemonGuard, daemon_client, printed, shoal_binary, wait::wait_for,
};
use tempfile::TempDir;

/// A daemon in a temporary home and state directory, with its global
/// config.
struct Machine {
    // Dropped first, so the daemon stops before its directory goes.
    _guard: DaemonGuard,
    root: TempDir,
}

impl Machine {
    fn start(config: &str) -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        Self::start_in(root, config)
    }

    fn start_in(root: TempDir, config: &str) -> Self {
        let file = root.path().join(".config/shoal/config.toml");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, config).unwrap();
        let guard = DaemonGuard::start(root.path(), &mut command(root.path()));
        Self {
            _guard: guard,
            root,
        }
    }

    /// The hub, listening on a port of loopback the system picks.
    fn hub() -> Self {
        Self::start("[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n")
    }

    fn run(&self, args: &[&str]) -> Output {
        command(self.root.path()).args(args).output().unwrap()
    }

    /// The JSON `args` print, which must succeed.
    fn json(&self, args: &[&str]) -> Value {
        let output = self.run(&[args, &["--json"]].concat());
        assert!(output.status.success(), "{}", printed(&output));
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// The command that reaches this daemon's socket from another machine.
    fn connect(&self) -> String {
        let state = self.root.path().join("state");
        format!(
            "{} --state-dir {} daemon connect",
            shoal_binary(),
            state.display()
        )
    }

    /// The hub's members, by name, once `ready` holds of them.
    fn members_once(&self, ready: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        wait_for("the hub's members", || {
            let overview = self.json(&["hub", "ls"]);
            let members = overview["members"].as_array().unwrap().clone();
            ready(&members).then_some(members)
        })
    }

    /// This daemon's hub role once `ready` holds of it.
    fn role_once(&self, ready: impl Fn(&Value) -> bool) -> Value {
        wait_for("the daemon's hub role", || {
            let status = self.json(&["status"]);
            let role = status["daemon"]["hub"].clone();
            ready(&role).then_some(role)
        })
    }

    /// Join `hub` through the command transport, as `name`.
    fn join_through_command(&self, hub: &Machine, name: &str) -> Value {
        self.json(&["hub", "join", "--connect", &hub.connect(), "--name", name])
    }

    fn connected(&self) -> Value {
        self.role_once(|role| role["connected"] == true)
    }
}

fn named<'a>(members: &'a [Value], name: &str) -> Option<&'a Value> {
    members.iter().find(|member| member["name"] == name)
}

#[test]
fn a_machine_joins_through_a_command_and_keeps_its_link() {
    let hub = Machine::hub();
    let laptop = Machine::start("");
    let membership = laptop.join_through_command(&hub, "laptop");
    let members = hub.members_once(|members| {
        named(members, "laptop").is_some_and(|member| member["connected"] == true)
    });
    assert_eq!(
        named(&members, "laptop").unwrap()["id"],
        membership["member"]
    );
    assert_eq!(named(&members, "laptop").unwrap()["kind"], "machine");
    assert!(members.iter().any(|member| member["hub"] == true));
    let role = laptop.connected();
    assert_eq!(role["membership"]["member"], membership["member"]);
    assert_eq!(
        role["route"]["command"].as_array().unwrap().last().unwrap(),
        "connect"
    );
    // A joined machine asks the hub for codes, which name the hub's URL.
    let listener = hub.json(&["hub", "ls"])["listener"]["url"].clone();
    let pairing = laptop.json(&["hub", "pair"]);
    let code = pairing["code"].as_str().unwrap();
    assert!(code.starts_with(listener.as_str().unwrap()), "{code}");
}

#[test]
fn a_machine_joins_over_the_listener_with_a_code_it_uses_once() {
    let hub = Machine::hub();
    let laptop = Machine::start("");
    let pairing = hub.json(&["hub", "pair"]);
    let code = pairing["code"].as_str().unwrap();
    let membership = laptop.json(&["hub", "join", code, "--name", "laptop"]);
    assert!(
        membership["through"]["url"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:")
    );
    laptop.connected();
    hub.members_once(|members| {
        named(members, "laptop").is_some_and(|member| member["connected"] == true)
    });
    let desk = Machine::start("");
    let reused = desk.run(&["hub", "join", code, "--name", "desk"]);
    assert!(!reused.status.success(), "{}", printed(&reused));
    let error = String::from_utf8_lossy(&reused.stderr);
    assert!(error.contains("pairing code"), "{error}");
}

#[test]
fn the_hub_refuses_a_revoked_credential() {
    let hub = Machine::hub();
    let laptop = Machine::start("");
    laptop.join_through_command(&hub, "laptop");
    laptop.connected();
    hub.json(&["hub", "revoke", "laptop"]);
    let role = laptop.role_once(|role| {
        role["connected"] == false
            && role["problem"]
                .as_str()
                .is_some_and(|p| p.contains("revoked"))
    });
    assert_eq!(role["membership"]["name"], "laptop");
    let members = hub.json(&["hub", "ls"])["members"]
        .as_array()
        .unwrap()
        .clone();
    assert!(named(&members, "laptop").is_none());
}

#[test]
fn the_hub_refuses_a_copy_of_a_machines_credential() {
    let hub = Machine::hub();
    let laptop = Machine::start("");
    laptop.join_through_command(&hub, "laptop");
    laptop.connected();
    let copy = tempfile::tempdir_in("/tmp").unwrap();
    fs::create_dir_all(copy.path().join("state")).unwrap();
    let database = laptop.root.path().join("state/state.db");
    let copied = copy.path().join("state/state.db");
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute("VACUUM INTO ?1", [copied.to_str().unwrap()])
        .unwrap();
    let config = fs::read_to_string(laptop.root.path().join(".config/shoal/config.toml")).unwrap();
    let copy = Machine::start_in(copy, &config);
    let role = copy.role_once(|role| {
        role["problem"]
            .as_str()
            .is_some_and(|problem| problem.contains("shoal hub join"))
    });
    assert_eq!(role["connected"], false);
    assert_eq!(laptop.json(&["status"])["daemon"]["hub"]["connected"], true);
}

#[test]
fn the_hub_refuses_another_protocol_and_alerts_the_user() {
    let hub = Machine::hub();
    let protocol = daemon_client::protocol(hub.root.path());
    let stream = daemon_client::connect(hub.root.path());
    let hello = json!({"version": "0.1.0", "intent": {"join": {"name": "old-laptop"}}});
    daemon_client::send(
        &stream,
        &json!({"protocol": protocol + 1, "id": 1, "method": {"hub_link": hello}}),
    );
    let refused = daemon_client::receive(&stream);
    assert_eq!(refused["refused"]["reason"], "version", "{refused}");
    let notifications = hub.json(&["alerts"]);
    let alert = (notifications.as_array().unwrap().iter())
        .find(|notification| notification["kind"] == "hub_refused")
        .unwrap_or_else(|| panic!("no hub alert in {notifications}"));
    let message = alert["message"].as_str().unwrap();
    assert!(
        message.contains("joining") && message.contains("0.1.0"),
        "{message}"
    );
}

#[test]
fn a_join_through_a_command_that_finds_no_shoal_names_the_fix() {
    let laptop = Machine::start("");
    let missing = "sh -c 'echo \"sh: shoal: command not found\" >&2; exit 127'";
    let output = laptop.run(&["hub", "join", "--connect", missing]);
    assert!(!output.status.success(), "{}", printed(&output));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("absolute path"), "{error}");
    assert_eq!(laptop.json(&["status"])["daemon"]["hub"], Value::Null);
}

#[test]
fn daemon_connect_starts_no_daemon() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let output = command(root.path())
        .args(["daemon", "connect"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "{}", printed(&output));
    assert!(!Path::new(&root.path().join("state/daemon.sock")).exists());
}

#[test]
fn a_machine_that_leaves_loses_its_credential_and_route() {
    let hub = Machine::hub();
    let laptop = Machine::start("");
    laptop.join_through_command(&hub, "laptop");
    laptop.connected();
    let left = laptop.json(&["hub", "leave"]);
    assert_eq!(left["revoked"], true);
    let members = hub.json(&["hub", "ls"])["members"]
        .as_array()
        .unwrap()
        .clone();
    assert!(named(&members, "laptop").is_none());
    assert_eq!(laptop.json(&["status"])["daemon"]["hub"], Value::Null);
}

#[test]
fn a_machine_joins_again_after_it_left_or_was_revoked() {
    let hub = Machine::hub();
    let laptop = Machine::start("");
    let first = laptop.join_through_command(&hub, "laptop");
    laptop.connected();
    laptop.json(&["hub", "leave"]);
    let second = laptop.join_through_command(&hub, "laptop");
    assert_ne!(second["member"], first["member"]);
    laptop.connected();
    hub.json(&["hub", "revoke", second["member"].as_str().unwrap()]);
    laptop.role_once(|role| {
        (role["problem"].as_str()).is_some_and(|problem| problem.contains("revoked"))
    });
    let third = laptop.join_through_command(&hub, "laptop");
    let role = laptop.connected();
    assert_eq!(role["membership"]["member"], third["member"]);
    let members = hub.members_once(|members| named(members, "laptop").is_some());
    assert_eq!(
        members
            .iter()
            .filter(|member| member["name"] == "laptop")
            .count(),
        1
    );
}

#[test]
fn the_hub_takes_no_link_from_a_space_s_scope() {
    let hub = Machine::hub();
    let protocol = daemon_client::protocol(hub.root.path());
    let opening =
        json!({"protocol": protocol, "id": 1, "scope": "token", "method": {"hub_link": {}}});
    let answer = daemon_client::exchange(hub.root.path(), &opening);
    assert_eq!(answer["type"], "error", "{answer}");
    let members = hub.json(&["hub", "ls"])["members"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(members.len(), 1);
}

#[test]
fn joins_at_once_leave_one_membership() {
    let hub = Machine::hub();
    let laptop = Machine::start("");
    let joins = std::thread::scope(|scope| {
        let joining = ["first", "second"].map(|name| {
            let (laptop, hub) = (&laptop, &hub);
            scope.spawn(move || {
                let connect = hub.connect();
                laptop.run(&[
                    "hub",
                    "join",
                    "--connect",
                    &connect,
                    "--name",
                    name,
                    "--json",
                ])
            })
        });
        joining.map(|join| join.join().unwrap())
    });
    let joined: Vec<_> = (joins.iter())
        .filter(|output| output.status.success())
        .collect();
    assert_eq!(joined.len(), 1);
    let membership: Value = serde_json::from_slice(&joined[0].stdout).unwrap();
    let role = laptop.connected();
    assert_eq!(role["membership"]["member"], membership["member"]);
}

#[test]
fn a_machine_that_revokes_itself_hears_that_it_did() {
    let hub = Machine::hub();
    let laptop = Machine::start("");
    let membership = laptop.join_through_command(&hub, "laptop");
    laptop.connected();
    let revoked = laptop.json(&["hub", "revoke", membership["member"].as_str().unwrap()]);
    assert_eq!(revoked["id"], membership["member"]);
    laptop.role_once(|role| {
        (role["problem"].as_str()).is_some_and(|problem| problem.contains("revoked"))
    });
}

#[test]
fn the_hub_keeps_a_joiner_s_name_short() {
    let hub = Machine::hub();
    let laptop = Machine::start("");
    let long = "laptop".repeat(5_000);
    laptop.join_through_command(&hub, &long);
    let members = hub.members_once(|members| members.len() == 2);
    let joined = members
        .iter()
        .find(|member| member["hub"] == false)
        .unwrap();
    let name = joined["name"].as_str().unwrap();
    assert!(
        name.starts_with("laptop") && name.len() <= 300,
        "{}",
        name.len()
    );
}
