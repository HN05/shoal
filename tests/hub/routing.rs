//! Requests for a space another joined machine owns, which the caller's
//! daemon forwards there through the hub, and the events a space passes up
//! to a coordinator on another machine.
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    directory::{hub_machine, join, listed, machine, reach, workspace},
    numbers::reroute,
    support::{
        daemon_client,
        fixture::{Base, home},
        git, printed, pty, shoal_binary,
        wait::{try_wait_for, wait_for},
    },
};

/// What `machine`'s daemon answers the request `method` with, as the user.
fn request(machine: &Base, method: Value) -> Value {
    let root = machine.root.path();
    let protocol = daemon_client::protocol(root);
    let request = json!({"protocol": protocol, "id": 1, "method": method});
    daemon_client::exchange(root, &request)
}

/// What a process of `machine`'s space `id` prints for the Shoal command
/// `args`, which must succeed.
fn ok_in(machine: &Base, id: &str, args: &[&str]) -> Value {
    let mut command = machine.command();
    command.args(["run", "--workspace", id, "--", shoal_binary()]);
    machine.ok_from(&mut command, args)
}

/// Have the agent of `machine`'s space `id` ask "Which database?" in a
/// thread with its coordinator; the thread's number there.
fn ask(machine: &Base, id: &str) -> String {
    let opened = ok_in(machine, id, &["thread", "open", "Which database?"]);
    let number = opened["number"].as_str().unwrap().to_owned();
    ok_in(machine, id, &["send", &number, "Postgres or SQLite?"]);
    number
}

/// Report that the agent of `machine`'s space `id` works, so the space
/// counts as one with an agent.
fn working(machine: &Base, id: &str) {
    let report = json!({"set_agent_state": {"workspace": id, "state": "working"}});
    let answer = request(machine, report);
    assert_ne!(answer["type"], "error", "{answer}");
}

/// A coordinator workspace on the hub with an agent, and a child below it
/// that moved to the laptop, by ID, with the laptop's.
pub(super) struct Tree {
    pub(super) hub: Base,
    pub(super) laptop: Base,
    pub(super) laptop_id: String,
    pub(super) coordinator: String,
    pub(super) child: String,
}

pub(super) fn tree_across_machines() -> Tree {
    tree_below_hub_config("")
}

/// [`tree_across_machines`] with the hub's global config `extra` besides
/// its role.
fn tree_below_hub_config(extra: &str) -> Tree {
    let tree = tree_without_coordinator(extra);
    working(&tree.hub, &tree.coordinator);
    tree
}

/// [`tree_below_hub_config`] whose space on the hub has no agent, so what
/// the child passes up reaches the user there.
fn tree_without_coordinator(extra: &str) -> Tree {
    let config = format!("[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n{extra}");
    let hub = Base::start(home(), &config, "app", &[]);
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    reach(&hub, &laptop);
    let coordinator = workspace(&hub, "coordinator");
    ready_at(&laptop, &coordinator, &hub_machine(&hub));
    let repo = hub.repo.to_str().unwrap().to_owned();
    hub.ok(&[
        "create",
        &repo,
        "--branch",
        "child",
        "--parent",
        &coordinator,
    ]);
    let child = hub.id("child");
    let moved = hub.ok(&["move", "--ssh", "laptop", &child, "--shoal", shoal_binary()]);
    assert_eq!(moved["failed"], json!([]), "{moved}");
    // The laptop reports the child below the hub's coordinator.
    let below = try_wait_for(|| {
        (listed(&hub, &child, &laptop_id)["parent"] == coordinator.as_str()).then_some(())
    });
    assert!(
        below.is_some(),
        "the hub never listed the child below its coordinator: {moved}"
    );
    Tree {
        hub,
        laptop,
        laptop_id,
        coordinator,
        child,
    }
}

/// Wait until `machine` lists the space `id` of the machine `of` as past
/// its preparation, as a parent the hub lists must be to take a child.
pub(super) fn ready_at(machine: &Base, id: &str, of: &str) {
    wait_for("the space ready at the hub", || {
        (listed(machine, id, of)["status"] != "preparing").then_some(())
    });
}

/// The scope token of a process of `machine`'s space `id`.
pub(super) fn scope_of(machine: &Base, id: &str) -> String {
    let token = request(machine, json!({"issue_scope": {"space": id}}));
    token["data"]
        .as_str()
        .unwrap_or_else(|| panic!("{token}"))
        .to_owned()
}

/// The messages waiting for the agent of `machine`'s space `id`, once one
/// holds `text`.
pub(super) fn message_with(machine: &Base, id: &str, text: &str) -> Value {
    wait_for("the agent's message", || {
        let to = json!({"space": {"space": id}});
        let messages = request(
            machine,
            json!({"list_alerts": {"to": to, "unread_only": true, "limit": 50}}),
        );
        let messages = messages["data"].as_array().cloned().unwrap_or_default();
        (messages.into_iter()).find(|message| {
            message["message"]
                .as_str()
                .is_some_and(|m| m.contains(text))
        })
    })
}

/// The hub and a laptop joined to it, with a workspace on the laptop that
/// the hub lists, by ID.
fn laptop_space() -> (Base, Base, String) {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    let there = workspace(&laptop, "on-laptop");
    listed(&hub, &there, &laptop_id);
    (hub, laptop, there)
}

#[test]
fn the_user_s_terminal_renames_and_removes_a_space_on_another_machine() {
    let (hub, laptop, there) = laptop_space();
    hub.ok(&["rename", &there, "Renamed there"]);
    let status = hub.ok(&["status", &there]);
    assert_eq!(status["workspace"]["name"], "Renamed there", "{status}");
    assert_eq!(status["machine"]["name"], "laptop", "{status}");
    let stopped = hub.ok(&["stop", &there]);
    assert_eq!(stopped["stopped"], true, "{stopped}");
    hub.ok(&["rm", &there, "--wait", "--yes"]);
    let gone = laptop.run(&["status", &there, "--json"]);
    assert!(!gone.status.success(), "{}", printed(&gone));
}

#[test]
fn asking_another_machine_s_space_to_rename_itself_finds_it_there() {
    let (hub, _laptop, there) = laptop_space();
    // No agent runs there to ask, which only its machine knows.
    let output = hub.run(&["rename", "--request", &there]);
    assert!(!output.status.success(), "{}", printed(&output));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("on-laptop has no running agent"), "{error}");
}

#[test]
fn a_restart_and_its_cancel_go_to_the_machine_of_the_space() {
    let (hub, _laptop, there) = laptop_space();
    // The laptop decides the restart: no agent runs in its space.
    let restart = request(
        &hub,
        json!({"restart_agent": {"space": there, "restart": {}}}),
    );
    assert_eq!(restart["data"]["outcome"], "skipped", "{restart}");
    let cancel = request(&hub, json!({"cancel_restart": {"space": there}}));
    assert_eq!(cancel["type"], "cancelled_restart", "{cancel}");
}

#[test]
fn a_selection_by_machine_takes_another_machine_s_spaces_once_through_the_daemon() {
    let (hub, _laptop, there) = laptop_space();
    // Named twice, the laptop's space is selected once.
    let method =
        json!({"select_spaces": {"spaces": [], "machines": ["laptop", "laptop"], "below": false}});
    let selected = request(&hub, method);
    let spaces = selected["data"]
        .as_array()
        .unwrap_or_else(|| panic!("{selected}"));
    let ids: Vec<&str> = (spaces.iter())
        .filter_map(|located| located["elsewhere"]["space"]["id"].as_str())
        .collect();
    assert_eq!(ids, [there.as_str()], "{selected}");
    let refused = hub.run(&["restart", "--machine", "nowhere"]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("nowhere"), "{error}");
}

#[test]
fn a_restart_names_the_machine_of_the_space_and_fails_at_once_while_it_is_offline() {
    let (hub, laptop, there) = laptop_space();
    let restarted = hub.ok(&["restart", &there]);
    let [skipped] = restarted["skipped"].as_array().unwrap().as_slice() else {
        panic!("expected the laptop to skip its space: {restarted}");
    };
    assert_eq!(skipped["machine"], "laptop", "{restarted}");
    reroute(&laptop, Some("false"));
    wait_for("the hub to see the laptop go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|member| member["name"] == "laptop");
        (laptop?["connected"] == false).then_some(())
    });
    let refused = hub.run(&["restart", &there]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    let shown = printed(&refused).to_string();
    assert!(shown.contains("machine laptop is offline"), "{shown}");
}

#[test]
fn an_offline_machine_s_last_report_reaches_only_a_caller_whose_scope_reaches_it() {
    let (hub, laptop, there) = laptop_space();
    let here = workspace(&hub, "here");
    reroute(&laptop, Some("false"));
    wait_for("the hub to see the laptop go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|member| member["name"] == "laptop");
        (laptop?["connected"] == false).then_some(())
    });
    let reported = hub.ok(&["status", &there]);
    assert_eq!(reported["machine"]["name"], "laptop", "{reported}");
    // A space's process outside it reads none of it.
    let mut command = hub.command();
    command.args([
        "run",
        "--workspace",
        &here,
        "--",
        shoal_binary(),
        "status",
        &there,
    ]);
    let refused = command.output().unwrap();
    assert!(!refused.status.success(), "{}", printed(&refused));
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("outside this space's scope"), "{error}");
}

#[test]
fn attaching_a_file_to_a_space_on_another_machine_names_that_machine() {
    let (hub, _laptop, there) = laptop_space();
    // Attaching reads the file where the command runs.
    let here = hub.root.path().join("here.md");
    std::fs::write(&here, "kept on the hub").unwrap();
    let output = hub.run(&["files", "add", here.to_str().unwrap(), "--space", &there]);
    assert!(!output.status.success(), "{}", printed(&output));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("laptop"), "{error}");
}

#[test]
fn the_diff_of_a_space_on_another_machine_takes_the_caller_s_colours() {
    let (hub, laptop, there) = laptop_space();
    let status = laptop.ok(&["status", &there]);
    let path = PathBuf::from(status["workspace"]["path"].as_str().unwrap());
    // Longer than a frame of the stream carries.
    let lines: String = (0..4000)
        .map(|line| format!("from the laptop {line}\n"))
        .collect();
    std::fs::write(path.join("added.txt"), &lines).unwrap();
    git(&path, &["add", "added.txt"]);
    let plain = hub.run(&["diff", &there]);
    assert!(plain.status.success(), "{}", printed(&plain));
    let text = String::from_utf8_lossy(&plain.stdout);
    let added = text
        .lines()
        .filter(|line| line.starts_with("+from the laptop "));
    assert_eq!(added.count(), 4000, "{}", printed(&plain));
    assert!(!text.contains('\x1b'), "{text}");
    let mut coloured = hub.command();
    coloured
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "color.diff")
        .env("GIT_CONFIG_VALUE_0", "always")
        .args(["diff", &there]);
    let coloured = hub.output(&mut coloured);
    assert!(coloured.status.success(), "{}", printed(&coloured));
    let text = String::from_utf8_lossy(&coloured.stdout);
    assert!(text.contains("\x1b["), "{text}");
}

#[test]
fn the_diff_of_a_space_on_another_machine_goes_to_the_caller_s_pager_on_a_terminal() {
    let (hub, laptop, there) = laptop_space();
    let status = laptop.ok(&["status", &there]);
    let path = PathBuf::from(status["workspace"]["path"].as_str().unwrap());
    std::fs::write(path.join("added.txt"), "from the laptop\n").unwrap();
    git(&path, &["add", "added.txt"]);
    let paged = hub.root.path().join("paged");
    let pager = format!(
        "printf '%s\\n' \"$LESS\" > {0}; cat >> {0}",
        paged.display()
    );
    let (_master, slave) = pty::open();
    // On a terminal of its own, which `Base::output` would replace.
    // The diff's own pager, which `pager.diff` names.
    let mut diff = (hub.command())
        .env_remove("GIT_PAGER")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "pager.diff")
        .env("GIT_CONFIG_VALUE_0", &pager)
        .env_remove("LESS")
        .stdin(std::process::Stdio::null())
        .stdout(slave)
        .args(["diff", &there])
        .spawn()
        .unwrap();
    let status = wait_for("the diff to end", || diff.try_wait().unwrap());
    assert!(status.success(), "{status}");
    let paged = std::fs::read_to_string(&paged).unwrap();
    assert!(paged.starts_with("FRX\n"), "{paged}");
    assert!(paged.contains("from the laptop"), "{paged}");
}

#[test]
fn the_user_s_terminal_checks_conflicts_of_a_space_on_another_machine() {
    let (hub, _laptop, there) = laptop_space();
    let check = hub.ok(&["conflicts", "--workspace", &there]);
    assert_eq!(check["files"], json!([]), "{check}");
}

#[test]
fn a_coordinator_messages_its_child_on_another_machine_and_nothing_outside_its_scope() {
    let Tree {
        hub,
        laptop,
        laptop_id,
        coordinator,
        child,
    } = tree_across_machines();
    let other = workspace(&laptop, "unrelated");
    listed(&hub, &other, &laptop_id);
    let token = scope_of(&hub, &coordinator);
    let scoped = |args: &[&str]| {
        let mut command = hub.command();
        command.env("SHOAL_SCOPE_TOKEN", &token).args(args);
        command.output().unwrap()
    };
    let sent = scoped(&["send", "--space", &child, "from the coordinator"]);
    assert!(sent.status.success(), "{}", printed(&sent));
    message_with(&laptop, &child, "from the coordinator");
    let refused = scoped(&["send", "--space", &other, "not yours"]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("outside this space's scope"), "{error}");
}

#[test]
fn a_process_acting_as_its_space_sends_and_withdraws_an_alert_on_another_machine() {
    let (hub, laptop, there) = laptop_space();
    // A chat's agent without a scope token names its chat, which only its
    // own machine has.
    let sender = workspace(&hub, "sender");
    listed(&laptop, &sender, &hub_machine(&hub));
    let to = json!({"space": {"space": there}});
    let sent = request(
        &hub,
        json!({"send_alert": {"to": to, "from": sender, "message": "from the hub"}}),
    );
    let id = sent["data"]["alerts"][0]
        .as_i64()
        .unwrap_or_else(|| panic!("{sent}"));
    let alert = message_with(&laptop, &there, "from the hub");
    assert_eq!(
        (&alert["sender"], &alert["space_id"]),
        (&json!("space"), &json!(sender)),
        "{alert}"
    );
    let withdrawn = request(
        &hub,
        json!({"withdraw_alert": {"id": id, "space": there, "from": sender}}),
    );
    assert_eq!(withdrawn["data"]["id"], id, "{withdrawn}");
}

/// Have the agent of the tree's child send a chart up to its coordinator;
/// the chart's text.
fn send_chart(tree: &Tree) -> &'static str {
    let chart = tree.laptop.root.path().join("chart.txt");
    std::fs::write(&chart, "latency by region").unwrap();
    working(&tree.laptop, &tree.child);
    let attach = chart.to_str().unwrap();
    ok_in(
        &tree.laptop,
        &tree.child,
        &["send", "--attach", attach, "See the chart"],
    );
    "latency by region"
}

/// Check that `file`, a record of the chart the tree's child sent, names
/// the laptop and the child, and that `machine` reads the chart's text
/// `text` through it.
fn reads_chart(tree: &Tree, machine: &Base, file: &Value, text: &str) {
    assert_eq!(
        (&file["name"], &file["machine"], &file["space"]),
        (&json!("chart.txt"), &json!("laptop"), &json!(tree.child)),
        "{file}"
    );
    assert!(file.get("path").is_none(), "{file}");
    let space = shoal_protocol::space_names::SpaceSelector::id(&tree.child.as_str().into());
    let id = file["id"].as_i64().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let app = super::following::app(machine);
    let bytes = runtime.block_on(async {
        let read = shoal_client::files::read(&app, space, id).await.unwrap();
        read.to_end().await.unwrap()
    });
    assert_eq!(bytes, text.as_bytes());
}

#[test]
fn a_child_s_files_reach_the_user_on_another_machine_as_records_to_read() {
    let tree = tree_without_coordinator("");
    let text = send_chart(&tree);
    let alert = wait_for("the alert at the hub", || {
        let alerts = tree.hub.ok(&["alert", "list", "--all"]);
        (alerts.as_array().unwrap().iter())
            .find(|alert| alert["message"] == "See the chart")
            .cloned()
    });
    reads_chart(&tree, &tree.hub, &alert["files"][0], text);
}

#[test]
fn a_child_s_files_reach_its_coordinator_on_another_machine_as_records_to_read() {
    let tree = tree_across_machines();
    let text = send_chart(&tree);
    let alert = message_with(&tree.hub, &tree.coordinator, "See the chart");
    reads_chart(&tree, &tree.hub, &alert["files"][0], text);
}

#[test]
fn a_child_s_thread_reaches_its_coordinator_on_another_machine() {
    let Tree {
        hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    working(&laptop, &child);
    let number = ask(&laptop, &child);
    assert_eq!(
        number,
        format!("{}.1", listed_number(&hub, &child)),
        "the hub names the thread as its machine does"
    );
    // The agent's text, as an agent on the child's machine reads it, not
    // the user's one line.
    let asked = message_with(&hub, &coordinator, "Which database?");
    let text = asked["message"].as_str().unwrap();
    assert!(text.starts_with(&format!("Thread {number}")), "{asked}");
    assert!(text.contains('\n'), "{asked}");
    // The coordinator replies from its own machine, which has no record of
    // the child's space.
    ok_in(&hub, &coordinator, &["send", &number, "SQLite"]);
    message_with(&laptop, &child, "SQLite");
}

#[test]
fn a_coordinator_opens_a_thread_with_its_child_on_another_machine() {
    let Tree {
        hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    working(&laptop, &child);
    let open = ["thread", "open", "Which database?", "--space", &child];
    let number = ok_in(&hub, &coordinator, &open)["number"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        number,
        format!("{}.1", listed_number(&hub, &coordinator)),
        "the thread is numbered in its opener's space"
    );
    ok_in(
        &hub,
        &coordinator,
        &["send", &number, "Postgres or SQLite?"],
    );
    message_with(&laptop, &child, "Postgres or SQLite?");
    // The child replies from its own machine, which has no record of the
    // thread.
    ok_in(&laptop, &child, &["send", &number, "SQLite"]);
    message_with(&hub, &coordinator, "SQLite");
}

#[test]
fn a_coordinator_reviews_its_child_on_another_machine() {
    let Tree {
        hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    working(&laptop, &child);
    ok_in(&hub, &coordinator, &["review", "start", &child]);
    let open = ["thread", "open", "Unclear name", "--space", &child];
    let comment = ok_in(&hub, &coordinator, &open);
    assert_eq!(comment["kind"], "review", "{comment}");
    let number = comment["number"].as_str().unwrap().to_owned();
    assert_eq!(
        number,
        format!("{}.1", listed_number(&hub, &child)),
        "the comment is numbered in the child, whose machine keeps the review"
    );
    ok_in(&hub, &coordinator, &["send", &number, "Call it retries"]);
    // Its only pending review, which the child's machine keeps.
    let submit = ["review", "submit", "--body", "One rename"];
    let submitted = ok_in(&hub, &coordinator, &submit);
    assert_eq!(submitted["state"], "finished", "{submitted}");
    message_with(&laptop, &child, "Call it retries");
    // The child answers its writer, which resolves the comment from its
    // own machine.
    ok_in(&laptop, &child, &["send", &number, "Renamed"]);
    message_with(&hub, &coordinator, "Renamed");
    let resolved = ok_in(&hub, &coordinator, &["thread", "resolve", &number, "fixed"]);
    assert_eq!(resolved["resolution"]["outcome"], "fixed", "{resolved}");
}

/// The coordinator's comment of its submitted review of its child on the
/// laptop, which waits for the child, by number.
fn submitted_comment(tree: &Tree) -> String {
    let Tree {
        hub,
        coordinator,
        child,
        ..
    } = tree;
    ok_in(hub, coordinator, &["review", "start", child]);
    let open = ["thread", "open", "Unclear name", "--space", child];
    let number = ok_in(hub, coordinator, &open)["number"]
        .as_str()
        .unwrap()
        .to_owned();
    ok_in(hub, coordinator, &["send", &number, "Call it retries"]);
    let submit = ["review", "submit", "-s", child, "--body", "One rename"];
    ok_in(hub, coordinator, &submit);
    number
}

#[test]
fn a_comment_waiting_on_another_machine_keeps_its_writer_from_cleanup() {
    let tree = tree_across_machines();
    working(&tree.laptop, &tree.child);
    submitted_comment(&tree);
    let status = wait_for("the waiting comment to keep its writer", || {
        let status = tree.hub.ok(&["status", &tree.coordinator]);
        (status["cleanup_reason"]["reason"] == "waiting_thread").then_some(status)
    });
    assert_eq!(status["waiting_threads"], 1, "{status}");
}

#[test]
fn a_coordinator_cancels_its_only_pending_review_kept_on_another_machine() {
    let tree = tree_across_machines();
    let (hub, coordinator, child) = (&tree.hub, &tree.coordinator, &tree.child);
    let started = ok_in(hub, coordinator, &["review", "start", child]);
    let cancelled = ok_in(hub, coordinator, &["review", "cancel"]);
    assert_eq!(cancelled["id"], started["id"], "{cancelled}");
    assert_eq!(cancelled["state"], "cancelled", "{cancelled}");
}

#[test]
fn a_child_on_another_machine_reads_the_files_its_coordinator_posts_to_it() {
    let tree = tree_across_machines();
    let (hub, laptop) = (&tree.hub, &tree.laptop);
    working(laptop, &tree.child);
    let open = ["thread", "open", "Which layout?", "--space", &tree.child];
    let opened = ok_in(hub, &tree.coordinator, &open);
    let number = opened["number"].as_str().unwrap().to_owned();
    let mock = hub.root.path().join("mock.txt");
    std::fs::write(&mock, "two columns").unwrap();
    let attach = mock.to_str().unwrap();
    let post = ["send", &number, "This one?", "--attach", attach];
    ok_in(hub, &tree.coordinator, &post);
    let alert = message_with(laptop, &tree.child, "This one?");
    // Kept in the coordinator's space, which the child's scope does not
    // reach.
    let file = &alert["files"][0];
    assert_eq!(file["space"], json!(tree.coordinator), "{alert}");
    let space = shoal_protocol::space_names::SpaceSelector::id(&tree.coordinator.as_str().into());
    let id = file["id"].as_i64().unwrap();
    let token = scope_of(laptop, &tree.child);
    let agent = super::following::app(laptop).scope(token.as_str().into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let bytes = runtime.block_on(async {
        let read = shoal_client::files::read(&agent, space, id).await.unwrap();
        read.to_end().await.unwrap()
    });
    assert_eq!(bytes, b"two columns");
}

#[test]
fn the_user_opens_and_a_holder_forwards_threads_to_a_space_on_another_machine() {
    let Tree {
        hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    working(&laptop, &child);
    let opened = hub.ok(&["thread", "open", "Which port?", "--space", &child]);
    assert_eq!(
        opened["number"],
        format!("{}.1", listed_number(&hub, &child)),
        "the user's thread is numbered in the space it is opened with: {opened}"
    );
    let number = opened["number"].as_str().unwrap();
    hub.ok(&["send", number, "8080 or 9090?"]);
    message_with(&laptop, &child, "8080 or 9090?");

    let asked = hub.ok(&["thread", "open", "Which cache?", "--space", &coordinator]);
    let asked = asked["number"].as_str().unwrap();
    hub.ok(&["send", asked, "Redis or memcached?"]);
    let forward = ["thread", "forward", asked, "--space", &child];
    ok_in(&hub, &coordinator, &forward);
    message_with(&laptop, &child, "Which cache?");
    ok_in(&laptop, &child, &["send", asked, "Redis"]);
    wait_for("the child's answer in the thread", || {
        let thread = hub.ok(&["view", "thread", asked]);
        let last = thread["posts"].as_array()?.last()?.clone();
        (last["text"] == "Redis" && last["space"]["id"] == child.as_str()).then_some(())
    });
}

/// The number the hub lists the space `id` under.
fn listed_number(hub: &Base, id: &str) -> u64 {
    let spaces = hub.ok(&["list"]);
    let listed = (spaces.as_array().unwrap().iter()).find(|space| space["id"] == id);
    listed.unwrap_or_else(|| panic!("no space {id}: {spaces}"))["number"]
        .as_u64()
        .unwrap()
}

#[test]
fn the_hub_holds_a_message_for_an_offline_machine_and_refuses_what_cannot_wait() {
    let (hub, laptop, there) = laptop_space();
    reroute(&laptop, Some("false"));
    wait_for("the hub to see the laptop go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|member| member["name"] == "laptop");
        (laptop?["connected"] == false).then_some(())
    });
    let queued = hub.ok(&["send", "--space", &there, "while away"]);
    assert_eq!(queued["name"], "laptop", "{queued}");
    assert!(queued["expires_at"].is_number(), "{queued}");
    let stop = hub.ok(&["stop", &there]);
    assert_eq!(stop["name"], "laptop", "{stop}");
    let refused = hub.run(&["rename", &there, "Renamed away"]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("machine laptop is offline"), "{error}");
    reroute(&laptop, None);
    message_with(&laptop, &there, "while away");
}

#[test]
fn a_thread_for_an_offline_coordinator_goes_on_to_the_user() {
    let hub = machine(true, "app");
    let desk = machine(false, "app");
    let config = "[machine.hub]\nescalate_after_minutes = 0\n";
    let laptop = Base::start(home(), config, "app", &[]);
    let desk_id = join(&desk, &hub, "desk");
    let laptop_id = join(&laptop, &hub, "laptop");
    reach(&desk, &laptop);
    let coordinator = workspace(&desk, "coordinator");
    ready_at(&laptop, &coordinator, &desk_id);
    let repo = desk.repo.to_str().unwrap().to_owned();
    desk.ok(&[
        "create",
        &repo,
        "--branch",
        "child",
        "--parent",
        &coordinator,
    ]);
    let child = desk.id("child");
    let moved = desk.ok(&["move", "--ssh", "laptop", &child, "--shoal", shoal_binary()]);
    assert_eq!(moved["failed"], json!([]), "{moved}");
    working(&desk, &coordinator);
    wait_for("the coordinator's agent at the hub", || {
        (listed(&hub, &coordinator, &desk_id)["agent"] == true).then_some(())
    });
    listed(&hub, &child, &laptop_id);
    working(&laptop, &child);
    reroute(&desk, Some("false"));
    wait_for("the hub to see the desk go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let desk = (members.as_array().unwrap().iter()).find(|member| member["name"] == "desk");
        (desk?["connected"] == false).then_some(())
    });
    ask(&laptop, &child);
    wait_for("the thread to reach the user", || {
        let alerts = laptop.ok(&["alert", "list", "--unread"]);
        (alerts.as_array().unwrap().iter())
            .find(|alert| alert["kind"] == "thread_posted" && alert["space_id"] == child.as_str())
            .cloned()
    });
}

#[test]
fn a_child_on_another_machine_holds_no_permission_its_parent_lacks() {
    // The hub's daemon runs while the test holds it.
    let Tree {
        hub: _hub,
        laptop,
        child,
        ..
    } = tree_below_hub_config("[permissions]\npush = \"deny\"\n");
    let status = laptop.ok(&["status", &child]);
    assert_eq!(status["permissions"]["push"]["value"], "deny", "{status}");
}

#[test]
fn removing_a_coordinator_removes_its_child_on_another_machine_first() {
    let Tree {
        hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    let removed = hub.run(&["rm", &coordinator, "--wait", "--yes"]);
    assert!(removed.status.success(), "{}", printed(&removed));
    let gone = laptop.run(&["status", &child, "--json"]);
    assert!(!gone.status.success(), "{}", printed(&gone));
}

#[test]
fn an_offline_child_keeps_its_coordinator_and_goes_once_its_machine_connects() {
    let Tree {
        hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    reroute(&laptop, Some("false"));
    wait_for("the hub to see the laptop go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|member| member["name"] == "laptop");
        (laptop?["connected"] == false).then_some(())
    });
    let refused = hub.run(&["rm", &coordinator, "--wait", "--yes"]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    hub.ok(&["status", &coordinator]);
    reroute(&laptop, None);
    wait_for("the child's removal on the laptop", || {
        let gone = laptop.run(&["status", &child, "--json"]);
        (!gone.status.success()).then_some(())
    });
}

#[test]
fn a_child_restored_on_another_machine_stays_below_its_coordinator() {
    let Tree {
        hub,
        laptop,
        laptop_id,
        coordinator,
        child,
    } = tree_across_machines();
    laptop.ok(&["rm", &child, "--wait", "--yes"]);
    let restored = laptop.ok(&["restore", &child]);
    let below = try_wait_for(|| {
        let listed = listed(&hub, &child, &laptop_id);
        (listed["parent"] == coordinator.as_str()).then_some(())
    });
    assert!(
        below.is_some(),
        "the restored child is not below its coordinator: {restored}"
    );
}

#[test]
fn stopping_a_coordinator_from_its_child_s_machine_stops_the_child_too() {
    // The hub's daemon runs while the test holds it.
    let Tree {
        hub: _hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    working(&laptop, &child);
    let stopped = laptop.run(&["stop", &coordinator, "--json"]);
    assert!(stopped.status.success(), "{}", printed(&stopped));
    let reports = String::from_utf8_lossy(&stopped.stdout);
    assert_eq!(reports.matches("\"stopped\":true").count(), 2, "{reports}");
}

#[test]
fn a_stop_takes_spaces_here_and_on_another_machine_together() {
    let Tree {
        hub,
        laptop: _laptop,
        child,
        ..
    } = tree_across_machines();
    let here = workspace(&hub, "here");
    let stopped = hub.run(&["stop", &here, &child, "--skip-children", "--json"]);
    assert!(stopped.status.success(), "{}", printed(&stopped));
    let reports = String::from_utf8_lossy(&stopped.stdout);
    // One report for the space here, one for the laptop's.
    assert_eq!(reports.matches("\"stopped\":true").count(), 2, "{reports}");
}

#[test]
fn a_removal_in_a_terminal_session_takes_spaces_here_and_on_another_machine_together() {
    let (hub, laptop, there) = laptop_space();
    let here = workspace(&hub, "here");
    // A zmx session's place, which a space of this machine may own.
    let mut command = hub.command();
    command.env("ZMX_SESSION", "terminal");
    let removed = hub.output(command.args(["rm", &here, &there, "--wait", "--yes", "--json"]));
    assert!(removed.status.success(), "{}", printed(&removed));
    let gone = laptop.run(&["status", &there, "--json"]);
    assert!(!gone.status.success(), "{}", printed(&gone));
}
