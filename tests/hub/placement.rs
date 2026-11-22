//! Placement: a detached start that names no machine goes where the hub's
//! placement policy puts it, among the machines that have its repository
//! and agent tool.
use serde_json::Value;

use super::{
    directory::{give_origin, hub_machine, join, workspace},
    routing::{ready_at, scope_of},
    support::{
        fixture::{Base, home},
        git, printed,
        wait::wait_for,
    },
};

/// An agent that runs until its space stops it.
const AGENT: &str = "[agents.pi]\ncommand = ['sh', '-c', 'exec sleep 600']\n";

/// An intermittent hub with a coordinator workspace in a repository with an
/// origin, and an always_on server joined to it that registered a clone of
/// that repository.
struct Machines {
    hub: Base,
    _server: Base,
    server_id: String,
    coordinator: String,
}

/// The [`Machines`], with the `server` settings, which set the server's
/// `machine` keys, once the hub has the server's capacity.
fn machines(server: &str) -> Machines {
    let hub = Base::start(
        home(),
        &format!(
            "machine.availability = \"intermittent\"\n{AGENT}[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n"
        ),
        "app",
        &[],
    );
    let origin = give_origin(&hub);
    let server = Base::start(
        home(),
        &format!("machine.availability = \"always_on\"\n{server}{AGENT}"),
        "notes",
        &[],
    );
    let there = server.root.path();
    git(there, &["clone", "-q", &origin, "clone"]);
    server.ok(&["repo", "add", there.join("clone").to_str().unwrap()]);
    let server_id = join(&server, &hub, "server");
    let coordinator = workspace(&hub, "coordinator");
    ready_at(&server, &coordinator, &hub_machine(&hub));
    wait_for("the server's capacity", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let server = (members.as_array().unwrap().iter())
            .find(|member| member["id"] == server_id.as_str())
            .cloned()?;
        // The clone; its own repository has no origin.
        let repositories = server["capacity"]["repositories"].as_array()?;
        (!repositories.is_empty()).then_some(())
    });
    Machines {
        hub,
        _server: server,
        server_id,
        coordinator,
    }
}

/// Where the hub placed the start that `args` make in the coordinator,
/// under its scope, which starts its agent detached, as it printed it.
fn placed(machines: &Machines, args: &[&str]) -> Value {
    let token = scope_of(&machines.hub, &machines.coordinator);
    let mut command = machines.hub.command();
    command.env("SHOAL_SCOPE_TOKEN", &token);
    let repo = machines.hub.repo.to_str().unwrap();
    command
        .args(["start", repo, "--agent", "pi", "--json"])
        .args(args);
    let started = machines.hub.output(&mut command);
    assert!(started.status.success(), "{}", printed(&started));
    let mut shown = serde_json::Deserializer::from_slice(&started.stdout).into_iter::<Value>();
    shown.next().unwrap().unwrap()
}

#[test]
fn a_heavy_start_goes_to_an_always_on_machine_with_a_free_heavy_slot() {
    let machines = machines("");
    let placement = placed(&machines, &["--branch", "child"]);
    assert_eq!(
        placement["machine"],
        machines.server_id.as_str(),
        "{placement}"
    );
    assert_eq!(placement["reason"], "heavy_slot", "{placement}");
    assert_eq!(placement["policy"], "balanced", "{placement}");
    // The server created the space.
    wait_for("the child on the server", || {
        let listed = machines.hub.ok(&["list"]);
        (listed.as_array().unwrap().iter())
            .find(|space| {
                space["name"] == "child" && space["machine"] == machines.server_id.as_str()
            })
            .map(|_| ())
    });
}

#[test]
fn a_light_start_goes_to_a_machine_without_heavy_agents_and_a_heavy_one_stays() {
    let machines = machines("machine.max_heavy_agents = 0\n");
    let heavy = placed(&machines, &["--branch", "child"]);
    assert_eq!(heavy["reason"], "caller", "{heavy}");
    let light = placed(&machines, &["--mode", "chat", "--name", "notes"]);
    assert_eq!(light["machine"], machines.server_id.as_str(), "{light}");
    assert_eq!(light["reason"], "no_heavy_agents", "{light}");
}
