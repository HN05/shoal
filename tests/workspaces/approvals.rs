//! Approvals and scope: execution scope, exported environments, approval
//! requests and their pickers, and pushes that wait for approval.
use super::simulators::SIM_CONFIG;
use super::*;

#[test]
fn execution_scope_limits_management_and_expires() {
    let fixture = Fixture::new();
    let setup = fixture.repo.join("setup.sh");
    install_test_script(&setup, "#!/bin/sh\nprintf setup >> setup-runs\n");
    let post_setup = fixture.repo.join("post-setup.sh");
    install_test_script(
        &post_setup,
        "#!/bin/sh\ntest -z \"$SHOAL_SCOPE_TOKEN\" || exit 81\ntest -z \"$SHOAL_EXECUTION_ID\" || exit 82\ntest -z \"$SHOAL_RESERVED_PORT_ENV\" || exit 83\ntest -z \"$SHOAL_PORT_WEB\" || exit 84\ntest -z \"$SHOAL_OUTPUT\" || exit 85\nprintf hook >> hook-runs\npython3 -c 'import time; time.sleep(30)' < /dev/null > /dev/null 2>&1 &\n",
    );
    commit_resource_config(
        &fixture.repo,
        "setup.command = 'setup.sh'\nhooks.post_setup = 'post-setup.sh'\n",
    );
    let worker = fixture.add("worker");
    let worker_path = Path::new(worker["path"].as_str().unwrap());
    fixture.add("other");
    let binary = support::shoal_binary();
    let scoped = |args: &[&str]| {
        fixture
            .command()
            .args(["run", "--workspace", "worker", "--", binary])
            .args(args)
            .output()
            .unwrap()
    };
    let output = scoped(&["--json", "work"]);
    assert!(output.status.success());
    let list: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["name"], "worker");
    let output = scoped(&["--json", "config", "show"]);
    assert!(output.status.success(), "{output:?}");
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        config
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["key"] == "setup.command" && entry["value"] == "setup.sh" })
    );
    assert!(scoped(&["acquire", "port", "web"]).status.success());
    let sibling_started = worker_path.join("sibling-started");
    let mut sibling = fixture
        .command()
        .args([
            "run",
            "--workspace",
            "worker",
            "--",
            "sh",
            "-c",
            "touch sibling-started; sleep 30",
        ])
        .spawn()
        .unwrap();
    wait_until("sibling execution", || sibling_started.exists());
    let output = scoped(&["work", "setup", "worker"]);
    assert!(!output.status.success(), "{output:?}");
    fixture.ok(&["stop", "worker"]);
    assert!(!sibling.wait().unwrap().success());
    // An agent's output default does not reach the hook its setup runs.
    let output = fixture
        .command()
        .env("SHOAL_OUTPUT", "json")
        .args(["run", "--workspace", "worker", "--", binary])
        .args(["work", "setup", "worker"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(worker_path.join("setup-runs")).unwrap(),
        "setupsetup"
    );
    assert_eq!(
        fs::read_to_string(worker_path.join("hook-runs")).unwrap(),
        "hookhook"
    );
    assert_eq!(
        fixture.ok(&["work", "status", "worker"])["executions"],
        serde_json::json!([]),
        "hook survivors must not belong to the invoking execution"
    );
    assert!(
        scoped(&[
            "run",
            "--workspace",
            "worker",
            "--",
            binary,
            "leases",
            "port"
        ])
        .status
        .success()
    );
    for args in [
        vec!["rm", "worker", "--yes"],
        vec!["stop", "worker"],
        vec!["work", "status", "other"],
        vec!["run", "--workspace", "other", "--", "true"],
        vec!["work", "setup", "other"],
        vec!["link", "pr", "42", "assigned", "--space", "other"],
        vec!["unlink", "pr", "assigned", "--space", "other"],
        vec!["done", "--undo", "other"],
        vec!["done", "other", "--cleanup"],
        vec!["config", "show", "other"],
        vec![
            "config",
            "set",
            "agent.default",
            "codex",
            "--repo",
            fixture.repo.to_str().unwrap(),
        ],
        vec![
            "config",
            "unset",
            "agent.default",
            "--repo",
            fixture.repo.to_str().unwrap(),
        ],
        vec!["acquire", "port", "web", "--workspace", "other"],
        vec!["repo", "rename", fixture.repo.to_str().unwrap(), "changed"],
        vec!["repo", "config", fixture.repo.to_str().unwrap()],
        vec!["daemon", "stop"],
    ] {
        let output = scoped(&args);
        assert!(
            !output.status.success(),
            "scoped {args:?} unexpectedly succeeded"
        );
    }
    let token = fixture.run(&[
        "run",
        "--workspace",
        "worker",
        "--",
        "sh",
        "-c",
        "printf '%s' \"$SHOAL_SCOPE_TOKEN\"",
    ]);
    let token = String::from_utf8(token.stdout).unwrap();
    assert!(!token.is_empty());
    let output = fixture
        .command()
        .env("SHOAL_SCOPE_TOKEN", token)
        .args(["work"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 2);
}

#[test]
fn redirected_requests_are_inspected_without_prompting_or_deciding() {
    let fixture = Fixture::new();
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    fixture.add("agent");
    let pending = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    let question = fixture.ok(&[
        "questions",
        "ask",
        "Which database?",
        "--recommend",
        "SQLite",
        "--space",
        "agent",
    ]);
    install_test_script(fixture.root.path().join("bin/fzf"), "#!/bin/sh\nexit 77\n");
    for args in [
        vec![
            "approvals".to_owned(),
            pending["number"].as_str().unwrap().to_owned(),
        ],
        vec!["approvals".into()],
        vec![
            "questions".into(),
            question["number"].as_str().unwrap().to_owned(),
        ],
        vec!["questions".into()],
    ] {
        let (output, transcript) =
            fixture.interactive_command(fixture.command().args(&args), "y\n");
        assert!(output.status.success(), "{args:?}: {output:?} {transcript}");
        let details = String::from_utf8_lossy(&output.stdout);
        assert!(
            details.contains("serve preview") || details.contains("Which database?"),
            "{details}"
        );
        assert!(
            !transcript.contains("[y/n]") && !transcript.contains("[SQLite]:"),
            "{transcript}"
        );
    }
    assert_eq!(
        fixture.ok(&["approvals", pending["number"].as_str().unwrap()])["status"],
        "pending"
    );
    assert_eq!(
        fixture.ok(&["questions", question["number"].as_str().unwrap()])["answer"],
        Value::Null
    );
    // An explicit decision remains scriptable with redirected output.
    let (output, transcript) = fixture.interactive_command(
        fixture
            .command()
            .args(["approvals", pending["number"].as_str().unwrap(), "--deny"]),
        "",
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    assert_eq!(
        fixture.ok(&["approvals", pending["number"].as_str().unwrap()])["status"],
        "denied"
    );
}

#[test]
fn approval_picker_inspects_before_deciding_the_selected_request() {
    let fixture = Fixture::new();
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    fixture.add("agent");
    let pending = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    let id = pending["number"].as_str().unwrap().to_owned();
    assert_eq!(fixture.ok(&["approvals", &id])["status"], "pending");
    let (output, rows) = fixture.pick(&["approvals"], &approval_row(&fixture, &pending), "Y\n");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains("serve preview"), "{rows}");
    assert_eq!(fixture.ok(&["approvals", &id])["status"], "approved");
}

#[test]
fn approval_prompt_accepts_only_case_insensitive_y_or_n() {
    let fixture = Fixture::new();
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    fixture.add("agent");
    let pending = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    let id = pending["number"].as_str().unwrap().to_owned();
    for response in ["yes\n", "x\n", "\n"] {
        let (output, transcript) =
            fixture.terminal_command(fixture.command().args(["approvals", &id]), response, true);
        assert_eq!(output.status.code(), Some(130), "{transcript}");
        assert_eq!(fixture.ok(&["approvals", &id])["status"], "pending");
    }
    let (output, transcript) =
        fixture.terminal_command(fixture.command().args(["approvals", &id]), "N\n", true);
    assert!(output.status.success(), "{transcript}");
    assert_eq!(fixture.ok(&["approvals", &id])["status"], "denied");
}

#[test]
fn approval_list_shortcuts_decide_in_place_and_reload_the_list() {
    for (key, status, mark) in [("ctrl-a", "approved", "✓"), ("ctrl-d", "denied", "✗")] {
        let fixture = Fixture::new();
        set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
        let mut pending = Vec::new();
        for agent in ["first", "second"] {
            fixture.add(agent);
            pending.push(pending_approval(scoped_command(
                &fixture,
                agent,
                &["acquire", "port", "web", "--reason", "serve preview"],
            )));
        }
        // Runs the key's actions on the first row as fzf would, then leaves.
        install_test_script(
            fixture.root.path().join("bin/fzf"),
            r#"#!/bin/sh
cat > "$HOME/picker-input"
row=$(head -1 "$HOME/picker-input" | cut -f1)
for arg in "$@"; do
  case "$arg" in "$SHOAL_TEST_KEY:transform-header("*) bind=${arg#*transform-header(} ;; esac
done
header=$(printf '%s' "${bind%%)+reload:*}" | sed "s/{1}/'$row'/")
sh -c "$header" > "$HOME/picker-header"
sh -c "${bind#*)+reload:}" > "$HOME/picker-reload"
exit 130
"#,
        );
        let (output, transcript) = fixture.terminal_command(
            fixture
                .command()
                .arg("approvals")
                .env("SHOAL_TEST_KEY", key),
            "",
            true,
        );
        assert_eq!(output.status.code(), Some(130), "{transcript}");
        let read = |name: &str| fs::read_to_string(fixture.root.path().join(name)).unwrap();
        let header = read("picker-header");
        let statuses: Vec<_> = pending
            .iter()
            .map(|request| {
                fixture.ok(&["approvals", request["number"].as_str().unwrap()])["status"].clone()
            })
            .collect();
        let decided = statuses
            .iter()
            .position(|decided| decided == status)
            .unwrap_or_else(|| panic!("{statuses:?} {header}"));
        assert!(statuses.contains(&Value::from("pending")), "{statuses:?}");
        let id = pending[decided]["number"].as_str().unwrap();
        assert!(
            header.starts_with(&format!("Approval {id} {status}")),
            "{header}"
        );
        let reload = read("picker-reload");
        let first = reload.lines().next().unwrap_or_default();
        let last = reload.lines().last().unwrap_or_default();
        assert!(!first.contains(mark) && last.contains(mark), "{reload}");
    }
}

#[test]
fn approval_list_shortcuts_refuse_a_request_that_changed_since_it_was_listed() {
    let fixture = Fixture::new();
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    fixture.add("agent");
    let pending = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    let id = pending["number"].as_str().unwrap().to_owned();
    let row = approval_row(&fixture, &pending);
    let (shown, _) = row.split_once(':').unwrap();
    // The same number with another record, as a replacement would list.
    let stale = format!("{shown}:{}", "0".repeat(64));
    let output = fixture
        .command()
        .args(["internal", "approval-decide", "approve", &stale])
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["approvals", &id])["status"], "pending");
    let output = fixture
        .command()
        .args(["internal", "approval-decide", "approve", &row])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["approvals", &id])["status"], "approved");
}

#[test]
fn approval_list_shortcuts_find_requests_of_workspaces_below_the_caller() {
    let fixture = Fixture::new();
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    fixture.add("parent");
    let split = scoped_command(&fixture, "parent", &["split", "child"]);
    assert!(split.status.success(), "{split:?}");
    let pending = pending_approval(scoped_command(
        &fixture,
        "child",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    let row = approval_row(&fixture, &pending);
    // The parent lists the child's requests with `--workspace child`; its
    // decision reaches the daemon, which decides only routed requests.
    let output = scoped_command(
        &fixture,
        "parent",
        &["internal", "approval-decide", "approve", &row],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(stderr.contains("routed to them"), "{stderr}");
}

#[test]
fn question_picker_answers_the_selected_question_with_a_suggestion() {
    let fixture = Fixture::new();
    fixture.add("asker");
    let question = fixture.ok(&[
        "questions",
        "ask",
        "Which database?",
        "--recommend",
        "SQLite",
        "--choice",
        "SQLite",
        "--choice",
        "Postgres",
        "--space",
        "asker",
    ]);
    let id = question["number"].as_str().unwrap().to_owned();
    assert_eq!(fixture.ok(&["questions", &id])["answer"], Value::Null);
    let (output, rows) = fixture.pick(&["questions"], &id, "1\n");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains("Which database?"), "{rows}");
    assert_eq!(fixture.ok(&["questions", &id])["answer"], "SQLite");
}

#[test]
fn question_prompt_accepts_the_recommendation_with_enter() {
    let fixture = Fixture::new();
    fixture.add("asker");
    for (recommendation, choices, input, expected) in [
        ("SQLite", vec![], "\n", "SQLite"),
        ("2", vec!["2", "4"], "\n", "2"),
        ("2", vec!["2", "4"], "2\n", "4"),
        ("/cancel", vec![], "\n", "/cancel"),
        ("/withdraw", vec![], "\n", "/withdraw"),
    ] {
        let text = format!("Choose {recommendation} for {input:?}?");
        let mut args = vec![
            "questions",
            "ask",
            &text,
            "--recommend",
            recommendation,
            "--space",
            "asker",
        ];
        for choice in choices {
            args.extend(["--choice", choice]);
        }
        let question = fixture.ok(&args);
        let id = question["number"].as_str().unwrap().to_owned();
        let (output, transcript) =
            fixture.terminal_command(fixture.command().args(["questions", &id]), input, true);
        assert!(output.status.success(), "{transcript}");
        assert_eq!(fixture.ok(&["questions", &id])["answer"], expected);
    }
}

#[test]
fn approval_decisions_pick_an_omitted_pending_request() {
    let fixture = Fixture::with_config(Some("[resources.signing]\nrequires_approval=true\n"));
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    fixture.add("agent");
    let none = fixture.run(&["approvals", "--approve"]);
    assert!(!none.status.success(), "{none:?}");
    let signing = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "resource", "signing", "--reason", "sign build"],
    ));
    let web = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    let (signing_row, web_row) = (
        approval_row(&fixture, &signing),
        approval_row(&fixture, &web),
    );
    let number = |request: &Value| request["number"].as_str().unwrap().to_owned();
    let (signing, web) = (number(&signing), number(&web));
    let (output, rows) = fixture.pick(&["approvals", "--approve"], &web_row, "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains(&format!("{signing_row}\t")) && rows.contains(&format!("{web_row}\t")));
    let (output, rows) = fixture.pick(&["approvals", "--deny"], &signing_row, "");
    assert!(output.status.success(), "{output:?}");
    assert!(!rows.contains(&format!("{web_row}\t")), "{rows:?}");
    let statuses: Vec<_> = fixture
        .ok(&["approvals"])
        .as_array()
        .unwrap()
        .iter()
        .map(|request| {
            (
                request["number"].as_str().unwrap().to_owned(),
                request["status"].clone(),
            )
        })
        .collect();
    assert!(statuses.contains(&(web, "approved".into())));
    assert!(statuses.contains(&(signing, "denied".into())));
}

#[test]
fn agents_withdraw_requests_they_no_longer_need() {
    let fixture = Fixture::new();
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    fixture.add("agent");
    let web = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    let id = web["number"].as_str().unwrap().to_owned();
    let output = scoped_command(&fixture, "agent", &["approvals", "withdraw", &id]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["approvals"])[0]["status"], "withdrawn");
    let refused = fixture.run(&["approvals", "--approve", &id]);
    assert!(!refused.status.success(), "{refused:?}");
}

/// The picker row id of an approval request as `fixture` lists it now: its
/// number and a digest of the record, serialized with sorted keys.
fn approval_row(fixture: &Fixture, request: &Value) -> String {
    use sha2::Digest;
    let id = request["number"].as_str().unwrap().to_owned();
    let record = serde_json::to_vec(&fixture.ok(&["approvals", &id])).unwrap();
    let digest: String = sha2::Sha256::digest(record)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{id}:{digest}")
}

pub(crate) fn pending_approval(output: Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["code"], "approval_pending");
    value["request"].clone()
}

#[test]
fn release_cancels_pending_requests_without_leases() {
    let fixture = Fixture::with_config(Some(
        "[resources.signing]\nrequires_approval=true\n[secrets.TOKEN]\ncommand=['true']\ntools=['sign']\nrequires_approval=true\n",
    ));
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    fixture.add("agent");
    for args in [
        &["acquire", "resource", "signing", "--reason", "sign build"][..],
        &["acquire", "port", "web", "--reason", "serve preview"],
        &["acquire", "secret", "TOKEN", "--reason", "sign build"],
    ] {
        pending_approval(scoped_command(&fixture, "agent", args));
    }
    assert_eq!(fixture.ok(&["approvals"]).as_array().unwrap().len(), 3);
    let released = fixture.ok(&[
        "release",
        "resource",
        "signing",
        "--name",
        "default",
        "--workspace",
        "agent",
    ]);
    assert_eq!(
        released["released"],
        serde_json::json!([{"kind": "resource", "pool": "signing", "name": "default"}])
    );
    let released = fixture.ok(&["release", "--workspace", "agent"]);
    assert_eq!(
        released["released"],
        serde_json::json!([{"kind": "port", "name": "web"}, {"kind": "secret", "name": "TOKEN"}])
    );
    assert!(
        fixture
            .ok(&["approvals"])
            .as_array()
            .unwrap()
            .iter()
            .all(|request| request["active"] == false)
    );
}

#[test]
fn resource_approvals_require_unscoped_decisions_and_preserve_capacity() {
    let mut fixture = Fixture::with_config(Some("[resources.signing]\nrequires_approval=true\n"));
    fixture.add("agent");
    fixture.add("human");
    let args = ["acquire", "resource", "signing", "--reason", "sign build"];
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
    let id = pending["number"].as_str().unwrap().to_owned();
    let id = id.as_str();
    assert_eq!(pending["space"], "agent");
    assert_eq!(
        fixture.ok(&["catalog", "resource", "--workspace", "agent"])["pools"][0]["resources"]["signing"]
            ["requires_approval"],
        true
    );
    assert!(
        fixture.ok(&["leases", "resource", "--workspace", "agent"])["pools"][0]["resources"][0]
            .get("requires_approval")
            .is_none()
    );
    assert_eq!(
        pending_approval(scoped_command(&fixture, "agent", &args))["number"],
        pending["number"]
    );
    assert_eq!(fixture.ok(&["approvals"])[0]["number"], pending["number"]);
    assert!(
        fixture.ok(&["leases", "resource", "--workspace", "agent"])["leases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !scoped_command(&fixture, "agent", &["approvals", "--approve", id])
            .status
            .success()
    );
    assert!(
        !scoped_command(&fixture, "agent", &["approvals", "--deny", id])
            .status
            .success()
    );
    assert!(
        !scoped_command(&fixture, "human", &["approvals", "--space", "agent"])
            .status
            .success()
    );
    let other: Value =
        serde_json::from_slice(&scoped_command(&fixture, "human", &["approvals"]).stdout).unwrap();
    assert!(other.as_array().unwrap().is_empty());
    fixture.ok(&["acquire", "resource", "signing", "--workspace", "human"]);
    fixture.ok(&["approvals", "--approve", id]);
    fixture.restart();
    let busy: Value =
        serde_json::from_slice(&scoped_command(&fixture, "agent", &args).stdout).unwrap();
    assert_eq!(busy["code"], "resource_busy");
    fixture.ok(&[
        "release",
        "resource",
        "signing",
        "--workspace",
        "human",
        "--name",
        "default",
    ]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    assert!(
        scoped_command(
            &fixture,
            "agent",
            &["release", "resource", "signing", "--name", "default"]
        )
        .status
        .success()
    );
    // Release deleted the decided request, so the new one reuses its number.
    let next = pending_approval(scoped_command(&fixture, "agent", &args));
    assert_eq!(next["status"], "pending");
    assert_eq!(next["number"], pending["number"]);
    fixture.ok(&["approvals", "--deny", next["number"].as_str().unwrap()]);
    let denied: Value =
        serde_json::from_slice(&scoped_command(&fixture, "agent", &args).stdout).unwrap();
    assert_eq!(denied["code"], "approval_denied");
    fixture.ok(&[
        "release",
        "resource",
        "signing",
        "--workspace",
        "agent",
        "--name",
        "default",
    ]);
    assert!(fixture.ok(&["approvals"]).as_array().unwrap().is_empty());
}

#[test]
fn resource_approvals_bind_a_member_even_when_the_pool_is_busy() {
    let fixture = Fixture::with_config(Some(
        "[resource_pools.devices]\ncapacity=1\n\
         [resource_pools.devices.resources.alpha]\nrequires_approval=true\n\
         [resource_pools.devices.resources.beta]\nrequires_approval=true\n",
    ));
    fixture.add("human");
    fixture.add("agent");
    fixture.ok(&[
        "acquire",
        "resource",
        "devices",
        "--workspace",
        "human",
        "--member",
        "alpha",
    ]);
    let args = ["acquire", "resource", "devices", "--reason", "test device"];
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
    let id = pending["number"].as_str().unwrap().to_owned();
    let id = id.as_str();
    assert_eq!(pending["specification"]["member"], "alpha");
    assert_eq!(pending["specification"]["mode"], "permit");
    assert_eq!(fixture.ok(&["approvals"])[0], pending);
    let overview = fixture.ok(&["leases", "resource", "--workspace", "agent"]);
    assert_eq!(overview["pools"][0]["used"], 1);
    assert_eq!(overview["leases"], serde_json::json!([]));
    fixture.ok(&["approvals", "--approve", id]);
    let busy = scoped_command(&fixture, "agent", &args);
    assert_eq!(busy.status.code(), Some(2));
    let busy: Value = serde_json::from_slice(&busy.stdout).unwrap();
    assert_eq!(busy["code"], "resource_busy");
    let changed = scoped_command(
        &fixture,
        "agent",
        &["acquire", "resource", "devices", "--member", "beta"],
    );
    assert!(!changed.status.success());
    fixture.ok(&[
        "release",
        "resource",
        "devices",
        "--workspace",
        "human",
        "--name",
        "default",
    ]);
    let acquired = scoped_command(&fixture, "agent", &args);
    assert!(acquired.status.success());
    let lease: Value = serde_json::from_slice(&acquired.stdout).unwrap();
    assert_eq!(lease["resource"], "alpha");
    assert_eq!(
        fixture.ok(&["leases", "resource", "--workspace", "agent"])["pools"][0]["used"],
        1
    );
}

#[test]
fn workspace_approvals_survive_release_but_do_not_expand_access_modes() {
    let mut fixture = Fixture::with_config(Some(
        "[resources.cache]\nkind='rwlock'\nrequires_approval=true\napproval_lifetime='workspace'\n",
    ));
    fixture.add("agent");
    let args = [
        "acquire",
        "resource",
        "cache",
        "--mode",
        "read",
        "--reason",
        "inspect cache",
    ];
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
    fixture.ok(&[
        "approvals",
        "--approve",
        pending["number"].as_str().unwrap(),
    ]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&[
        "release",
        "resource",
        "cache",
        "--workspace",
        "agent",
        "--name",
        "default",
    ]);
    fixture.restart();
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&[
        "release",
        "resource",
        "cache",
        "--workspace",
        "agent",
        "--name",
        "default",
    ]);
    let write = pending_approval(scoped_command(
        &fixture,
        "agent",
        &[
            "acquire",
            "resource",
            "cache",
            "--mode",
            "write",
            "--reason",
            "rebuild cache",
        ],
    ));
    assert_ne!(write["number"], pending["number"]);
    fixture.ok(&["rm", "agent", "--yes"]);
    assert!(fixture.ok(&["approvals"]).as_array().unwrap().is_empty());
}

#[test]
fn port_approvals_bind_overrides_and_follow_both_lifetimes() {
    for lifetime in ["lease", "workspace"] {
        let mut fixture = Fixture::new();
        commit_resource_config(
            &fixture.repo,
            &format!("[ports.web]\nrequires_approval=true\napproval_lifetime='{lifetime}'\n"),
        );
        fixture.add("agent");
        let args = ["acquire", "port", "web", "--reason", "serve preview"];
        let pending = pending_approval(scoped_command(&fixture, "agent", &args));
        assert!(
            fixture.ok(&["leases", "port", "--workspace", "agent"])["reserved"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let id = pending["number"].as_str().unwrap().to_owned();
        let id = id.as_str();
        fixture.ok(&["approvals", "--approve", id]);
        assert!(
            !scoped_command(
                &fixture,
                "agent",
                &[
                    "acquire",
                    "port",
                    "web",
                    "--env",
                    "OTHER_PORT",
                    "--reason",
                    "serve preview",
                ]
            )
            .status
            .success()
        );
        fixture.restart();
        assert!(scoped_command(&fixture, "agent", &args).status.success());
        fixture.ok(&["release", "port", "web", "--workspace", "agent"]);
        let next = scoped_command(&fixture, "agent", &args);
        if lifetime == "workspace" {
            assert!(
                next.status.success(),
                "{}",
                String::from_utf8_lossy(&next.stderr)
            );
        } else {
            assert_eq!(pending_approval(next)["status"], "pending");
        }
        fixture.ok(&["release", "port", "web", "--workspace", "agent"]);
    }
}

#[test]
#[cfg(target_os = "macos")]
fn simulator_approvals_precede_mutations_and_cannot_be_bypassed_by_device_args() {
    let config = SIM_CONFIG.replace(
        "[machine.simulators.profiles.phone]",
        "[machine.simulators.profiles.phone]\nrequires_approval=true\napproval_lifetime='workspace'",
    );
    let mut fixture = Fixture::with_tools(Some(&config), true);
    commit_resource_config(&fixture.repo, "[simulators]\nrequires_approval=false\n");
    fixture.add("agent");
    let args = ["acquire", "sim", "--reason", "test app"];
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
    let explicit = [
        "acquire",
        "sim",
        "--device",
        "type.Phone",
        "--runtime",
        "iOS Test",
        "--reason",
        "test app",
    ];
    assert_eq!(
        pending_approval(scoped_command(&fixture, "agent", &explicit))["number"],
        pending["number"]
    );
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    assert!(
        events
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).unwrap()[0] == "list")
    );
    assert!(!fixture.root.path().join("sim-devices.json").exists());
    fixture.ok(&[
        "approvals",
        "--approve",
        pending["number"].as_str().unwrap(),
    ]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "agent",
    ]);
    fixture.restart();
    assert!(
        scoped_command(&fixture, "agent", &explicit)
            .status
            .success()
    );
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "agent",
    ]);
    let clean = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "sim", "--clean", "--reason", "isolate app state"],
    ));
    assert_ne!(clean["number"], pending["number"]);
    fixture.ok(&["approvals", "--deny", clean["number"].as_str().unwrap()]);
    let denied: Value = serde_json::from_slice(
        &scoped_command(
            &fixture,
            "agent",
            &["acquire", "sim", "--clean", "--reason", "isolate app state"],
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(denied["code"], "approval_denied");
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "agent",
    ]);
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    assert!(!events.contains("\"erase\""));
    let child = fixture
        .command()
        .args([
            "run",
            "--workspace",
            "agent",
            "--",
            support::shoal_binary(),
            "--json",
            "acquire",
            "sim",
            "--clean",
            "--reason",
            "reset test data",
            "--queue",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let id = wait_for("pending acquisition approval", || {
        let requests = fixture.ok(&["approvals"]);
        requests
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["status"] == "pending")
            .map(|request| request["number"].as_str().unwrap().to_owned())
    });
    fixture.ok(&["approvals", "--approve", &id]);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let history = fixture.ok(&["history", "sim", "--workspace", "agent"]);
    assert_eq!(history[0]["status"], "acquired");
    assert_eq!(
        history
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["request"]["reason"] == "reset test data")
            .count(),
        1,
        "polling must keep one clean audit request"
    );
    assert_eq!(history[0]["request"]["reason"], "reset test data");
    assert_eq!(history[0]["action"], "create");
}

#[test]
#[cfg(target_os = "macos")]
fn simulator_approval_defaults_layer_per_option_and_release_expires_lease_grants() {
    let config = SIM_CONFIG.replace(
        "[machine.simulators]",
        "[simulators]\nrequires_approval=true\napproval_lifetime='workspace'\n[machine.simulators]",
    );
    let fixture = Fixture::with_tools(Some(&config), true);
    commit_resource_config(&fixture.repo, "[simulators]\napproval_lifetime='lease'\n");
    fixture.add("agent");
    let args = ["acquire", "sim", "--reason", "test app"];
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
    assert_eq!(pending["lifetime"], "lease");
    fixture.ok(&[
        "approvals",
        "--approve",
        pending["number"].as_str().unwrap(),
    ]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "agent",
    ]);
    assert_eq!(
        pending_approval(scoped_command(&fixture, "agent", &args))["status"],
        "pending"
    );
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "agent",
    ]);
}

#[test]
#[cfg(target_os = "macos")]
fn exported_scopes_request_simulator_approval_before_mutations() {
    let config = SIM_CONFIG.replace(
        "[machine.simulators.profiles.phone]",
        "[machine.simulators.profiles.phone]\nrequires_approval=true",
    );
    let fixture = Fixture::with_tools(Some(&config), true);
    let workspace = fixture.add("external");
    let exported = fixture.ok(&["work", "env", "external"]);
    let values: std::collections::BTreeMap<String, String> =
        serde_json::from_value(exported).unwrap();
    let acquire = |args: &[&str]| {
        fixture
            .command()
            .envs(&values)
            .current_dir(workspace["path"].as_str().unwrap())
            .args(["--json", "acquire", "sim"])
            .args(args)
            .output()
            .unwrap()
    };
    let pending = pending_approval(acquire(&["--reason", "test app"]));
    fixture.ok(&["approvals", "--deny", pending["number"].as_str().unwrap()]);
    fixture.ok(&[
        "release",
        "sim",
        "--name",
        "default",
        "--workspace",
        "external",
    ]);
    let clean = pending_approval(acquire(&["--clean", "--reason", "isolate app state"]));
    assert_eq!(clean["specification"]["clean"], true);
    let events = fs::read_to_string(fixture.root.path().join("sim-events")).unwrap();
    assert!(
        events
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).unwrap()[0] == "list")
    );
    assert!(!fixture.root.path().join("sim-devices.json").exists());
}

#[test]
fn tampered_resource_approvals_fail_instead_of_selecting_a_member() {
    let fixture = Fixture::with_config(Some("[resources.signing]\nrequires_approval=true\n"));
    fixture.add("agent");
    let args = ["acquire", "resource", "signing", "--reason", "sign build"];
    let pending = pending_approval(scoped_command(&fixture, "agent", &args));
    fixture.ok(&[
        "approvals",
        "--approve",
        pending["number"].as_str().unwrap(),
    ]);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let mut record = pending.clone();
    record["status"] = "approved".into();
    record["specification"] = serde_json::json!({
        "preferred": null, "env": "PORT_WEB", "on_conflict": "suggest", "range": [3000, 3100]
    });
    let tamper = |record: &Value| {
        db.execute(
            "UPDATE access_requests SET record=?3 WHERE space_number=?1 AND number=?2",
            rusqlite::params![
                pending["space_number"].as_i64().unwrap(),
                pending["item"].as_i64().unwrap(),
                record.to_string()
            ],
        )
        .unwrap();
    };
    tamper(&record);
    let output = scoped_command(&fixture, "agent", &args);
    assert!(!output.status.success(), "{output:?}");
    record["specification"] = serde_json::json!({"member": "signing"});
    tamper(&record);
    let output = scoped_command(&fixture, "agent", &args);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        fixture.ok(&["leases", "resource", "--workspace", "agent"])["leases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn releasing_resource_names_clears_requests_from_previous_pool_scopes() {
    let config = "[resources.signing]\nrequires_approval=true\n";
    let mut fixture = Fixture::with_config(Some(config));
    let workspace = fixture.add("agent");
    let args = ["acquire", "resource", "signing", "--reason", "sign build"];
    let previous = pending_approval(scoped_command(&fixture, "agent", &args));
    fs::write(fixture.root.path().join(".config/shoal/config.toml"), "").unwrap();
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join(".shoal.toml"),
        config,
    )
    .unwrap();
    fixture.restart();
    let current = pending_approval(scoped_command(&fixture, "agent", &args));
    assert_ne!(previous["target"], current["target"]);
    fixture.ok(&[
        "approvals",
        "--approve",
        current["number"].as_str().unwrap(),
    ]);
    assert!(scoped_command(&fixture, "agent", &args).status.success());
    fixture.ok(&[
        "release",
        "resource",
        "signing",
        "--workspace",
        "agent",
        "--name",
        "default",
    ]);
    assert!(fixture.ok(&["approvals"]).as_array().unwrap().is_empty());
    assert_ne!(
        pending_approval(scoped_command(&fixture, "agent", &args))["number"],
        current["number"]
    );
}

#[test]
fn exported_environment_delivers_ports_and_confines_external_processes() {
    let mut fixture = Fixture::new();
    install_test_script(fixture.repo.join("setup.sh"), "#!/bin/sh\nexit 0\n");
    git(&fixture.repo, &["add", "setup.sh"]);
    commit_resource_config(&fixture.repo, "setup.command = 'setup.sh'\n");
    let workspace = fixture.add("external");
    fixture.add("other");
    let web = fixture.ok(&["acquire", "port", "web", "--workspace", "external"]);
    let api = fixture.ok(&[
        "acquire",
        "port",
        "api",
        "--workspace",
        "external",
        "--env",
        "API_PORT",
    ]);
    let exported = fixture.ok(&["work", "env", "external"]);
    let values: std::collections::BTreeMap<String, String> =
        serde_json::from_value(exported.clone()).unwrap();
    assert_eq!(exported["SHOAL_WORKSPACE_ID"], workspace["id"]);
    assert_eq!(exported["SHOAL_RUN_ID"], workspace["id"]);
    assert_eq!(exported["SHOAL_WORKSPACE"], "external");
    assert_eq!(
        exported["SHOAL_STATE_DIR"],
        fixture.root.path().join("state").to_str().unwrap()
    );
    assert_eq!(exported["SHOAL_PORT_WEB"], web["port"].to_string());
    assert_eq!(exported["API_PORT"], api["port"].to_string());
    assert_eq!(
        exported["SHOAL_RESERVED_PORT_ENV"],
        "API_PORT:SHOAL_PORT_WEB"
    );
    assert!(!values.contains_key("SHOAL_EXECUTION_ID"));
    let child = support::isolated(fixture.root.path(), "sh")
        .envs(&values)
        .args(["-c", "printf '%s|%s' \"$SHOAL_PORT_WEB\" \"$API_PORT\""])
        .output()
        .unwrap();
    assert!(child.status.success());
    assert_eq!(
        String::from_utf8(child.stdout).unwrap(),
        format!("{}|{}", web["port"], api["port"])
    );
    assert!(
        fixture.ok(&["work", "status", "external"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let scoped = |args: &[&str]| {
        fixture
            .command()
            .envs(&values)
            .current_dir(workspace["path"].as_str().unwrap())
            .args(args)
            .output()
            .unwrap()
    };
    for args in [
        vec!["work", "status"],
        vec!["leases", "port"],
        vec!["work", "setup"],
        vec!["run", "--", support::shoal_binary(), "leases", "port"],
    ] {
        let output = scoped(&args);
        assert!(output.status.success(), "{args:?}: {output:?}");
    }
    // A process with the exported environment opens workspaces only below
    // its own.
    let below = scoped(&["start", fixture.repo.to_str().unwrap(), "below"]);
    assert!(below.status.success(), "{below:?}");
    let below = fixture.ok(&["work", "status", "below"]);
    assert_eq!(below["workspace"]["parent"]["name"], "external", "{below}");
    for args in [
        vec!["rm", "external", "--yes"],
        vec!["rm", "other", "--yes"],
        vec!["alerts"],
        vec!["work", "status", "other"],
        vec!["work", "env"],
        vec!["work", "env", "other"],
        vec![
            "work",
            "env",
            "external",
            "--revoke",
            &values["SHOAL_SCOPE_TOKEN"],
        ],
    ] {
        let output = scoped(&args);
        assert!(!output.status.success(), "{args:?}: {output:?}");
    }
    fixture.restart();
    let output = fixture
        .command()
        .envs(&values)
        .args(["work", "status", "external"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn exported_tokens_expire_on_revocation_and_successful_removal() {
    let fixture = Fixture::new();
    let workspace = fixture.add("external");
    fixture.add("other");
    let first = fixture.ok(&["work", "env", "external"]);
    let second = fixture.ok(&["work", "env", "external"]);
    let first_token = first["SHOAL_SCOPE_TOKEN"].as_str().unwrap();
    let second_token = second["SHOAL_SCOPE_TOKEN"].as_str().unwrap();
    let inspect = |token: &str| {
        fixture
            .command()
            .env("SHOAL_SCOPE_TOKEN", token)
            .args(["work", "status", "external"])
            .output()
            .unwrap()
    };
    let wrong_owner = fixture.run(&["work", "env", "other", "--revoke", first_token]);
    assert!(!wrong_owner.status.success());
    assert!(inspect(first_token).status.success());
    assert_eq!(
        fixture.ok(&["work", "env", "external", "--revoke", first_token])["revoked"],
        true
    );
    let revoked = inspect(first_token);
    assert!(!revoked.status.success());
    assert!(inspect(second_token).status.success());
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join("dirty"),
        "preserve\n",
    )
    .unwrap();
    assert!(!fixture.run(&["rm", "external"]).status.success());
    assert!(inspect(second_token).status.success());
    fixture.ok(&["rm", "external", "--yes"]);
    let removed = inspect(second_token);
    assert!(!removed.status.success());
    // The branch had no commits, so removal deleted it too.
    fixture.ok(&["start", fixture.repo.to_str().unwrap(), "external"]);
    assert!(!inspect(second_token).status.success());
    assert!(fixture.ok(&["work", "env", "external"])["SHOAL_SCOPE_TOKEN"].is_string());
}

#[test]
fn an_agent_push_waits_for_approval_and_approving_runs_it() {
    let fixture = Fixture::with_config(Some("permissions.push = \"propose\"\n"));
    let remote = fixture.root.path().join("remote.git");
    git(
        fixture.root.path(),
        &["init", "-q", "--bare", remote.to_str().unwrap()],
    );
    let remote = remote.to_str().unwrap();
    git(&fixture.repo, &["remote", "add", "origin", remote]);
    git(&fixture.repo, &["push", "-q", "origin", "main"]);
    git(&fixture.repo, &["remote", "set-head", "origin", "main"]);
    let workspace = fixture.add("pusher");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let head = git(path, &["rev-parse", "HEAD"]);
    // What an agent launch adds: the helper on the PATH, and the rewrite
    // that sends pushes to the remote through it.
    let helper = fixture.root.path().join("push-gate");
    fs::create_dir_all(&helper).unwrap();
    install_test_script(
        helper.join("git-remote-shoal"),
        format!(
            "#!/bin/sh\nexec '{}' internal push-gate \"$@\"\n",
            support::shoal_binary()
        ),
    );
    let path_with_helper = std::env::join_paths(std::iter::once(helper.clone()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let pushed = || {
        let refs = git(fixture.root.path(), &["--git-dir", remote, "for-each-ref"]);
        refs.contains("refs/heads/pusher")
    };

    // Your own push from the worktree carries no workspace scope, so the
    // helper pushes it at once.
    let own = support::isolated(fixture.root.path(), "git")
        .current_dir(path)
        .env("PATH", &path_with_helper)
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("url.shoal::{remote}.pushInsteadOf"),
        )
        .env("GIT_CONFIG_VALUE_0", remote)
        .args(["push", "origin", "HEAD:refs/heads/yours"])
        .output()
        .unwrap();
    assert!(
        own.status.success(),
        "{}",
        String::from_utf8_lossy(&own.stderr)
    );
    let refs = git(fixture.root.path(), &["--git-dir", remote, "for-each-ref"]);
    assert!(refs.contains("refs/heads/yours"), "{refs}");

    // The agent's push carries the workspace's scope, as `shoal run` gives it.
    let push = fixture
        .command()
        .current_dir(path)
        .env("PATH", &path_with_helper)
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("url.shoal::{remote}.pushInsteadOf"),
        )
        .env("GIT_CONFIG_VALUE_0", remote)
        .args(["run", "--workspace", "pusher", "--"])
        .args([
            "git",
            "push",
            "--no-verify",
            "origin",
            "HEAD:refs/heads/pusher",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&push.stderr);
    assert!(!push.status.success(), "{stderr}");
    assert!(!pushed());

    let requests = fixture.ok(&["approvals"]);
    let request = &requests[0];
    assert_eq!(request["target"], "push", "{requests}");
    assert_eq!(request["specification"]["commit"], head.trim());
    let approved = fixture.ok(&[
        "approvals",
        "--approve",
        request["number"].as_str().unwrap(),
    ]);
    assert_eq!(approved["status"], "approved", "{approved}");
    assert!(pushed());
    assert_eq!(
        git(
            fixture.root.path(),
            &["--git-dir", remote, "rev-parse", "pusher"]
        )
        .trim(),
        head.trim()
    );
}
