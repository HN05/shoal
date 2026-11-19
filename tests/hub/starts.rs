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
/// there, and a laptop joined to it that has another repository and the
/// agent the hub lacks, by ID.
struct Machines {
    hub: Base,
    /// The URL of the hub's repository's origin.
    origin: String,
    laptop: Base,
    laptop_id: String,
    coordinator: String,
}

fn machines() -> Machines {
    // Only the laptop has the agent, which a start there checks.
    let config = "[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n";
    let hub = Base::start(home(), config, "app", &[]);
    let origin = give_origin(&hub);
    let laptop = Base::start(home(), AGENT, "notes", &[]);
    let laptop_id = join(&laptop, &hub, "laptop");
    let coordinator = workspace(&hub, "coordinator");
    ready_at(&laptop, &coordinator, &hub_machine(&hub));
    Machines {
        hub,
        origin,
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

/// Start a workspace of `machine`'s repository `repository` on the new
/// branch `name` and return its ID.
fn workspace_of(machine: &Base, repository: &str, name: &str) -> String {
    machine.ok(&["create", repository, "--branch", name]);
    machine.id(name)
}

/// The laptop's status of its workspace `id` once its preparation ended.
fn prepared_on_laptop(machines: &Machines, id: &str) -> Value {
    wait_for("the child's preparation", || {
        let status = machines.laptop.ok(&["status", id]);
        (status["workspace"]["state"] != "preparing").then_some(status)
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
fn a_start_on_another_machine_carries_a_base_only_this_machine_has() {
    let machines = machines();
    let repo = machines.hub.repo.to_str().unwrap().to_owned();
    // A commit past `main` that only the hub has.
    let tree = ["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "local"];
    let commit = git_with_identity(&machines.hub.repo, &tree);
    let commit = commit.trim();
    git(&machines.hub.repo, &["branch", "unpushed", commit]);
    let started = start_in_coordinator(&machines, &[&repo, "--branch", "x", "--base", "unpushed"]);
    assert!(started.status.success(), "{}", printed(&started));
    let child = on_laptop(&machines, "x", "workspace");
    let status = prepared_on_laptop(&machines, child["id"].as_str().unwrap());
    assert_eq!(status["workspace"]["state"], "ready", "{status}");
    let base = &status["workspace"]["checkout"]["base"]["commit"];
    assert_eq!(base, commit, "{status}");
    // Its branch holds the carried commit, which no ref of Shoal's keeps.
    let path = std::path::Path::new(status["workspace"]["path"].as_str().unwrap());
    let carried = git(path, &["for-each-ref", "refs/shoal/carried/"]);
    assert!(carried.trim().is_empty(), "{carried}");
}

#[test]
fn a_start_on_another_machine_refuses_a_repository_this_machine_has_not_registered() {
    let machines = machines();
    let started = start_in_coordinator(&machines, &["--repo", "typo", "--branch", "x"]);
    assert!(!started.status.success(), "{}", printed(&started));
    let listed = machines.laptop.ok(&["list"]);
    let started = (listed.as_array().unwrap().iter()).any(|space| space["name"] == "x");
    assert!(!started, "{listed}");
}

#[test]
fn a_start_on_another_machine_bases_on_a_pushed_commit_its_clone_never_fetched() {
    let machines = machines();
    // A clone that fetches `main` alone.
    let clone = machines.laptop.root.path().join("single");
    let single = ["clone", "-q", "--single-branch", &machines.origin];
    git(
        machines.laptop.root.path(),
        &[&single[..], &[clone.to_str().unwrap()]].concat(),
    );
    machines
        .laptop
        .ok(&["repo", "add", clone.to_str().unwrap()]);
    // A commit the origin gets on another branch after the laptop cloned
    // it.
    let tree = ["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "later"];
    let commit = git_with_identity(&machines.hub.repo, &tree);
    let commit = commit.trim();
    git(
        &machines.hub.repo,
        &[
            "push",
            "-q",
            "origin",
            &format!("{commit}:refs/heads/later"),
        ],
    );
    git(&machines.hub.repo, &["fetch", "-q", "origin"]);
    let repo = machines.hub.repo.to_str().unwrap().to_owned();
    let started = start_in_coordinator(&machines, &[&repo, "--branch", "x", "--base", commit]);
    assert!(started.status.success(), "{}", printed(&started));
    let child = on_laptop(&machines, "x", "workspace");
    // Its worktree starts from the commit, which the laptop fetched first.
    let status = prepared_on_laptop(&machines, child["id"].as_str().unwrap());
    assert_eq!(status["workspace"]["state"], "ready", "{status}");
}

#[test]
fn the_user_starts_a_space_below_another_machine_s_space_there() {
    let machines = machines();
    machines.laptop.ok(&["repo", "add", &machines.origin]);
    let there = workspace_of(&machines.laptop, "origin", "there");
    ready_at(&machines.hub, &there, &machines.laptop_id);
    let repo = machines.hub.repo.to_str().unwrap().to_owned();
    let args = [
        "start",
        &repo,
        "--machine",
        "laptop",
        "--parent",
        &there,
        "--branch",
        "child",
    ];
    let started = machines
        .hub
        .run(&[&args[..], &["--agent", "pi", "--json"]].concat());
    assert!(started.status.success(), "{}", printed(&started));
    let child = on_laptop(&machines, "child", "workspace");
    assert_eq!(child["parent"], there.as_str(), "{child}");
}

#[test]
fn a_start_below_another_machine_s_space_leaves_a_repository_it_lacks_to_that_machine() {
    let machines = machines();
    // A repository only the laptop has, with an origin the hub never cloned.
    give_origin(&machines.laptop);
    let notes = machines.laptop.repo.to_str().unwrap().to_owned();
    let there = workspace_of(&machines.laptop, &notes, "there");
    ready_at(&machines.hub, &there, &machines.laptop_id);
    // From the hub's own checkout, whose repository is another one.
    let args = ["start", "--machine", "laptop", "--parent", &there];
    let mut command = machines.hub.command();
    command.current_dir(&machines.hub.repo);
    let rest = ["--branch", "child", "--agent", "pi", "--json"];
    let started = machines
        .hub
        .output(command.args([&args[..], &rest[..]].concat()));
    assert!(started.status.success(), "{}", printed(&started));
    let child = on_laptop(&machines, "child", "workspace");
    assert_eq!(child["parent"], there.as_str(), "{child}");
}

#[test]
fn a_space_carries_nothing_for_a_start_in_another_repository_than_its_own() {
    let machines = machines();
    // A second repository of the hub, with an origin and a commit only the
    // hub has.
    let root = machines.hub.root.path();
    let other = root.join("other");
    git(root, &["init", "-q", "-b", "main", other.to_str().unwrap()]);
    git_with_identity(&other, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(root, &["init", "-q", "--bare", "-b", "main", "other.git"]);
    let origin = format!("file://{}", root.join("other.git").display());
    git(&other, &["remote", "add", "origin", &origin]);
    git(&other, &["push", "-q", "-u", "origin", "main"]);
    git(&other, &["remote", "set-head", "origin", "main"]);
    let tree = ["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "local"];
    let commit = git_with_identity(&other, &tree);
    git(&other, &["branch", "unpushed", commit.trim()]);
    machines.hub.ok(&["repo", "add", other.to_str().unwrap()]);
    let repo = other.to_str().unwrap();
    let args = ["--repo", repo, "--branch", "x", "--base", "unpushed"];
    let started = start_in_coordinator(&machines, &args);
    assert!(!started.status.success(), "{}", printed(&started));
    // The laptop registered nothing of it.
    let repositories = machines.laptop.ok(&["repo", "ls"]);
    assert_eq!(repositories.as_array().unwrap().len(), 1, "{repositories}");
}
