//! Starts on another joined machine: `shoal start --machine` from a space
//! on one machine creates a child there, cloning the repository where that
//! machine has none of it.
use std::{fs, os::unix::fs::PermissionsExt as _, path::Path, process::Stdio};

use serde_json::Value;

use super::{
    directory::{give_origin, hub_machine, join, workspace},
    routing::{ready_at, scope_of},
    support::{
        self as support,
        fixture::{Base, home},
        git, git_with_identity, install_test_script, printed,
        prompt::{self, AnswerOptions},
        wait::wait_for,
        zmx,
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
    machines_with(("", &[]), "")
}

/// The [`machines`], with `hub` added to the hub's config and its files to
/// its repository, and `laptop` to the laptop's config.
fn machines_with((hub, files): (&str, &[(&str, &str)]), laptop: &str) -> Machines {
    let laptop = Base::start(home(), &format!("{AGENT}{laptop}"), "notes", &[]);
    machines_joining((hub, files), laptop)
}

/// The [`machines_with`] `hub` config and files, which `laptop` joins.
fn machines_joining((hub, files): (&str, &[(&str, &str)]), laptop: Base) -> Machines {
    let config = format!("[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n{hub}");
    let hub = Base::start(home(), &config, "app", files);
    let origin = give_origin(&hub);
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

/// The space named `name` of `kind` that the hub runs, as the laptop lists
/// it from the hub's directory, which names a parent on another machine
/// too, once it lists it.
fn on_hub(machines: &Machines, name: &str, kind: &str) -> Value {
    let hub = hub_machine(&machines.hub);
    let list = match kind {
        "chat" => &["list", "--chats"][..],
        _ => &["list"][..],
    };
    wait_for("the space on the hub", || {
        let listed = machines.laptop.ok(list);
        (listed.as_array().unwrap().iter())
            .find(|space| space["name"] == name && space["machine"] == hub.as_str())
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
fn a_start_on_another_machine_waits_for_the_preparation_there() {
    let machines = machines();
    let repo = machines.hub.repo.to_str().unwrap().to_owned();
    let started = start_in_coordinator(&machines, &[&repo, "--branch", "child", "--wait"]);
    assert!(started.status.success(), "{}", printed(&started));
    // It returns the workspace once that machine prepared it.
    let mut shown = serde_json::Deserializer::from_slice(&started.stdout).into_iter::<Value>();
    let workspace = shown.next().unwrap().unwrap();
    assert_eq!(workspace["state"], "ready", "{}", printed(&started));
}

#[test]
fn a_start_on_another_machine_here_attaches_this_terminal_to_its_agent_there() {
    let machines = machines();
    // The laptop's queue starts the agent in a session of its zmx.
    let laptop = machines.laptop.root.path();
    install_test_script(laptop.join("bin/zmx"), zmx::terminal_zmx());
    // Debug builds only open an existing ownership lock of the host.
    fs::write(laptop.join("zmx.shoal-ownership.lock"), "").unwrap();
    fs::write(laptop.join("zmx-keep-sessions"), "").unwrap();
    let repo = machines.hub.repo.to_str().unwrap().to_owned();
    let mut command = machines.hub.command();
    let args = ["start", "--machine", "laptop", "--agent", "pi", &repo];
    command.args(args).args(["--branch", "child", "--here"]);
    let options = AnswerOptions {
        controlling_terminal: true,
        kill_on_timeout: true,
        terminal_stdout: true,
    };
    let (output, transcript) = prompt::answer(&mut command, "", options);
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    // That machine attached its terminal for this one to the agent's session.
    let calls = fs::read_to_string(laptop.join("zmx-calls")).unwrap();
    assert!(calls.contains(r#"["attach", "#), "{calls}");
}

/// Claude Code as the script `$HOME/agent.sh`, which [`forking_agent`]
/// writes.
const FORKING_CLAUDE: &str =
    "[agents.claude]\ncommand = ['sh', '-c', 'exec sh \"$HOME/agent.sh\" \"$@\"', 'claude']\n";

/// A Claude Code that, started from a fork, records its arguments in
/// `$HOME/forked`, and otherwise keeps the transcript of session
/// `parent-session`, reports it as Claude Code's hooks do, and starts a
/// child on the laptop from a fork of it with `shoal`.
fn forking_agent(shoal: &str) -> String {
    format!(
        "if [ \"$1\" = --resume ]; then printf '%s\\0' \"$@\" > \"$HOME/forked.tmp\"; mv \"$HOME/forked.tmp\" \"$HOME/forked\"; exit 0; fi\n\
         mkdir -p \"$HOME/.claude/projects/p\"\n\
         printf '{{\"type\":\"user\"}}\\n' > \"$HOME/.claude/projects/p/parent-session.jsonl\"\n\
         printf '{{\"session_id\":\"parent-session\"}}' | '{shoal}' internal agent-state idle >/dev/null || exit 3\n\
         '{shoal}' --json start --machine laptop \"$(cat \"$HOME/repo-path\")\" --branch child --fork -- 'Take the child part' > \"$HOME/split.json\" 2> \"$HOME/split.err\"\n"
    )
}

#[test]
fn a_start_on_another_machine_forks_a_session_only_this_machine_keeps() {
    // The laptop puts the carried session where its Claude Code looks.
    let config = (
        &*format!("{AGENT}{FORKING_CLAUDE}"),
        &[("SHOAL_DEV_USER_FILES", "1")][..],
    );
    let laptop = Base::start_with_env(home(), config, "notes", &[]);
    let machines = machines_joining((FORKING_CLAUDE, &[]), laptop);
    let (hub, laptop) = (machines.hub.root.path(), machines.laptop.root.path());
    for home in [hub, laptop] {
        let agent = forking_agent(support::shoal_binary());
        fs::write(home.join("agent.sh"), agent).unwrap();
    }
    fs::write(hub.join("repo-path"), machines.hub.repo.to_str().unwrap()).unwrap();
    // The laptop's queue starts the forked agent in a session of its zmx.
    install_test_script(laptop.join("bin/zmx"), zmx::terminal_zmx());
    fs::write(laptop.join("zmx.shoal-ownership.lock"), "").unwrap();
    fs::write(laptop.join("zmx-keep-sessions"), "").unwrap();
    let mut command = machines.hub.command();
    let args = [
        "start",
        "--in",
        &machines.coordinator,
        "--here",
        "--agent",
        "claude",
    ];
    let output = machines.hub.output(command.args(args).stdin(Stdio::null()));
    assert!(output.status.success(), "{}", printed(&output));
    let forked = wait_for("the forked agent's start", || {
        fs::read_to_string(laptop.join("forked")).ok()
    });
    let split = || fs::read_to_string(hub.join("split.err")).unwrap_or_default();
    assert!(
        forked.contains("--resume\0parent-session\0--fork-session"),
        "{forked}{}",
        split()
    );
    let transcript = laptop.join(".claude/projects/p/parent-session.jsonl");
    let carried = fs::read_to_string(transcript).unwrap();
    assert_eq!(carried, "{\"type\":\"user\"}\n");
}

#[test]
fn a_start_on_another_machine_copies_a_workspace_s_unsaved_files_there() {
    let machines = machines();
    let status = machines.hub.ok(&["status", &machines.coordinator]);
    let source = Path::new(status["workspace"]["path"].as_str().unwrap()).to_owned();
    // A commit only this machine has, then a changed and a new file.
    fs::write(source.join("committed.txt"), "committed\n").unwrap();
    git(&source, &["add", "committed.txt"]);
    git_with_identity(&source, &["commit", "-q", "-m", "Local"]);
    let head = git(&source, &["rev-parse", "HEAD"]);
    fs::write(source.join("committed.txt"), "changed\n").unwrap();
    fs::write(source.join("untracked.txt"), "new\n").unwrap();
    let args = [
        "start",
        "--machine",
        "laptop",
        "--copy",
        &machines.coordinator,
    ];
    let rest = ["--agent", "pi", "--wait", "--json"];
    let started = machines.hub.run(&[&args[..], &rest[..]].concat());
    assert!(started.status.success(), "{}", printed(&started));
    let mut shown = serde_json::Deserializer::from_slice(&started.stdout).into_iter::<Value>();
    let copy = shown.next().unwrap().unwrap();
    assert_eq!(copy["state"], "ready", "{}", printed(&started));
    // The copy on the laptop starts at the same HEAD with the same files,
    // unsaved as they were.
    let path = Path::new(copy["path"].as_str().unwrap());
    assert_eq!(git(path, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(path.join("committed.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(
        fs::read_to_string(path.join("untracked.txt")).unwrap(),
        "new\n"
    );
    let unsaved = git(path, &["status", "--porcelain"]);
    assert!(unsaved.contains(" M committed.txt"), "{unsaved}");
    assert!(unsaved.contains("?? untracked.txt"), "{unsaved}");
    // A second copy there takes a branch of its own, with the files too.
    let again = machines.hub.run(&[&args[..], &rest[..]].concat());
    assert!(again.status.success(), "{}", printed(&again));
    let mut shown = serde_json::Deserializer::from_slice(&again.stdout).into_iter::<Value>();
    let second = shown.next().unwrap().unwrap();
    assert_ne!(second["branch"], copy["branch"], "{}", printed(&again));
    let path = Path::new(second["path"].as_str().unwrap());
    assert_eq!(git(path, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(path.join("untracked.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn a_space_copies_to_another_machine_only_what_it_copies_here() {
    let machines = machines();
    let token = scope_of(&machines.hub, &machines.coordinator);
    let mut create = machines.hub.command();
    create.env("SHOAL_SCOPE_TOKEN", &token);
    let created = machines
        .hub
        .output(create.args(["create", "--branch", "kid", "--json"]));
    assert!(created.status.success(), "{}", printed(&created));
    let kid = machines.hub.id("kid");
    machines.hub.ok(&["rm", &kid, "--yes", "--wait"]);
    // A space's process copies no removed workspace here, nor there.
    let started = start_in_coordinator(&machines, &["--copy", &kid, "--checkpoint", "1"]);
    assert!(!started.status.success(), "{}", printed(&started));
    let listed = machines.laptop.ok(&["list"]);
    let there = (listed.as_array().unwrap().iter()).any(|space| space["machine"].is_null());
    assert!(!there, "{listed}");
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
fn a_start_on_another_machine_takes_a_mode_only_that_machine_defines() {
    let machines = machines_with(("", &[]), "[modes.remote]\nkind = \"chat\"\n");
    let repo = machines.hub.repo.to_str().unwrap().to_owned();
    let started = start_in_coordinator(&machines, &[&repo, "--mode", "remote", "--name", "talk"]);
    assert!(started.status.success(), "{}", printed(&started));
    // That machine's mode starts a chat there.
    let chat = on_laptop(&machines, "talk", "chat");
    assert_eq!(chat["parent"], machines.coordinator.as_str(), "{chat}");
}

#[test]
fn a_first_start_on_another_machine_takes_a_mode_its_repository_defines() {
    // A mode only the hub's repository defines, which the laptop has not
    // cloned yet.
    let tracked = [(".shoal.toml", "[modes.remote]\nkind = \"chat\"\n")];
    let machines = machines_with(("", &tracked), "");
    let repo = machines.hub.repo.to_str().unwrap().to_owned();
    let started = start_in_coordinator(&machines, &[&repo, "--mode", "remote", "--name", "talk"]);
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
    // Its branch holds the carried commit, which then no ref of Shoal's
    // keeps.
    let path = std::path::Path::new(status["workspace"]["path"].as_str().unwrap());
    wait_for("the carried ref's removal", || {
        let carried = git(path, &["for-each-ref", "refs/shoal/carried/"]);
        carried.trim().is_empty().then_some(())
    });
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

#[test]
fn a_start_on_a_terminal_below_another_machine_s_space_takes_that_space_s_repository() {
    let machines = machines();
    // A repository only the laptop has, with an origin the hub never cloned.
    give_origin(&machines.laptop);
    let notes = machines.laptop.repo.to_str().unwrap().to_owned();
    let there = workspace_of(&machines.laptop, &notes, "there");
    ready_at(&machines.hub, &there, &machines.laptop_id);
    // On a terminal in the hub's own checkout, whose repository is another
    // one.
    let mut command = machines.hub.command();
    command.current_dir(&machines.hub.repo);
    let args = ["start", "--machine", "laptop", "--parent", &there];
    let rest = ["--mode", "chat", "--name", "talk", "--agent", "pi"];
    command.args([&args[..], &rest[..]].concat());
    let (started, transcript) = prompt::answer(&mut command, "", AnswerOptions::default());
    assert!(
        started.status.success(),
        "{}\n{transcript}",
        printed(&started)
    );
    let chat = on_laptop(&machines, "talk", "chat");
    assert_eq!(chat["parent"], there.as_str(), "{chat}");
}

#[test]
fn a_start_below_another_machine_s_space_clones_its_repository_where_it_starts() {
    // The hub starts the agent itself.
    let machines = machines_with((AGENT, &[]), "");
    // A repository only the laptop has, with an origin the hub never cloned.
    give_origin(&machines.laptop);
    let notes = machines.laptop.repo.to_str().unwrap().to_owned();
    let there = workspace_of(&machines.laptop, &notes, "there");
    ready_at(&machines.hub, &there, &machines.laptop_id);
    let args = ["start", "--parent", &there, "--branch", "child"];
    let started = (machines.hub).run(&[&args[..], &["--agent", "pi", "--json"]].concat());
    assert!(started.status.success(), "{}", printed(&started));
    let child = on_hub(&machines, "child", "workspace");
    assert_eq!(child["parent"], there.as_str(), "{child}");
    // The hub registered a clone of the laptop's repository for it.
    let repositories = machines.hub.ok(&["repo", "ls"]);
    let count = repositories.as_array().unwrap().len();
    assert_eq!(count, 2, "{repositories}");
}

#[test]
fn a_first_start_below_another_machine_s_space_takes_a_mode_its_repository_defines() {
    let machines = machines_with((AGENT, &[]), "");
    // A repository only the laptop has, whose tracked config defines a
    // mode no machine's own config does.
    let notes = &machines.laptop.repo;
    fs::write(notes.join(".shoal.toml"), "[modes.custom]\n").unwrap();
    git(notes, &["add", ".shoal.toml"]);
    git_with_identity(notes, &["commit", "-q", "-m", "Add a mode"]);
    give_origin(&machines.laptop);
    let there = workspace_of(&machines.laptop, notes.to_str().unwrap(), "there");
    ready_at(&machines.hub, &there, &machines.laptop_id);
    let args = [
        "start", "--parent", &there, "--mode", "custom", "--branch", "child",
    ];
    let started = (machines.hub).run(&[&args[..], &["--agent", "pi", "--json"]].concat());
    assert!(started.status.success(), "{}", printed(&started));
    let child = on_hub(&machines, "child", "workspace");
    assert_eq!(child["parent"], there.as_str(), "{child}");
}

#[test]
fn a_first_chat_below_another_machine_s_space_takes_an_agent_its_repository_defines() {
    // Neither machine's own config has the agent.
    let machines = machines_with(("", &[]), "");
    let notes = &machines.laptop.repo;
    let agent = "[agents.tracked]\ncommand = ['sh', '-c', 'exec sleep 600']\n";
    fs::write(notes.join(".shoal.toml"), agent).unwrap();
    git(notes, &["add", ".shoal.toml"]);
    git_with_identity(notes, &["commit", "-q", "-m", "Add an agent"]);
    give_origin(&machines.laptop);
    let there = workspace_of(&machines.laptop, notes.to_str().unwrap(), "there");
    ready_at(&machines.hub, &there, &machines.laptop_id);
    let args = [
        "start", "--parent", &there, "--mode", "chat", "--name", "talk",
    ];
    let rest = ["--agent", "tracked", "--json"];
    let started = (machines.hub).run(&[&args[..], &rest[..]].concat());
    assert!(started.status.success(), "{}", printed(&started));
    let chat = on_hub(&machines, "talk", "chat");
    assert_eq!(chat["parent"], there.as_str(), "{chat}");
}

#[test]
fn a_space_cannot_clone_the_repository_of_a_space_outside_its_scope() {
    let machines = machines_with((AGENT, &[]), "");
    give_origin(&machines.laptop);
    let notes = machines.laptop.repo.to_str().unwrap().to_owned();
    let there = workspace_of(&machines.laptop, &notes, "there");
    ready_at(&machines.hub, &there, &machines.laptop_id);
    // The coordinator's scope reaches no space of the laptop's.
    let token = scope_of(&machines.hub, &machines.coordinator);
    let mut command = machines.hub.command();
    command.env("SHOAL_SCOPE_TOKEN", &token);
    let args = [
        "start", "--parent", &there, "--branch", "child", "--agent", "pi", "--json",
    ];
    let started = machines.hub.output(command.args(args));
    assert!(!started.status.success(), "{}", printed(&started));
    let repositories = machines.hub.ok(&["repo", "ls"]);
    let count = repositories.as_array().unwrap().len();
    assert_eq!(count, 1, "{repositories}");
}

/// The ID of a ready workspace of the laptop's repository, which only the
/// laptop has, with an origin no machine cloned, and which tracks
/// `tracked` as its config when given.
fn laptop_space(machines: &Machines, tracked: Option<&str>) -> String {
    let notes = &machines.laptop.repo;
    if let Some(tracked) = tracked {
        fs::write(notes.join(".shoal.toml"), tracked).unwrap();
        git(notes, &["add", ".shoal.toml"]);
        git_with_identity(notes, &["commit", "-q", "-m", "Track a config"]);
    }
    give_origin(&machines.laptop);
    let there = workspace_of(&machines.laptop, notes.to_str().unwrap(), "there");
    ready_at(&machines.hub, &there, &machines.laptop_id);
    there
}

/// A `shoal start` with `args` on a terminal in the hub, whose fake fzf
/// picks `picked` and writes the rows it is given to the returned path.
fn start_on_terminal(
    machines: &Machines,
    args: &[&str],
    picked: &str,
) -> (std::process::Output, String, std::path::PathBuf) {
    let (bin, rows) = (
        machines.hub.root.path().join("bin"),
        machines.hub.root.path().join("rows"),
    );
    fake_fzf(&bin, &rows);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut command = machines.hub.command();
    command.env("PATH", path).env("SHOAL_TEST_PICK_ID", picked);
    command.arg("start").args(args);
    // A start from the user's terminal hands it to an agent it runs here.
    let options = AnswerOptions {
        controlling_terminal: true,
        ..AnswerOptions::default()
    };
    let (output, transcript) = prompt::answer(&mut command, "", options);
    (output, transcript, rows)
}

/// A fake fzf in `bin` that writes the rows it is given to `rows` and
/// picks the one whose ID or label is `$SHOAL_TEST_PICK_ID`.
fn fake_fzf(bin: &Path, rows: &Path) {
    fs::create_dir_all(bin).unwrap();
    let fzf = bin.join("fzf");
    let script = format!(
        "#!/bin/sh\ncat > '{}'\ncase \"$*\" in *--expect=*) printf '\\n';; esac\nawk -F '\\t' -v id=\"$SHOAL_TEST_PICK_ID\" '$1 == id || $2 == id {{ print }}' '{}'\n",
        rows.display(),
        rows.display()
    );
    fs::write(&fzf, script).unwrap();
    fs::set_permissions(&fzf, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_start_on_a_terminal_below_another_machine_s_space_picks_among_its_machine_s_modes() {
    let machines = machines_with(("", &[]), "[modes.remote]\n");
    let there = laptop_space(&machines, None);
    let args = [
        "--machine",
        "laptop",
        "--parent",
        &there,
        "--branch",
        "child",
        "--agent",
        "pi",
    ];
    let (started, transcript, rows) = start_on_terminal(&machines, &args, "remote");
    assert!(
        started.status.success(),
        "{}\n{transcript}",
        printed(&started)
    );
    // The picker listed the mode only the laptop defines.
    let listed = fs::read_to_string(&rows).unwrap_or_default();
    assert!(listed.contains("remote"), "{listed}");
    let child = on_laptop(&machines, "child", "workspace");
    assert_eq!(child["parent"], there.as_str(), "{child}");
}

#[test]
fn a_start_on_a_terminal_below_another_machine_s_space_refuses_its_arguments_before_a_picker() {
    let machines = machines_with(("", &[]), "[modes.remote]\n");
    let there = laptop_space(&machines, None);
    let args = [
        "--machine",
        "laptop",
        "--parent",
        &there,
        "--branch",
        "bad..name",
    ];
    let (started, transcript, rows) = start_on_terminal(
        &machines,
        &[&args[..], &["--agent", "pi"]].concat(),
        "remote",
    );
    assert!(
        !started.status.success(),
        "{}\n{transcript}",
        printed(&started)
    );
    // The branch was refused before the mode picker opened.
    assert!(!rows.exists(), "{transcript}");
}

#[test]
fn a_start_on_a_terminal_below_another_machine_s_space_refuses_an_unknown_agent_before_a_picker() {
    // The hub has two modes of a workspace, so a picker would open.
    let machines = machines_with(("[modes.other]\n", &[]), "");
    let there = laptop_space(&machines, None);
    let args = [
        "--parent", &there, "--branch", "child", "--agent", "missing",
    ];
    let (started, transcript, rows) = start_on_terminal(&machines, &args, "other");
    assert!(
        !started.status.success(),
        "{}\n{transcript}",
        printed(&started)
    );
    assert!(!rows.exists(), "{transcript}");
}

#[test]
fn a_start_on_a_terminal_below_another_machine_s_space_picks_its_agent() {
    // Two agents and no default, so the terminal picks one, which runs in
    // it and exits at once.
    let agents = "[agents.quick]\ncommand = ['true']\n[agents.other]\ncommand = ['true']\n";
    let machines = machines_with((agents, &[]), "");
    let there = laptop_space(&machines, None);
    let args = ["--parent", &there, "--branch", "child"];
    let (started, transcript, rows) = start_on_terminal(&machines, &args, "quick");
    assert!(
        started.status.success(),
        "{}\n{transcript}",
        printed(&started)
    );
    let listed = fs::read_to_string(&rows).unwrap_or_default();
    assert!(listed.contains("other"), "{listed}");
    let child = on_hub(&machines, "child", "workspace");
    assert_eq!(child["parent"], there.as_str(), "{child}");
}

#[test]
fn a_start_on_a_terminal_below_another_machine_s_space_refuses_an_unknown_blocker_before_a_picker()
{
    // The hub has two modes of a workspace, so a picker would open.
    let machines = machines_with((&format!("{AGENT}[modes.other]\n"), &[]), "");
    let there = laptop_space(&machines, None);
    let args = ["--parent", &there, "--branch", "child", "--agent", "pi"];
    let blocker = ["--add-blocker", "00000000-0000-0000-0000-000000000000"];
    let (started, transcript, rows) =
        start_on_terminal(&machines, &[&args[..], &blocker[..]].concat(), "other");
    assert!(
        !started.status.success(),
        "{}\n{transcript}",
        printed(&started)
    );
    assert!(!rows.exists(), "{transcript}");
}

#[test]
fn a_start_below_another_machine_s_space_with_an_unknown_agent_clones_nothing() {
    let machines = machines_with((AGENT, &[]), "");
    let there = laptop_space(&machines, None);
    let args = [
        "start", "--parent", &there, "--branch", "child", "--agent", "missing", "--json",
    ];
    let started = machines.hub.run(&args);
    assert!(!started.status.success(), "{}", printed(&started));
    let repositories = machines.hub.ok(&["repo", "ls"]);
    let count = repositories.as_array().unwrap().len();
    assert_eq!(count, 1, "{repositories}");
}

#[test]
fn a_start_below_another_machine_s_space_with_arguments_its_mode_refuses_clones_nothing() {
    let machines = machines_with((AGENT, &[]), "");
    let there = laptop_space(&machines, None);
    // A chat takes no branch of its own.
    let args = [
        "start", "--parent", &there, "--mode", "chat", "--branch", "child",
    ];
    let started = (machines.hub).run(&[&args[..], &["--agent", "pi", "--json"]].concat());
    assert!(!started.status.success(), "{}", printed(&started));
    let repositories = machines.hub.ok(&["repo", "ls"]);
    let count = repositories.as_array().unwrap().len();
    assert_eq!(count, 1, "{repositories}");
}
