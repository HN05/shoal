//! Placement: a detached start that names no machine goes where the hub's
//! placement policy puts it, among the machines that have its repository
//! and agent tool.
use serde_json::Value;
use shoal_client::{
    connection::Connector,
    hub::{Placing, answer_placement},
};
use shoal_protocol::paths::Paths;
use shoal_schedule::placement::{Placed, Placement, Reason};

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
    machines_with("", server)
}

/// The [`machines`], with `hub` added to the hub's `[machine.hub]` table.
fn machines_with(hub: &str, server: &str) -> Machines {
    let hub = Base::start(
        home(),
        &format!(
            "machine.availability = \"intermittent\"\n{AGENT}[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n{hub}"
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

/// The hub's daemon as an app on its machine reaches it, as the user.
fn app(machines: &Machines) -> Connector {
    let state = machines.hub.root.path().join("state");
    Connector {
        paths: Paths {
            home: machines.hub.root.path().into(),
            socket: state.join("daemon.sock"),
            state,
        },
        scope: None,
    }
}

#[test]
fn an_external_placer_places_a_start_and_balanced_does_without_one() {
    let machines = machines_with("placement = \"external\"\n", "");
    let unplaced = placed(&machines, &["--branch", "first"]);
    assert_eq!(unplaced["policy"], "balanced", "{unplaced}");
    assert_eq!(unplaced["unanswered"], "no_placer", "{unplaced}");
    // An app on the hub's machine registers and keeps the next start there.
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let app = app(&machines);
    let mut placing = runtime.block_on(Placing::serve(&app)).unwrap();
    let hub_id = hub_machine(&machines.hub);
    let answered = runtime.spawn({
        let hub_id = hub_id.clone();
        async move {
            let asked = placing.next().await?;
            let placement = Placement {
                ranked: vec![Placed {
                    machine: hub_id.as_str().into(),
                    reason: Reason::Local,
                }],
                skipped: Vec::new(),
            };
            answer_placement(&app, asked.number, placement).await?;
            anyhow::Ok(asked.snapshot)
        }
    });
    let placement = placed(&machines, &["--branch", "second"]);
    assert_eq!(placement["policy"], "external", "{placement}");
    assert_eq!(placement["machine"], hub_id.as_str(), "{placement}");
    let snapshot = runtime.block_on(answered).unwrap().unwrap();
    assert!(snapshot.request.heavy);
    assert_eq!(snapshot.machines.len(), 2);
}

#[test]
fn a_placer_that_chooses_no_connected_machine_leaves_the_start_to_balanced() {
    let machines = machines_with("placement = \"external\"\n", "");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let app = app(&machines);
    let mut placing = runtime.block_on(Placing::serve(&app)).unwrap();
    let answered = runtime.spawn(async move {
        let asked = placing.next().await?;
        let placement = Placement {
            ranked: vec![Placed {
                machine: "gone".into(),
                reason: Reason::Local,
            }],
            skipped: Vec::new(),
        };
        answer_placement(&app, asked.number, placement).await
    });
    let placement = placed(&machines, &["--branch", "child"]);
    runtime.block_on(answered).unwrap().unwrap();
    assert_eq!(placement["policy"], "balanced", "{placement}");
    assert_eq!(placement["unanswered"], "failed", "{placement}");
    assert_eq!(
        placement["machine"],
        machines.server_id.as_str(),
        "{placement}"
    );
}
