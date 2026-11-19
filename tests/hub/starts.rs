//! Starts on another joined machine: `shoal start --machine` from a space
//! on one machine creates a child there, cloning the repository where that
//! machine has none of it.
use serde_json::Value;

use super::{
    directory::{give_origin, hub_machine, join, workspace},
    routing::{ready_at, scope_of},
    support::{
        fixture::{Base, home},
        git, git_with_identity, printed,
        wait::wait_for,
    },
};

/// An agent that runs until its space stops it.
const AGENT: &str = "[agents.pi]\ncommand = ['sh', '-c', 'exec sleep 600']\n";

/// The hub, whose repository has an origin, with a coordinator workspace
/// there, and a laptop joined to it that has another repository, by ID.
struct Machines {
    hub: Base,
    laptop: Base,
    laptop_id: String,
    coordinator: String,
}

fn machines() -> Machines {
    let config = format!("[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n{AGENT}");
    let hub = Base::start(home(), &config, "app", &[]);
    give_origin(&hub);
    let laptop = Base::start(home(), AGENT, "notes", &[]);
    let laptop_id = join(&laptop, &hub, "laptop");
    let coordinator = workspace(&hub, "coordinator");
    ready_at(&laptop, &coordinator, &hub_machine(&hub));
    Machines {
        hub,
        laptop,
        laptop_id,
        coordinator,
    }
}

/// Run `shoal start` with `args` in the hub's coordinator, under its scope.
fn start_in_coordinator(machines: &Machines, args: &[&str]) -> std::process::Output {
    let token = scope_of(&machines.hub, &machines.coordinator);
    let mut command = machines.hub.command();
    command.env("SHOAL_SCOPE_TOKEN", &token);
    command.args(["start", "--machine", "laptop", "--agent", "pi", "--json"]);
    machines.hub.output(command.args(args))
}

/// The space named `name` the hub lists on the laptop among its spaces of
/// `kind`, once it lists it.
fn on_laptop(machines: &Machines, name: &str, kind: &str) -> Value {
    let list = match kind {
        "chat" => &["list", "--chats"][..],
        _ => &["list"][..],
    };
    wait_for("the space on the laptop", || {
        let listed = machines.hub.ok(list);
        (listed.as_array().unwrap().iter())
            .find(|space| space["name"] == name && space["machine"] == machines.laptop_id.as_str())
            .cloned()
    })
}

#[test]
fn a_space_starts_a_child_workspace_on_another_machine_that_clones_its_repository() {
    let machines = machines();
    let repo = machines.hub.repo.to_str().unwrap().to_owned();
    let started = start_in_coordinator(&machines, &[&repo, "--branch", "child"]);
    assert!(started.status.success(), "{}", printed(&started));
    let child = on_laptop(&machines, "child", "workspace");
    assert_eq!(child["parent"], machines.coordinator.as_str(), "{child}");
    // The laptop registered a clone of the hub's repository for it.
    let repositories = machines.laptop.ok(&["repo", "ls"]);
    let count = repositories.as_array().unwrap().len();
    assert_eq!(count, 2, "{repositories}");
}

#[test]
fn a_space_starts_a_child_chat_on_another_machine() {
    let machines = machines();
    let repo = machines.hub.repo.to_str().unwrap().to_owned();
    let started = start_in_coordinator(&machines, &[&repo, "--mode", "chat", "--name", "talk"]);
    assert!(started.status.success(), "{}", printed(&started));
    let chat = on_laptop(&machines, "talk", "chat");
    assert_eq!(chat["parent"], machines.coordinator.as_str(), "{chat}");
}

#[test]
fn a_start_on_another_machine_refuses_a_base_only_this_machine_has() {
    let machines = machines();
    let repo = machines.hub.repo.to_str().unwrap().to_owned();
    // A commit past `main` that only the hub has.
    let tree = ["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "local"];
    let commit = git_with_identity(&machines.hub.repo, &tree);
    git(&machines.hub.repo, &["branch", "unpushed", commit.trim()]);
    let started = start_in_coordinator(&machines, &[&repo, "--branch", "x", "--base", "unpushed"]);
    assert!(!started.status.success(), "{}", printed(&started));
    let error = String::from_utf8_lossy(&started.stderr);
    assert!(error.contains("git push"), "{error}");
}
