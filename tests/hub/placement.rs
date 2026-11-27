//! Placement: a start that names no machine goes where the hub's placement
//! policy puts it, among the machines that have its repository and agent
//! tool.
use serde_json::Value;
use shoal_client::{
    connection::Connector,
    hub::{Placing, answer_placement},
    launches,
};
use shoal_protocol::{
    agent::Agent,
    launches::{Launch, StartTarget},
    paths::Paths,
};
use shoal_schedule::placement::Placement;

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
    // A chat's start carries it; a workspace's start prints it last.
    let shown = serde_json::Deserializer::from_slice(&started.stdout).into_iter::<Value>();
    let shown: Vec<Value> = shown.map(Result::unwrap).collect();
    let first = &shown[0];
    match &first["placement"] {
        Value::Null => shown.last().unwrap().clone(),
        placement => placement.clone(),
    }
}

#[test]
fn a_heavy_start_goes_to_an_always_on_machine_with_a_free_heavy_slot() {
    // A fixed limit leaves the slot free whatever memory the runner has.
    let machines = machines("machine.max_heavy_agents = 2\n");
    let placement = placed(&machines, &["--branch", "child"]);
    assert_eq!(
        placement["machine"],
        machines.server_id.as_str(),
        "{placement}"
    );
    assert_eq!(placement["reason"], "heavy_slot", "{placement}");
    assert_eq!(placement["policy"], "balanced", "{placement}");
    created_on_server(&machines, "child");
}

#[test]
fn a_start_whose_agent_would_run_in_the_users_terminal_goes_to_an_always_on_machine() {
    let machines = machines("machine.max_heavy_agents = 2\n");
    // As the user's terminal sends it, which then attaches to its agent.
    let launch = Launch {
        target: Some(StartTarget::Repository(
            machines.hub.repo.to_str().unwrap().into(),
        )),
        branch: Some("child".into()),
        agent: Some(Agent::unchecked("pi".into())),
        here: true,
        ..Launch::default()
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let launched = runtime.block_on(launches::launch(&app(&machines), launch));
    let launched = launched.unwrap();
    let placement = launched.placement.expect("the start was placed");
    assert_eq!(placement.machine.to_string(), machines.server_id);
    assert!(launched.machine.is_some(), "{:?}", launched.start);
    created_on_server(&machines, "child");
}

fn created_on_server(machines: &Machines, name: &str) {
    wait_for("the space on the server", || {
        let listed = machines.hub.ok(&["list"]);
        (listed.as_array().unwrap().iter())
            .find(|space| space["name"] == name && space["machine"] == machines.server_id.as_str())
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
fn without_an_answer_from_the_placer_balanced_places_the_start() {
    let machines = machines_with("placement = \"external\"\n", "");
    let unplaced = placed(&machines, &["--branch", "first"]);
    assert_eq!(unplaced["policy"], "balanced", "{unplaced}");
    assert_eq!(unplaced["unanswered"], "no_placer", "{unplaced}");
    // A placer that is asked but never answers is late, whatever the
    // runner's speed, and its answer comes too late to count.
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let app = app(&machines);
    let mut placing = runtime.block_on(Placing::serve(&app)).unwrap();
    // It keeps its registration, which a placer that hangs up would end.
    let asked = runtime.spawn(async move { (placing.next().await, placing) });
    let late = placed(&machines, &["--branch", "second"]);
    assert_eq!(late["policy"], "balanced", "{late}");
    assert_eq!(late["unanswered"], "late", "{late}");
    let (asked, _placing) = runtime.block_on(asked).unwrap();
    let asked = asked.unwrap();
    assert!(asked.snapshot.request.heavy);
    assert_eq!(asked.snapshot.machines.len(), 2);
    let refused = runtime.block_on(answer_placement(&app, asked.number, Placement::default()));
    assert!(refused.is_err(), "{refused:?}");
}
