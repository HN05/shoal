//! Issue and PR actions against a scripted forge: `gh` on GitHub, `glab` on
//! GitLab, an HTTP server on Forgejo, all from `fixtures/forge.py`.
// These tests never restart the daemon.
#[allow(dead_code)]
#[path = "support/daemon.rs"]
mod daemon_fixture;
use daemon_fixture::DaemonGuard;

#[path = "support/git.rs"]
mod git_fixture;
use git_fixture::{git, init_repo};

mod support;

use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Output},
};

const FORGE: &str = include_str!("fixtures/forge.py");

#[derive(Clone, Copy, PartialEq)]
enum Forge {
    GitHub,
    Forgejo,
}

struct Fixture {
    // Fields drop in declaration order: the daemon and server before the directory.
    _daemon: DaemonGuard,
    server: Option<Child>,
    root: tempfile::TempDir,
    repo: PathBuf,
    /// `owner/repository` URL prefix of items on the scripted forge.
    web: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(server) = &mut self.server {
            let _ = server.kill();
            let _ = server.wait();
        }
    }
}

/// The user's and the agent account's tokens on the scripted forge.
const AGENT_TOKEN: &str =
    "[machine.forges.\"{forge}\"]\ntoken = \"usertoken\"\nagent_token = \"agenttoken\"\n";

/// Start the Forgejo server in `root` and return it with its `host:port`.
fn serve(root: &Path) -> (Child, String) {
    let port_file = root.join("forge/port");
    let child = support::isolated(root, "python3")
        .arg(root.join("forge.py"))
        .args(["serve", port_file.to_str().unwrap()])
        .spawn()
        .unwrap();
    // A hang guard only: the server writes its port once it listens.
    support::wait::wait_until("forge server to start", || port_file.exists());
    let port = fs::read_to_string(port_file).unwrap();
    (child, format!("127.0.0.1:{port}"))
}

impl Fixture {
    fn new(forge: Forge, config: &str) -> Self {
        Self::with_daemon_env(forge, config, &[])
    }

    /// A fixture whose daemon also has `env` in its environment.
    /// `config` names the forge's host as `{forge}`; on Forgejo the user has
    /// the token `usertoken` there unless `config` sets the host's tokens.
    fn with_daemon_env(forge: Forge, config: &str, env: &[(&str, &str)]) -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        fs::create_dir_all(root.path().join("forge")).unwrap();
        fs::write(root.path().join("forge/responses.json"), "{}").unwrap();
        let script = root.path().join("forge.py");
        support::install_test_script(&script, FORGE);
        let server = (forge == Forge::Forgejo).then(|| serve(root.path()));
        let host = server
            .as_ref()
            .map_or_else(|| "github.com".to_owned(), |(_, host)| host.clone());
        let mut config = config.replace("{forge}", &host);
        let table = format!("[machine.forges.\"{host}\"]");
        if forge == Forge::Forgejo && !config.contains(&table) {
            config.push_str(&format!("\n{table}\ntoken = \"usertoken\"\n"));
        }
        fs::create_dir_all(root.path().join(".config/shoal")).unwrap();
        fs::write(root.path().join(".config/shoal/config.toml"), config).unwrap();
        let repo = init_repo(root.path(), "repo", &[("tracked", "committed\n")]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let mut launch = support::cli(root.path());
        launch.envs(env.iter().copied());
        let daemon = DaemonGuard::start(root.path(), &mut launch);
        let mut fixture = Self {
            _daemon: daemon,
            server: server.map(|(child, _)| child),
            root,
            repo,
            web: String::new(),
        };
        fixture.ok(&["repo", "add", fixture.repo.to_str().unwrap()]);
        fixture.ok(&["add", fixture.repo.to_str().unwrap(), "topic"]);
        let origin = match forge {
            Forge::GitHub => {
                fs::create_dir_all(fixture.root.path().join("bin")).unwrap();
                std::os::unix::fs::symlink(&script, fixture.root.path().join("bin/gh")).unwrap();
                fixture.web = "https://github.com/team/project".into();
                "git@github.com:team/project.git".to_owned()
            }
            Forge::Forgejo => {
                fixture.web = format!("http://{host}/team/project");
                format!("http://{host}/team/project.git")
            }
        };
        git(&fixture.repo, &["remote", "add", "origin", &origin]);
        git(
            &fixture.repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        fixture
    }

    /// Replace the global config, which the next CLI command reads.
    fn write_config(&self, config: &str) {
        fs::write(self.root.path().join(".config/shoal/config.toml"), config).unwrap();
    }

    fn respond(&self, responses: Value) {
        fs::write(
            self.root.path().join("forge/responses.json"),
            responses.to_string(),
        )
        .unwrap();
    }

    /// Requests since the last call, as `[key, body, auth]`.
    fn requests(&self) -> Vec<Value> {
        let log = self.root.path().join("forge/requests.jsonl");
        let text = fs::read_to_string(&log).unwrap_or_default();
        let _ = fs::remove_file(&log);
        text.lines()
            .map(|line| {
                let request: Value = serde_json::from_str(line).unwrap();
                json!([request["key"], request["body"], request["auth"]])
            })
            .collect()
    }

    fn command(&self) -> Command {
        support::cli(self.root.path())
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Value {
        let output = self.command().arg("--json").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn request(&self, method: Value) -> Value {
        self.request_as(method, None)
    }

    fn request_as(&self, method: Value, scope: Option<&str>) -> Value {
        let root = self.root.path();
        let protocol = support::daemon_client::protocol(root);
        support::daemon_client::exchange(
            root,
            &json!({"protocol": protocol, "id": 1, "method": method, "scope": scope}),
        )
    }

    fn events(&self) -> Vec<Value> {
        let output = self.run(&["--json", "internal", "events"]);
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn pull(number: u64, state: &str, title: &str, labels: &[&str]) -> Value {
    json!({"number": number, "title": title, "state": state, "draft": false,
        "merged": state == "merged", "node_id": "PR_node",
        "labels": labels.iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
        "head": {"ref": "topic", "repo": {"full_name": "team/project"}},
        "base": {"ref": "main"}})
}

#[test]
fn item_actions_send_rest_requests_and_record_events() {
    for forge in [Forge::GitHub, Forge::Forgejo] {
        let fixture = Fixture::new(forge, "");
        let endpoint = "repos/team/project/pulls/8";
        fixture.respond(json!({
            format!("GET {endpoint}"): [
                {"body": pull(8, "open", "Fix", &["bug"])},
                {"body": pull(8, "open", "Fix", &["bug", "ready"])},
            ],
            "PUT repos/team/project/issues/8/labels": {"body": []},
        }));
        let action = json!({"item_action": {"workspace": "topic", "kind": "pr", "item": "8",
            "action": {"action": "edit", "add_labels": ["ready"]}}});
        let response = fixture.request(action);
        assert_eq!(response["type"], "item", "{response}");
        let url = format!(
            "{}/{}/8",
            fixture.web,
            if forge == Forge::GitHub {
                "pull"
            } else {
                "pulls"
            }
        );
        assert_eq!(
            response["data"],
            json!({"kind": "pr", "number": 8, "url": url, "title": "Fix", "state": "open",
                "labels": ["bug", "ready"], "assignees": [], "milestone": null,
                "pr": {"head": "topic", "base": "main", "draft": false}})
        );
        let auth = match forge {
            Forge::GitHub => Value::Null,
            Forge::Forgejo => json!("token usertoken"),
        };
        assert_eq!(
            fixture.requests(),
            [
                json!([format!("GET {endpoint}"), null, auth]),
                json!(["PUT repos/team/project/issues/8/labels", {"labels": ["bug", "ready"]}, auth]),
                json!([format!("GET {endpoint}"), null, auth]),
            ]
        );
        let event = fixture
            .events()
            .into_iter()
            .find(|event| event["kind"] == "item_changed")
            .unwrap();
        assert_eq!(
            event["item"],
            json!({"kind": "pr", "url": url, "action": "edit"})
        );

        // The forge's own refusal reaches the caller.
        fixture.respond(json!({
            format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
            "POST repos/team/project/issues/8/comments": {"status": 403, "body": {"message": "no write access"}},
        }));
        let refused = fixture.request(json!({"item_action": {"workspace": "topic", "kind": "pr",
            "item": "8", "action": {"action": "comment", "body": "hi"}}}));
        assert_eq!(refused["type"], "error");
        assert!(
            refused["data"]["message"]
                .as_str()
                .unwrap()
                .contains("no write access"),
            "{refused}"
        );
    }
}

#[test]
fn an_added_exclusive_label_replaces_the_one_of_its_scope() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let endpoint = "GET repos/team/project/pulls/8";
    let labels = "GET repos/team/project/labels?limit=50&page=1";
    let put = "PUT repos/team/project/issues/8/labels";
    let edit = |add: &str| {
        fixture.request(
            json!({"item_action": {"workspace": "topic", "kind": "pr", "item": "8",
            "action": {"action": "edit", "add_labels": [add]}}}),
        )
    };
    fixture.respond(json!({
        endpoint: [
            {"body": pull(8, "open", "Fix", &["bug", "complexity/small"])},
            {"body": pull(8, "open", "Fix", &["bug", "complexity/medium"])},
        ],
        labels: {"body": [
            {"name": "bug"},
            {"name": "complexity/small", "exclusive": true},
            {"name": "complexity/medium", "exclusive": true},
        ]},
        put: {"body": []},
    }));
    let response = edit("complexity/medium");
    assert_eq!(response["type"], "item", "{response}");
    let keys: Vec<_> = fixture
        .requests()
        .into_iter()
        .map(|request| (request[0].clone(), request[1].clone()))
        .collect();
    assert!(keys.iter().any(|(key, _)| key == labels), "{keys:?}");
    assert!(
        keys.contains(&(json!(put), json!({"labels": ["bug", "complexity/medium"]}))),
        "{keys:?}"
    );

    // A label the forge leaves out fails the edit, naming the item's labels.
    fixture.respond(json!({
        endpoint: {"body": pull(8, "open", "Fix", &["bug"])},
        put: {"body": []},
    }));
    let response = edit("missing");
    assert_eq!(response["type"], "error", "{response}");
    let message = response["data"]["message"].as_str().unwrap();
    assert!(
        message.contains("the forge did not add missing on #8, which has the labels bug"),
        "{message}"
    );
    // An item without a label of the added one's scope lists no labels.
    assert!(
        fixture
            .requests()
            .iter()
            .all(|request| request[0] != labels)
    );
}

/// Forgejo's answer to a list of the teams of a repository a user owns.
fn user_owned() -> Value {
    json!({"status": 405, "body": {"message": "repo is not owned by an organization"}})
}

#[test]
fn forgejo_lists_organization_labels_and_reviewers_except_the_author() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let mut pr = pull(8, "open", "Fix", &[]);
    pr["user"] = json!({"login": "sam"});
    let organization = "GET orgs/team";
    let teams = "GET repos/team/project/teams";
    let mut responses = json!({
        "GET repos/team/project/labels?limit=50&page=1": {"body": [{"name": "bug"}]},
        "GET orgs/team/labels?limit=50&page=1":
            {"body": [{"name": "priority/high", "exclusive": true}]},
        organization: {"body": {"username": "team"}},
        teams: {"body": [{"name": "core"}]},
        "GET repos/team/project/reviewers": {"body": [
            {"login": "kim", "full_name": "Kim Lee"},
            {"login": "sam", "full_name": ""},
        ]},
        "GET repos/team/project/pulls/8": {"body": pr},
    });
    fixture.respond(responses.clone());
    let labels = fixture.ok(&["repo", "labels", "--workspace", "topic"]);
    assert_eq!(
        labels,
        json!([
            {"name": "bug", "description": "", "exclusive": false},
            {"name": "priority/high", "description": "", "exclusive": true},
        ])
    );
    let reviewers = fixture.ok(&["pr", "reviewers", "8", "--workspace", "topic"]);
    assert_eq!(
        reviewers,
        json!([
            {"name": "kim", "display_name": "Kim Lee", "team": false},
            {"name": "team/core", "display_name": "", "team": true},
        ])
    );

    // Without a login, Forgejo shows no organization for a user, and refuses
    // the repository's teams.
    responses[organization] = json!({"status": 404, "body": {"message": "GetOrgByName"}});
    responses[teams] = json!({"status": 401, "body": {"message": "token is required"}});
    fixture.respond(responses.clone());
    let labels = fixture.ok(&["repo", "labels", "--workspace", "topic"]);
    assert_eq!(labels.as_array().map(Vec::len), Some(1), "{labels}");

    // A token without the read:organization scope learns that a user owns the
    // repository from its teams, which Forgejo then refuses to list.
    responses[organization] = json!({"status": 403, "body": {"message": "token scope"}});
    responses[teams] = user_owned();
    fixture.respond(responses);
    let labels = fixture.ok(&["repo", "labels", "--workspace", "topic"]);
    assert_eq!(labels.as_array().map(Vec::len), Some(1), "{labels}");
    let reviewers = fixture.ok(&["pr", "reviewers", "8", "--workspace", "topic"]);
    assert_eq!(
        reviewers,
        json!([{"name": "kim", "display_name": "Kim Lee", "team": false}])
    );

    // `--repo` reads another repository's.
    fixture.respond(json!({
        "GET repos/ann/tool/labels?limit=50&page=1": {"body": [{"name": "type/bug"}]},
        "GET orgs/ann": {"status": 404, "body": {"message": "GetOrgByName"}},
    }));
    let labels = fixture.ok(&[
        "repo",
        "labels",
        "--repo",
        "ann/tool",
        "--workspace",
        "topic",
    ]);
    assert_eq!(
        labels,
        json!([{"name": "type/bug", "description": "", "exclusive": false}])
    );
}

#[test]
fn github_lists_assignable_users_and_an_organizations_teams_as_reviewers() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let mut pr = pull(8, "open", "Fix", &[]);
    pr["user"] = json!({"login": "sam"});
    fixture.respond(json!({
        "GET repos/team/project": {"body": {"owner": {"login": "team", "type": "Organization"}}},
        "GET repos/team/project/assignees?per_page=100&page=1":
            {"body": [{"login": "kim"}, {"login": "sam"}]},
        "GET repos/team/project/teams?per_page=100&page=1":
            {"body": [{"slug": "core", "name": "Core"}]},
        "GET repos/team/project/pulls/8": {"body": pr},
    }));
    let reviewers = fixture.ok(&["pr", "reviewers", "8", "--workspace", "topic"]);
    assert_eq!(
        reviewers,
        json!([
            {"name": "kim", "display_name": "", "team": false},
            {"name": "team/core", "display_name": "Core", "team": true},
        ])
    );
}

/// Add the label `ready` to PR 8 of the workspace's repository.
fn label_pr(fixture: &Fixture) -> Value {
    fixture.request(
        json!({"item_action": {"workspace": "topic", "kind": "pr", "item": "8",
        "action": {"action": "edit", "add_labels": ["ready"]}}}),
    )
}

#[test]
fn unknown_hosts_are_asked_once_which_forge_they_serve() {
    // A token gh reads for every Enterprise host stays out of the probe.
    let fixture =
        Fixture::with_daemon_env(Forge::GitHub, "", &[("GH_ENTERPRISE_TOKEN", "enterprise")]);
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "git@ghe.invalid:team/project.git",
        ],
    );
    let endpoint = "repos/team/project/pulls/8";
    fixture.respond(json!({
        "GET meta": {"body": {"installed_version": "3.14.0"}},
        "GET repos/team/project/labels?per_page=100&page=1": {"body": [{"name": "bug"}]},
        format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &["bug", "ready"])},
        "PUT repos/team/project/issues/8/labels": {"body": []},
    }));
    let keys = |fixture: &Fixture| -> Vec<Value> {
        fixture
            .requests()
            .into_iter()
            .map(|request| request[0].clone())
            .collect()
    };
    // The CLI asks the daemon, which asks the host and records its answer.
    for _ in 0..2 {
        let labels = fixture.ok(&["repo", "labels", "--workspace", "topic"]);
        assert_eq!(labels[0]["name"], "bug");
    }
    let label_list = "GET repos/team/project/labels?per_page=100&page=1";
    assert_eq!(
        fixture.requests(),
        [
            json!(["GET meta", null, null]),
            json!([label_list, null, null]),
            json!([label_list, null, null]),
        ]
    );
    let response = label_pr(&fixture);
    assert_eq!(response["type"], "item", "{response}");
    assert_eq!(
        response["data"]["url"],
        "https://ghe.invalid/team/project/pull/8"
    );
    // Once the host is known to be GitHub, gh's own requests use the token.
    let requests = fixture.requests();
    assert!(requests.iter().all(|request| request[0] != "GET meta"));
    assert_eq!(requests[0][2], "enterprise", "{requests:?}");
    // A pasted link of the host names its forge before anything is asked.
    let url = "https://ghe.invalid/team/project/pull/8";
    let linked = fixture.ok(&["link", url, "related", "--workspace", "topic"]);
    assert_eq!(linked["registered"], true, "{linked}");
    keys(&fixture);

    // Each reload asks the host again, also when it cannot answer, which
    // keeps the forge it answered before.
    // Reading the log deletes it, so it is cleared once before each reload and
    // then only read until the request appears.
    let log = fixture.root.path().join("forge/requests.jsonl");
    let asked = || fs::read_to_string(&log).is_ok_and(|text| text.contains("GET meta"));
    keys(&fixture);
    fixture.ok(&["daemon", "reload"]);
    support::wait::wait_until("the host asked again", asked);
    fixture.respond(json!({}));
    keys(&fixture);
    fixture.ok(&["daemon", "reload"]);
    support::wait::wait_until("the host asked again", asked);
}

#[test]
fn pinned_hosts_are_not_asked_and_unrecognized_ones_name_the_setting() {
    let fixture = Fixture::new(
        Forge::GitHub,
        "[machine.forges.\"GHE.invalid\"]\ntype = \"github\"\n\
         [machine.forges.\"gitlab.invalid\"]\ntype = \"github\"\n",
    );
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "git@ghe.invalid:team/project.git",
        ],
    );
    fixture.respond(json!({
        "GET repos/team/project/pulls/8": {"body": pull(8, "open", "Fix", &["bug", "ready"])},
        "PUT repos/team/project/issues/8/labels": {"body": []},
    }));
    assert_eq!(label_pr(&fixture)["type"], "item");
    assert!(
        fixture
            .requests()
            .iter()
            .all(|request| request[0] != "GET meta")
    );

    // A pin outranks the name guess for a pasted link, too.
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "git@gitlab.invalid:team/project.git",
        ],
    );
    let url = "https://gitlab.invalid/team/project/issues/7";
    let linked = fixture.ok(&["link", url, "related", "--workspace", "topic"]);
    assert_eq!(linked["registered"], true, "{linked}");

    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "git@unknown.invalid:team/project.git",
        ],
    );
    let refused = label_pr(&fixture);
    assert_eq!(refused["type"], "error");
    let message = refused["data"]["message"].as_str().unwrap();
    assert!(
        message.contains("unknown.invalid answers as none of")
            && message.contains("[machine.forges.\"unknown.invalid\"]"),
        "{message}"
    );
}

#[test]
fn changes_use_the_agent_account_and_merges_the_users() {
    for forge in [Forge::GitHub, Forge::Forgejo] {
        let config =
            format!("[agent.forge_clis]\ngh = '~/agent-gh'\nfj = '~/agent-fj'\n{AGENT_TOKEN}");
        let fixture = Fixture::new(forge, &config);
        let root = fixture.root.path();
        for (name, body) in [
            (
                "agent-gh",
                "AGENT=agent exec python3 \"$HOME/forge.py\" \"$@\"",
            ),
            ("agent-fj", "exit 1"),
        ] {
            support::install_test_script(root.join(name), format!("#!/bin/sh\n{body}\n"));
        }
        let (agent, user) = match forge {
            Forge::GitHub => (json!("agent"), Value::Null),
            Forge::Forgejo => (json!("token agenttoken"), json!("token usertoken")),
        };
        let endpoint = "repos/team/project/pulls/8";
        fixture.respond(json!({
            format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
            "POST repos/team/project/issues/8/comments": {"body": {}},
            format!("PUT {endpoint}/merge"): {"body": {"merged": true}},
            format!("POST {endpoint}/merge"): {"body": {}},
            format!("DELETE {endpoint}/merge"): {"body": {}},
            "POST graphql": {"body": {"data": {}}},
        }));
        let act = |action: Value| {
            fixture.request(json!({"item_action": {"workspace": "topic", "kind": "pr",
                "item": "8", "action": action}}))
        };
        assert_eq!(
            act(json!({"action": "comment", "body": "hi"}))["type"],
            "item"
        );
        let merged = act(json!({"action": "merge", "method": "rebase", "delete_branch": false}));
        assert_eq!(merged["type"], "item", "{merged}");
        let cancelled = act(json!({"action": "cancel_auto_merge"}));
        assert_eq!(cancelled["type"], "item", "{cancelled}");
        let auth: Vec<_> = fixture
            .requests()
            .into_iter()
            .filter(|request| !request[0].as_str().unwrap().starts_with("GET"))
            .map(|request| request[2].clone())
            .collect();
        assert_eq!(auth, [agent, user.clone(), user]);
    }

    // With an fj wrapper but no agent token, the user's token does not stand in.
    let fixture = Fixture::new(Forge::Forgejo, "[agent.forge_clis]\nfj = '~/agent-fj'\n");
    support::install_test_script(fixture.root.path().join("agent-fj"), "#!/bin/sh\n");
    let refused = fixture.request(json!({"item_action": {"workspace": "topic", "kind": "pr",
        "item": "8", "action": {"action": "close"}}}));
    assert!(
        refused["data"]["message"]
            .as_str()
            .unwrap()
            .contains("set its agent_token under [machine.forges."),
        "{refused}"
    );
    assert!(fixture.requests().is_empty());
}

#[test]
fn pr_update_takes_in_the_base_on_the_forge_for_workspace_processes_too() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let endpoint = "repos/team/project/pulls/8";
    fixture.respond(json!({
        format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
        format!("POST {endpoint}/update?style=rebase"): {"body": null},
        format!("POST {endpoint}/update?style=merge"): {"body": null},
    }));
    let updated = fixture.ok(&[
        "pr",
        "update",
        "8",
        "--workspace",
        "topic",
        "--method",
        "rebase",
    ]);
    assert_eq!(
        updated["url"],
        format!("{}/pulls/8", fixture.web),
        "{updated}"
    );
    let shoal = env!("CARGO_BIN_EXE_shoal");
    let scoped = fixture.run(&[
        "run",
        "--workspace",
        "topic",
        "--",
        shoal,
        "pr",
        "update",
        "8",
    ]);
    assert!(scoped.status.success(), "{scoped:?}");
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| request[0].as_str().unwrap().starts_with("POST"))
        .map(|request| request[0].clone())
        .collect();
    assert_eq!(
        sent,
        [
            json!(format!("POST {endpoint}/update?style=rebase")),
            json!(format!("POST {endpoint}/update?style=merge")),
        ]
    );
}

#[test]
fn reviews_comment_as_the_agent_and_leave_verdicts_to_the_user() {
    let fixture = Fixture::new(Forge::Forgejo, AGENT_TOKEN);
    let mut pull = pull(8, "open", "Fix", &[]);
    pull["head"]["sha"] = json!("c0ffee");
    let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
                @@ -1,2 +1,3 @@\n a\n+b\n c\n";
    fixture.respond(json!({
        "GET repos/team/project/pulls/8": {"body": pull},
        "GET repos/team/project/pulls/8.diff": {"body": diff},
        "POST repos/team/project/pulls/8/reviews": {"body": {"id": 5}},
    }));
    let scope = fixture.ok(&["work", "env", "topic"])["SHOAL_SCOPE_TOKEN"]
        .as_str()
        .unwrap()
        .to_owned();
    let review = |verdict: &str, comments: Value| {
        json!({"item_action": {"workspace": "topic", "kind": "pr", "item": "8",
            "action": {"action": "review", "verdict": verdict, "body": "Close",
                "comments": comments}}})
    };
    let posts = || -> Vec<Value> {
        fixture
            .requests()
            .into_iter()
            .filter(|request| request[0].as_str().unwrap().starts_with("POST"))
            .collect()
    };

    let line = json!([{"path": "src/a.rs", "line": 2, "body": "Why b?"}]);
    let commented = fixture.request_as(review("comment", line), Some(&scope));
    assert_eq!(commented["type"], "item", "{commented}");
    assert_eq!(
        posts(),
        [json!(["POST repos/team/project/pulls/8/reviews",
            {"event": "COMMENT", "body": "Close", "commit_id": "c0ffee",
                "comments": [{"path": "src/a.rs", "new_position": 2, "old_position": 0,
                    "body": "Why b?"}]},
            "token agenttoken"])]
    );

    // A comment off the diff refuses the whole review before anything is posted.
    let off = json!([{"path": "src/a.rs", "line": 2, "body": "Fine"},
        {"path": "src/a.rs", "line": 9, "body": "Why?"}]);
    let refused = fixture.request_as(review("comment", off), Some(&scope));
    let message = refused["data"]["message"].as_str().unwrap();
    assert!(message.contains("src/a.rs:9: no hunk"), "{refused}");
    assert!(posts().is_empty());

    // Verdicts are the user's: refused for the agent, posted as the user otherwise.
    let scoped = fixture.request_as(review("approve", json!([])), Some(&scope));
    let message = scoped["data"]["message"].as_str().unwrap();
    assert!(message.contains("the user's decision"), "{scoped}");
    assert!(fixture.requests().is_empty());
    let approved = fixture.request(review("approve", json!([])));
    assert_eq!(approved["type"], "item", "{approved}");
    let posted = posts();
    assert_eq!(posted[0][1]["event"], "APPROVED");
    assert_eq!(posted[0][2], "token usertoken");
}

#[test]
fn pr_review_takes_line_comments_from_arguments_and_a_file_and_watches_skip_it() {
    let fixture = Fixture::new(Forge::Forgejo, "[cleanup.auto]\nenabled=false\n");
    let pull = |state: &str| {
        let mut pull = pull(8, state, "Fix", &[]);
        pull["head"]["sha"] = json!("head");
        json!({"body": pull})
    };
    let reviews = |ids: &[u64]| {
        json!({"body": ids.iter().map(|id| json!({"id": id, "body": "", "state": "COMMENT",
            "comments_count": 2, "user": {"login": "dev"}})).collect::<Vec<_>>()})
    };
    let responses = |state: &str, reviewed: &[u64]| {
        json!({
            "GET repos/team/project/pulls/8": pull(state),
            "GET repos/team/project/issues/8/comments?limit=50&page=1":
                {"body": [{"id": 1, "body": "First"}]},
            "GET repos/team/project/pulls/8/reviews?limit=50&page=1": reviews(reviewed),
            "GET repos/team/project/commits/head/status?limit=50":
                {"body": {"total_count": 0, "statuses": []}},
            "GET repos/team/project/pulls/8.diff": {"body": "diff --git a/src/a.rs b/src/a.rs\n\
                --- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,3 @@\n a\n+b\n c\n"},
            "POST repos/team/project/pulls/8/reviews": {"body": {"id": 5}},
        })
    };
    fixture.respond(responses("open", &[]));
    fixture.ok(&["link", "pr", "8", "assigned", "--workspace", "topic"]);
    let first = fixture.ok(&["watch", "pr", "--workspace", "topic"]);
    assert_eq!(first["updates"][0]["kind"], "comment", "{first}");
    fixture.requests();

    let unjudged = fixture.run(&["pr", "review", "--workspace", "topic", "--body", "Hm"]);
    assert!(!unjudged.status.success());
    assert!(String::from_utf8_lossy(&unjudged.stderr).contains("--comment"));
    let file = fixture.root.path().join("comments.json");
    fs::write(
        &file,
        r#"[{"path": "src/a.rs", "line": 1, "side": "old", "body": "Was a"}]"#,
    )
    .unwrap();
    let reviewed = fixture.ok(&[
        "pr",
        "review",
        "--workspace",
        "topic",
        "--comment",
        "--line",
        "src/a.rs:2",
        "Why b?",
        "--comments-file",
        file.to_str().unwrap(),
    ]);
    assert_eq!(reviewed["number"], 8, "{reviewed}");
    let posted: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| request[0] == "POST repos/team/project/pulls/8/reviews")
        .collect();
    assert_eq!(
        posted[0][1]["comments"],
        json!([
            {"path": "src/a.rs", "new_position": 2, "old_position": 0, "body": "Why b?"},
            {"path": "src/a.rs", "new_position": 0, "old_position": 1, "body": "Was a"},
        ])
    );

    // The agent's own review is not reported back beside the closing.
    fixture.respond(responses("closed", &[5]));
    let closed = fixture.ok(&["watch", "pr", "--workspace", "topic"]);
    let kinds: Vec<_> = closed["updates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|update| update["kind"].clone())
        .collect();
    assert_eq!(kinds, ["closed"], "{closed}");
}

#[test]
fn watches_do_not_report_a_forgejo_reviewer_request_as_a_review() {
    let fixture = Fixture::new(Forge::Forgejo, "[cleanup.auto]\nenabled=false\n");
    let responses = |state: &str, reviews: Value| {
        let mut pull = pull(8, state, "Fix", &[]);
        pull["head"]["sha"] = json!("head");
        json!({
            "GET repos/team/project/pulls/8": {"body": pull},
            "GET repos/team/project/issues/8/comments?limit=50&page=1":
                {"body": [{"id": 1, "body": "First"}]},
            "GET repos/team/project/pulls/8/reviews?limit=50&page=1": {"body": reviews},
            "GET repos/team/project/commits/head/status?limit=50":
                {"body": {"total_count": 0, "statuses": []}},
            "POST repos/team/project/pulls/8/requested_reviewers": {"status": 201, "body": []},
        })
    };
    fixture.respond(responses("open", json!([])));
    fixture.ok(&["link", "pr", "8", "assigned", "--workspace", "topic"]);
    let first = fixture.ok(&["watch", "pr", "--workspace", "topic"]);
    assert_eq!(first["updates"][0]["kind"], "comment", "{first}");

    fixture.ok(&[
        "pr",
        "edit",
        "--workspace",
        "topic",
        "--add-reviewer",
        "lead",
    ]);
    // Forgejo records the request as a review with no body or comments.
    let request = json!([{"id": 3, "body": "", "state": "REQUEST_REVIEW",
        "comments_count": 0, "user": {"login": "lead"}}]);
    fixture.respond(responses("closed", request));
    let closed = fixture.ok(&["watch", "pr", "--workspace", "topic"]);
    let kinds: Vec<_> = closed["updates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|update| update["kind"].clone())
        .collect();
    assert_eq!(kinds, ["closed"], "{closed}");
}

#[test]
fn review_threads_take_replies_and_ask_before_resolving_without_one() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let endpoint = "repos/team/project/pulls/8";
    let threads = |comments: Value| {
        json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": false, "endCursor": null},
            "nodes": [{"id": "T_1", "isResolved": false, "comments": {"nodes": comments}}]}}}}})
    };
    fixture.respond(json!({
        format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
        "POST graphql": {"body": threads(json!([{"databaseId": 5}]))},
        format!("POST {endpoint}/comments/5/replies"): {"body": {}},
    }));
    let pr = ["--workspace", "topic", "8"];

    // Without a reply, a non-interactive resolve asks for one or for --yes.
    let unanswered = fixture.run(&[&["pr", "resolve", "5"][..], &pr].concat());
    let stderr = String::from_utf8_lossy(&unanswered.stderr);
    assert!(!unanswered.status.success());
    assert!(
        stderr.contains("Thread 5 has no reply") && stderr.contains("--yes"),
        "{stderr}"
    );
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    assert_eq!(sent, ["POST graphql"]);

    // A reply goes to the thread's first comment, before the resolution.
    fixture.ok(&[&["pr", "resolve", "5", "--body", "Fixed"][..], &pr].concat());
    let sent = fixture.requests();
    assert_eq!(
        sent[2],
        json!([format!("POST {endpoint}/comments/5/replies"), {"body": "Fixed"}, null])
    );
    assert_eq!(sent[3][0], "POST graphql");
    assert!(
        sent[3][1]["query"]
            .as_str()
            .unwrap()
            .contains("resolveReviewThread")
    );

    // A thread someone answered resolves without asking.
    fixture.respond(json!({
        format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
        "POST graphql": {"body": threads(json!([{"databaseId": 5}, {"databaseId": 6}]))},
    }));
    fixture.ok(&[&["pr", "resolve", "6"][..], &pr].concat());
    fixture.requests();

    // Comments past a thread's first page are read from the thread itself.
    let first_page = threads(json!([{"databaseId": 5}]));
    let mut first_page_more = first_page.clone();
    first_page_more["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0]["comments"]
        ["pageInfo"] = json!({"hasNextPage": true, "endCursor": "c1"});
    fixture.respond(json!({
        format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
        "POST graphql": [{"body": first_page_more}, {"body": {"data": {"node": {"comments": {
            "pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": [{"databaseId": 6}]}}}}}],
    }));
    fixture.ok(&[&["pr", "resolve", "6", "--yes"][..], &pr].concat());
    let sent = fixture.requests();
    assert_eq!(sent[2][1]["variables"], json!({"id": "T_1", "after": "c1"}));
    assert!(
        sent[3][1]["query"]
            .as_str()
            .unwrap()
            .contains("resolveReviewThread")
    );

    let nothing = fixture.run(&[&["pr", "reply", "7"][..], &pr].concat());
    assert!(String::from_utf8_lossy(&nothing.stderr).contains("a reply needs --body"));
}

#[test]
fn forgejo_threads_take_replies_but_cannot_be_resolved() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let endpoint = "repos/team/project/pulls/8";
    fixture.respond(json!({
        format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
        format!("GET {endpoint}/reviews?limit=50&page=1"): {"body": [{"id": 3, "comments_count": 1}]},
        format!("GET {endpoint}/reviews/3/comments"): {"body": [{"id": 5, "path": "a.rs",
            "position": 4, "original_position": 0, "resolver": null}]},
        format!("POST {endpoint}/reviews/3/comments"): {"body": {}},
    }));
    let pr = ["--workspace", "topic", "8"];
    fixture.ok(&[&["pr", "reply", "5", "--body", "Kept"][..], &pr].concat());
    assert!(fixture.requests().contains(&json!([
        format!("POST {endpoint}/reviews/3/comments"),
        {"body": "Kept", "path": "a.rs", "new_position": 4, "extra_lines_count": 0},
        "token usertoken"
    ])));
    let refused = fixture.run(&[&["pr", "resolve", "5", "--body", "Kept"][..], &pr].concat());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("Shoal cannot resolve review threads on Forgejo or Gitea"),
        "{refused:?}"
    );
    assert!(
        fixture
            .requests()
            .iter()
            .all(|request| request[0].as_str().unwrap().starts_with("GET")),
    );
    let reported = fixture.ok(&["forge", "capabilities", "--workspace", "topic"]);
    assert_eq!(reported["forge"], "Forgejo or Gitea", "{reported}");
    let capabilities = reported["capabilities"].as_array().unwrap();
    assert!(
        capabilities.contains(&json!({"capability": "resolve_threads",
        "supported": false, "hint": "reply in the thread instead"}))
    );
    assert!(capabilities.contains(&json!({"capability": "cancel_jobs", "supported": true})));
    assert!(fixture.requests().is_empty());
}

#[test]
fn forgejo_views_keep_reviews_whose_comments_fail_and_retry_busy_lookups() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let endpoint = "repos/team/project/pulls/8";
    let review = |id: u64, body: &str| {
        json!({"id": id, "comments_count": 1, "state": "COMMENT", "body": body,
            "user": {"login": "bot"}, "submitted_at": "t"})
    };
    let comment = json!([{"id": 5, "path": "a.rs", "position": 4, "original_position": 0,
        "body": "Nit", "user": {"login": "bot"}, "resolver": null}]);
    fixture.respond(json!({
        format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
        "GET repos/team/project/issues/8/comments": {"body": []},
        "GET repos/team/project/issues/8/timeline?limit=50&page=1":
            {"body": [{"type": "pull_scheduled_merge"}]},
        format!("GET {endpoint}/reviews?limit=50&page=1"):
            {"body": [review(3, "Retried"), review(4, "Unreadable")]},
        format!("GET {endpoint}/reviews/3/comments"):
            [{"status": 503, "body": {"message": "busy"}}, {"body": comment}],
        format!("GET {endpoint}/reviews/4/comments"): {"status": 403, "body": {"message": "no"}},
    }));
    let view = &fixture.ok(&["view", "pr", "8", "--workspace", "topic"])[0];
    assert_eq!(view["pr"]["auto_merge"], json!({"method": null}), "{view}");
    let reviews = view["reviews"].as_array().unwrap();
    assert_eq!(reviews[0]["comments"][0]["body"], "Nit", "{view}");
    assert_eq!(reviews[1]["body"], "Unreadable", "{view}");
    let errors: Vec<&str> = view["errors"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|error| error.as_str().filter(|error| error.contains("comments")))
        .collect();
    assert_eq!(errors.len(), 1, "{view}");
    assert!(
        errors[0].starts_with("comments lookup failed for review 4: GET "),
        "{view}"
    );
}

#[test]
fn pr_views_count_the_base_commits_the_head_lacks() {
    for forge in [Forge::GitHub, Forge::Forgejo] {
        let fixture = Fixture::new(forge, "");
        let mut open = pull(8, "open", "Fix", &[]);
        open["head"]["sha"] = json!("c0ffee");
        let (compare, count) = match forge {
            Forge::GitHub => (
                "GET repos/team/project/compare/release/2...c0ffee?per_page=1",
                json!({"behind_by": 3, "ahead_by": 1}),
            ),
            Forge::Forgejo => (
                "GET repos/team/project/compare/c0ffee...release/2?files=false&stat=false&verification=false",
                json!({"total_commits": 3}),
            ),
        };
        open["base"]["ref"] = json!("release/2");
        fixture.respond(json!({
            "GET repos/team/project/pulls/8": {"body": open},
            "pr view 8": {"body": {"number": 8, "state": "OPEN", "title": "Fix", "body": "",
                "author": {"login": "ann"}, "labels": [], "createdAt": "t0",
                "headRefName": "topic", "baseRefName": "release/2", "isDraft": false}},
            compare: {"body": count},
            "GET repos/team/project/issues/8/timeline?limit=50&page=1": {"body": []},
        }));
        let args = ["view", "pr", "8", "--workspace", "topic", "--no-comments"];
        let view = &fixture.ok(&args)[0];
        assert_eq!(view["pr"]["behind"], 3, "{view}");
        let text = fixture.run(&args);
        let text = String::from_utf8_lossy(&text.stdout);
        assert!(
            text.contains("Branch:   topic → release/2, 3 commits behind"),
            "{text}"
        );

        // A closed PR is not compared.
        fixture.respond(json!({
            "GET repos/team/project/pulls/8": {"body": pull(8, "closed", "Fix", &[])},
            "pr view 8": {"body": {"number": 8, "state": "CLOSED", "title": "Fix", "body": "",
                "author": {"login": "ann"}, "labels": [], "createdAt": "t0",
                "headRefName": "topic", "baseRefName": "main", "isDraft": false}},
            "GET repos/team/project/issues/8/timeline?limit=50&page=1": {"body": []},
        }));
        fixture.requests();
        let view = &fixture.ok(&args)[0];
        assert!(view["pr"].get("behind").is_none(), "{view}");
        assert!(
            !fixture
                .requests()
                .iter()
                .any(|request| request[0].as_str().unwrap().contains("compare")),
        );
    }

    // GitLab counts them on the merge request when asked.
    let fixture = Fixture::new(Forge::GitHub, "");
    let root = fixture.root.path();
    std::os::unix::fs::symlink(root.join("forge.py"), root.join("bin/glab")).unwrap();
    let remote = "git@gitlab.com:team/project.git";
    git(&fixture.repo, &["remote", "set-url", "origin", remote]);
    let request = json!({"iid": 8, "title": "Fix", "state": "opened", "description": "",
        "labels": [], "source_branch": "topic", "target_branch": "main", "draft": false,
        "sha": "abc", "author": {"username": "ann"}, "created_at": "t0",
        "source_project_id": 1, "target_project_id": 1});
    let mut counted = request.clone();
    counted["diverged_commits_count"] = json!(2);
    let endpoint = "projects/team%2Fproject/merge_requests/8";
    fixture.respond(json!({
        format!("GET {endpoint}"): {"body": request},
        format!("GET {endpoint}?include_diverged_commits_count=true"): {"body": counted},
    }));
    let args = ["view", "pr", "8", "--workspace", "topic", "--no-comments"];
    let view = &fixture.ok(&args)[0];
    assert_eq!(view["pr"]["behind"], 2, "{view}");
}

#[test]
fn pr_views_list_the_reviewers() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let mut closed = pull(8, "closed", "Fix", &[]);
    closed["requested_reviewers"] = json!([{"login": "kim"}]);
    closed["requested_reviewers_teams"] = json!([{"name": "core"}]);
    fixture.respond(json!({"GET repos/team/project/pulls/8": {"body": closed}}));
    let args = ["view", "pr", "8", "--workspace", "topic", "--no-comments"];
    let view = &fixture.ok(&args)[0];
    assert_eq!(
        view["pr"]["reviewers"],
        json!(["kim", "team/core"]),
        "{view}"
    );
    let text = fixture.run(&args);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Reviewers: kim, team/core"), "{text}");
}

#[test]
fn views_count_reactions_and_mark_the_logins_own() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let issue = "repos/team/project/issues/3";
    let reaction =
        |content: &str, login: &str| json!({"content": content, "user": {"login": login}});
    fixture.respond(json!({
        format!("GET {issue}"): {"body": {"number": 3, "title": "Crash", "state": "open",
            "labels": [], "user": {"login": "ann"}}},
        format!("GET {issue}/comments"): {"body": [{"id": 9, "body": "Same", "user": {"login": "bob"}}]},
        "GET user": {"body": {"login": "me"}},
        format!("GET {issue}/reactions?limit=50&page=1"):
            {"body": [reaction("+1", "ann"), reaction("heart", "ann"), reaction("+1", "me")]},
        // Forgejo lists a comment's reactions at once, however many.
        "GET repos/team/project/issues/comments/9/reactions":
            {"body": vec![reaction("eyes", "ann"); 50]},
    }));
    let view = &fixture.ok(&["view", "issue", "3", "--workspace", "topic"])[0];
    assert_eq!(
        view["reactions"],
        json!([{"content": "+1", "count": 2, "reacted": true},
            {"content": "heart", "count": 1, "reacted": false}]),
        "{view}"
    );
    assert_eq!(view["comments"][0]["id"], 9, "{view}");
    assert_eq!(
        view["comments"][0]["reactions"],
        json!([{"content": "eyes", "count": 50, "reacted": false}]),
        "{view}"
    );
    assert!(view.get("errors").is_none(), "{view}");
    let text = fixture.run(&["view", "issue", "3", "--workspace", "topic"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Reactions: +1 2 (you), heart 1"), "{text}");
    assert!(text.contains("comment 9"), "{text}");

    // Without a token, a public repository's reactions are still listed.
    fixture.write_config("");
    let view = &fixture.ok(&["view", "issue", "3", "--workspace", "topic"])[0];
    assert_eq!(view["reactions"][0]["reacted"], false, "{view}");
    assert!(view.get("errors").is_none(), "{view}");
    assert!(
        !fixture
            .requests()
            .iter()
            .any(|request| request[0] == "GET user" && request[2].is_null()),
    );
}

#[test]
fn views_read_another_repositorys_item_by_its_url() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let issue = "repos/team/tool/issues/5";
    fixture.respond(json!({
        format!("GET {issue}"): {"body": {"number": 5, "title": "Crash", "state": "open",
            "labels": [], "user": {"login": "ann"}}},
        format!("GET {issue}/comments"): {"body": [{"id": 9, "body": "Same", "user": {"login": "bob"}}]},
        "GET user": {"body": {"login": "me"}},
        format!("GET {issue}/reactions?limit=50&page=1"): {"body": []},
        "GET repos/team/tool/issues/comments/9/reactions": {"body": []},
    }));
    let url = fixture.web.replace("team/project", "team/tool") + "/issues/5";
    let view = &fixture.ok(&["view", &url, "--workspace", "topic"])[0];
    assert_eq!(view["url"], url, "{view}");
    assert_eq!(view["title"], "Crash", "{view}");
    assert_eq!(view["comments"][0]["body"], "Same", "{view}");
    assert!(view.get("errors").is_none(), "{view}");
    assert_eq!(scoped(&fixture, &["view", &url])[0]["title"], "Crash");
    // Watching it stays limited to the workspace's repository.
    let watched = fixture.run(&[
        "watch",
        "issue",
        &url,
        "--workspace",
        "topic",
        "--timeout",
        "1",
    ]);
    assert!(
        String::from_utf8_lossy(&watched.stderr).contains("different repository"),
        "{watched:?}"
    );
}

#[test]
fn github_reactions_find_inline_comments_and_remove_by_id() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let threads = json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
        "pageInfo": {"hasNextPage": false, "endCursor": null},
        "nodes": [{"id": "T_1", "isResolved": false, "comments": {"nodes": [{"databaseId": 5}]}}]}}}}});
    let own =
        |id: u64, content: &str| json!({"id": id, "content": content, "user": {"login": "me"}});
    fixture.respond(json!({
        "GET repos/team/project/pulls/8": {"body": pull(8, "open", "Fix", &[])},
        "GET repos/team/project/issues/8": {"body": {"number": 8, "title": "Fix",
            "state": "open", "labels": [], "pull_request": {}}},
        "pr view 8": {"body": {"number": 8, "state": "OPEN", "title": "Fix", "body": "",
            "author": {"login": "ann"}, "labels": [], "createdAt": "t0",
            "headRefName": "topic", "baseRefName": "main", "isDraft": false,
            "comments": [{"author": {"login": "ann"}, "createdAt": "t1", "body": "Nice",
                "url": "https://github.com/team/project/pull/8#issuecomment-6"}]}},
        "GET repos/team/project/pulls/8/reviews": {"body": [[{"id": 11, "state": "COMMENTED",
            "user": {"login": "bot"}, "body": "", "submitted_at": "t2"}]]},
        "GET repos/team/project/pulls/8/comments": {"body": [[{"id": 5,
            "pull_request_review_id": 11, "user": {"login": "bot"}, "path": "a.rs",
            "line": 3, "body": "Why?", "created_at": "t2"}]]},
        "POST graphql": {"body": threads},
        "GET user": {"body": {"login": "me"}},
        "GET repos/team/project/pulls/comments/5/reactions?per_page=100&page=1": {"body": []},
        "POST repos/team/project/pulls/comments/5/reactions": {"status": 201, "body": own(7, "+1")},
        "GET repos/team/project/issues/comments/6/reactions?per_page=100&page=1":
            {"body": [own(3, "-1")]},
        "DELETE repos/team/project/issues/comments/6/reactions/3": {"status": 204},
        "GET repos/team/project/issues/8/reactions?per_page=100&page=1": {"body": [own(4, "heart")]},
    }));
    let pr = ["--workspace", "topic", "8"];
    let added = fixture.ok(&[&["pr", "react", "+1", "--comment", "5"][..], &pr].concat());
    assert_eq!(added["number"], 8, "{added}");
    fixture.ok(&[
        &["pr", "react", "-1", "--comment", "6", "--remove"][..],
        &pr,
    ]
    .concat());
    // A reaction that is already there is not sent again.
    fixture.ok(&[&["pr", "react", "heart"][..], &pr].concat());
    // Another item's comment, and a PR named as an issue, are refused
    // before anything is sent.
    let missing = fixture.run(&[&["pr", "react", "+1", "--comment", "99"][..], &pr].concat());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("#8 has no comment 99"),
        "{missing:?}"
    );
    let issue = ["issue", "react", "+1", "--workspace", "topic", "8"];
    assert!(!fixture.run(&issue).status.success());
    let mut writes = writes(&fixture);
    // gh reads the PR's comments with `pr view`, and its threads by GraphQL.
    writes.retain(|request| request[0] != "POST graphql" && request[0] != "pr view 8");
    assert_eq!(
        writes,
        [
            json!(["POST repos/team/project/pulls/comments/5/reactions", {"content": "+1"}]),
            json!([
                "DELETE repos/team/project/issues/comments/6/reactions/3",
                null
            ]),
        ]
    );
    let event = fixture
        .events()
        .into_iter()
        .rfind(|event| event["kind"] == "item_changed")
        .unwrap();
    assert_eq!(event["item"]["action"], "react", "{event}");
}

#[test]
fn forge_reactions_list_what_the_instance_allows_or_githubs() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    fixture.respond(json!({"GET settings/ui": {"body": {"allowed_reactions": ["+1", "heart"]}}}));
    let reactions = fixture.ok(&["forge", "reactions", "--workspace", "topic"]);
    assert_eq!(
        reactions,
        json!({"names": ["+1", "heart"], "accepts_other_names": false})
    );
    let text = fixture.run(&["forge", "reactions", "--workspace", "topic"]);
    assert_eq!(String::from_utf8_lossy(&text.stdout), "+1\nheart\n");

    let fixture = Fixture::new(Forge::GitHub, "");
    let reactions = fixture.ok(&["forge", "reactions", "--workspace", "topic"]);
    assert_eq!(
        reactions["names"].as_array().map(Vec::len),
        Some(8),
        "{reactions}"
    );
    assert_eq!(reactions["accepts_other_names"], false, "{reactions}");
    assert!(fixture.requests().is_empty());
}

#[test]
fn forgejo_reactions_are_removed_by_content_and_left_when_already_as_asked() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let issue = "repos/team/project/issues/3";
    fixture.respond(json!({
        format!("GET {issue}"): {"body": {"number": 3, "title": "Crash", "state": "open",
            "labels": [], "user": {"login": "ann"}}},
        format!("GET {issue}/comments"): {"body": [{"id": 9, "body": "Same", "user": {"login": "bob"}}]},
        "GET user": {"body": {"login": "me"}},
        format!("GET {issue}/reactions?limit=50&page=1"): {"body": null},
        format!("POST {issue}/reactions"): {"status": 201, "body": {"content": "eyes"}},
        "GET repos/team/project/issues/comments/9/reactions":
            {"body": [{"content": "heart", "user": {"login": "Me"}}]},
        "DELETE repos/team/project/issues/comments/9/reactions": {"body": null},
    }));
    let target = ["--workspace", "topic", "3"];
    fixture.ok(&[&["issue", "react", "eyes"][..], &target].concat());
    fixture.ok(&[
        &["issue", "react", "heart", "--comment", "9", "--remove"][..],
        &target,
    ]
    .concat());
    // Removing a reaction that is not there changes nothing.
    fixture.ok(&[&["issue", "react", "heart", "--remove"][..], &target].concat());
    assert_eq!(
        writes(&fixture),
        [
            json!([format!("POST {issue}/reactions"), {"content": "eyes"}]),
            json!(["DELETE repos/team/project/issues/comments/9/reactions", {"content": "heart"}]),
        ]
    );
    let refused = fixture.run(&[&["issue", "react", "two words"][..], &target].concat());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("name the reaction"),
        "{refused:?}"
    );
}

#[test]
fn comment_edits_change_only_the_logins_own_comments_and_are_not_reported_back() {
    let fixture = Fixture::new(Forge::Forgejo, "[cleanup.auto]\nenabled=false\n");
    let issue = |state: &str| {
        json!({"body": {"number": 8, "title": "Issue", "state": state, "body": "",
            "labels": [], "pull_request": null, "user": {"login": "ann"}}})
    };
    let comments = |own: &str| {
        json!({"body": [{"id": 1, "body": "Crashes", "user": {"login": "ann"}},
            {"id": 2, "body": own, "user": {"login": "me"}}]})
    };
    fixture.respond(json!({
        "GET repos/team/project/issues/8": issue("open"),
        "GET repos/team/project/issues/8/comments?limit=50&page=1": comments("Fixd"),
        // Views read the discussion at once.
        "GET repos/team/project/issues/8/comments": comments("Fixd"),
        "GET user": {"body": {"login": "me"}},
        "PATCH repos/team/project/issues/comments/2":
            {"body": {"id": 2, "body": "Fixed", "user": {"login": "me"}}},
    }));
    fixture.ok(&["link", "issue", "8", "assigned", "--workspace", "topic"]);
    fixture.ok(&["watch", "issue", "--workspace", "topic"]);
    let note = fixture.root.path().join("note.md");
    fs::write(&note, "Fixed").unwrap();
    let edit = |comment: &str| {
        let body = note.to_str().unwrap();
        let args = [
            "issue",
            "comment",
            "--workspace",
            "topic",
            "--edit",
            comment,
        ];
        fixture.run(&[&args[..], &["--body-file", body]].concat())
    };
    let edited = edit("2");
    assert!(edited.status.success(), "{edited:?}");
    // Someone else's comment, and another item's, are refused before
    // anything is sent.
    let others = edit("1");
    assert!(
        String::from_utf8_lossy(&others.stderr)
            .contains("comment 1 was written by ann, not by the login Shoal acts as"),
        "{others:?}"
    );
    let missing = edit("99");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("#8 has no comment 99"),
        "{missing:?}"
    );
    // A comment that already says the body is left unchanged.
    fs::write(&note, "Fixd").unwrap();
    let unchanged = edit("2");
    assert!(unchanged.status.success(), "{unchanged:?}");
    assert_eq!(
        writes(&fixture),
        [json!(["PATCH repos/team/project/issues/comments/2", {"body": "Fixed"}])]
    );
    let event = fixture
        .events()
        .into_iter()
        .rfind(|event| event["kind"] == "item_changed")
        .unwrap();
    assert_eq!(event["item"]["action"], "edit_comment", "{event}");
    // The edit is the agent's own change, unlike closing in the web interface.
    fixture.respond(json!({
        "GET repos/team/project/issues/8": issue("closed"),
        "GET repos/team/project/issues/8/comments?limit=50&page=1": comments("Fixed"),
    }));
    let closed = fixture.ok(&["watch", "issue", "--workspace", "topic"]);
    let kinds: Vec<_> = closed["updates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|update| update["kind"].clone())
        .collect();
    assert_eq!(kinds, ["closed"], "{closed}");
}

#[test]
fn github_comment_edits_reach_inline_comments() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let threads = json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
        "pageInfo": {"hasNextPage": false, "endCursor": null},
        "nodes": [{"id": "T_1", "isResolved": false, "comments": {"nodes": [{"databaseId": 5}]}}]}}}}});
    fixture.respond(json!({
        "GET repos/team/project/pulls/8": {"body": pull(8, "open", "Fix", &[])},
        "pr view 8": {"body": {"number": 8, "state": "OPEN", "title": "Fix", "body": "",
            "author": {"login": "ann"}, "labels": [], "createdAt": "t0",
            "headRefName": "topic", "baseRefName": "main", "isDraft": false, "comments": []}},
        "GET repos/team/project/pulls/8/reviews": {"body": [[{"id": 11, "state": "COMMENTED",
            "user": {"login": "me"}, "body": "", "submitted_at": "t2"}]]},
        "GET repos/team/project/pulls/8/comments": {"body": [[{"id": 5,
            "pull_request_review_id": 11, "user": {"login": "me"}, "path": "a.rs",
            "line": 3, "body": "Wy?", "created_at": "t2"}]]},
        "POST graphql": {"body": threads},
        "GET user": {"body": {"login": "Me"}},
        "PATCH repos/team/project/pulls/comments/5": {"body": {"id": 5, "body": "Why?"}},
    }));
    let edited = fixture.ok(&[
        "pr",
        "comment",
        "--workspace",
        "topic",
        "8",
        "--edit",
        "5",
        "--body",
        "Why?",
    ]);
    assert_eq!(edited["number"], 8, "{edited}");
    let mut writes = writes(&fixture);
    writes.retain(|request| request[0] != "POST graphql" && request[0] != "pr view 8");
    assert_eq!(
        writes,
        [json!(["PATCH repos/team/project/pulls/comments/5", {"body": "Why?"}])]
    );
    // An edit names the comment's new body, never files.
    let attached = fixture.run(&[
        "pr", "comment", "8", "--edit", "5", "--body", "Why?", "--attach", "a.png",
    ]);
    assert!(
        String::from_utf8_lossy(&attached.stderr).contains("cannot be used with"),
        "{attached:?}"
    );
}

/// `shots/login.png` and `shots/demo.mp4` under the fixture, and a body file
/// beside them that references the image.
fn attachment_files(fixture: &Fixture) -> [String; 3] {
    let shots = fixture.root.path().join("shots");
    fs::create_dir_all(&shots).unwrap();
    fs::write(shots.join("login.png"), "png").unwrap();
    fs::write(shots.join("demo.mp4"), "video").unwrap();
    fs::write(
        shots.join("note.md"),
        "Before:\n\n![The error](./login.png)\n",
    )
    .unwrap();
    ["note.md", "login.png", "demo.mp4"].map(|name| shots.join(name).to_str().unwrap().to_owned())
}

#[test]
fn github_comments_embed_files_uploaded_to_the_repository() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let [note, image, video] = attachment_files(&fixture);
    let uploads = "POST https://uploads.github.com/user-attachments/assets";
    fixture.respond(json!({
        "GET repos/team/project/pulls/8": {"body": pull(8, "open", "Fix", &[])},
        "GET repos/team/project": {"body": {"id": 42}},
        format!("{uploads}?name=login.png&content_type=image%2Fpng&repository_id=42"):
            {"body": {"url": "https://github.com/user-attachments/assets/1"}},
        format!("{uploads}?name=demo.mp4&content_type=video%2Fmp4&repository_id=42"):
            {"body": {"url": "https://github.com/user-attachments/assets/2"}},
        "POST repos/team/project/issues/8/comments": {"body": {"node_id": "C_1"}},
    }));
    let video_arg = format!("{video}#Ignored for videos");
    fixture.ok(&[
        "pr",
        "comment",
        "8",
        "--workspace",
        "topic",
        "--body-file",
        &note,
        "--attach",
        &image,
        "--attach",
        &video_arg,
    ]);
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| request[0].as_str().unwrap().starts_with("POST"))
        .map(|request| request[1].clone())
        .collect();
    assert_eq!(
        sent,
        [
            json!({"file": "login.png", "type": "image/png", "bytes": 3}),
            json!({"file": "demo.mp4", "type": "video/mp4", "bytes": 5}),
            json!({"body": "Before:\n\n![The error](https://github.com/user-attachments/assets/1)\n\n\
                https://github.com/user-attachments/assets/2"}),
        ]
    );

    // Files are checked before anything is sent.
    let text = fixture.root.path().join("notes.txt");
    fs::write(&text, "").unwrap();
    let refused = fixture.run(&[
        "pr",
        "comment",
        "8",
        "--workspace",
        "topic",
        "--attach",
        text.to_str().unwrap(),
    ]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("is empty"),
        "{refused:?}"
    );
    assert!(fixture.requests().is_empty());

    // Without files nothing needs the working directory, which may be gone.
    let deleted = fixture.root.path().join("deleted");
    fs::create_dir(&deleted).unwrap();
    let output = support::isolated(fixture.root.path(), "bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "cd -- \"$1\" && rmdir -- \"$1\" || exit; shift; exec \"$@\"",
            "deleted-cwd-test",
        ])
        .arg(&deleted)
        .arg(env!("CARGO_BIN_EXE_shoal"))
        .args([
            "pr",
            "comment",
            "8",
            "--workspace",
            "topic",
            "--body",
            "Done",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn gitlab_uploads_send_a_form_through_the_agents_glab() {
    // The scripted gh answers as glab on a GitLab remote too.
    let fixture = Fixture::new(Forge::GitHub, "[agent.forge_clis]\nglab = '~/agent-glab'\n");
    let root = fixture.root.path();
    std::os::unix::fs::symlink(root.join("forge.py"), root.join("bin/glab")).unwrap();
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "git@gitlab.com:team/project.git",
        ],
    );
    let wrapper = "#!/bin/sh\nAGENT=agent exec python3 \"$HOME/forge.py\" \"$@\"\n";
    support::install_test_script(fixture.root.path().join("agent-glab"), wrapper);
    let [_, image, _] = attachment_files(&fixture);
    let project = "projects/team%2Fproject";
    fixture.respond(json!({
        format!("GET {project}/merge_requests/8"): {"body": {"iid": 8, "title": "Fix",
            "state": "opened", "description": "", "labels": [], "source_branch": "topic",
            "target_branch": "main", "draft": false, "sha": "abc",
            "source_project_id": 1, "target_project_id": 1}},
        format!("POST {project}/uploads"): {"status": 201, "body": {"url": "/uploads/1/login.png"}},
        format!("POST {project}/merge_requests/8/notes"): {"status": 201, "body": {"id": 5}},
    }));
    fixture.ok(&[
        "pr",
        "comment",
        "8",
        "--workspace",
        "topic",
        "--attach",
        &image,
    ]);
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| request[0].as_str().unwrap().starts_with("POST"))
        .map(|request| (request[1].clone(), request[2].clone()))
        .collect();
    assert_eq!(
        sent,
        [
            (
                json!({"field": "file", "file": "login.png", "type": "image/png", "bytes": 3}),
                json!("agent")
            ),
            (
                json!({"body": "![login.png](/uploads/1/login.png)"}),
                json!("agent")
            ),
        ]
    );
}

#[test]
fn forgejo_attaches_files_to_the_comment_or_item_that_shows_them() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let [note, image, video] = attachment_files(&fixture);
    let ledger = fixture.root.path().join("ledger.tsv");
    fs::write(&ledger, "a\tb\n").unwrap();
    let ledger = ledger.to_str().unwrap();
    let host = fixture.web.trim_end_matches("/team/project").to_owned();
    let url = |id: u64| format!("{host}/attachments/{id}");
    let comments = "repos/team/project/issues/comments/70";
    fixture.respond(json!({
        "GET settings/attachment": attachment_settings(true, ".png,.mp4,text/*", 1),
        "GET repos/team/project/pulls/8": {"body": pull(8, "open", "Fix", &[])},
        "POST repos/team/project/issues/8/comments": {"status": 201, "body": {"id": 70}},
        format!("POST {comments}/assets?name=login.png"):
            {"status": 201, "body": {"browser_download_url": url(1)}},
        format!("POST {comments}/assets?name=demo.mp4"):
            {"status": 201, "body": {"browser_download_url": url(2)}},
        format!("POST {comments}/assets?name=ledger.tsv"):
            {"status": 201, "body": {"browser_download_url": url(5)}},
        format!("PATCH {comments}"): {"body": {}},
        "GET repos/team/project/issues/3": {"body": {"number": 3, "title": "Bug",
            "state": "open", "labels": [], "body": "Steps"}},
        "POST repos/team/project/issues/3/assets?name=login.png":
            {"status": 201, "body": {"browser_download_url": url(3)}},
        "PATCH repos/team/project/issues/3": {"body": {}},
        "POST repos/team/project/issues": {"status": 201, "body": {"number": 40}},
        "GET repos/team/project/issues/40": {"body": {"number": 40, "title": "Flaky",
            "state": "open", "labels": [], "body": "Seen with `login.png`"}},
        "POST repos/team/project/issues/40/assets?name=login.png":
            {"status": 201, "body": {"browser_download_url": url(4)}},
        "PATCH repos/team/project/issues/40": {"body": {}},
    }));
    fixture.ok(&[
        "pr",
        "comment",
        "8",
        "--workspace",
        "topic",
        "--body-file",
        &note,
        "--attach",
        &image,
        "--attach",
        &video,
        "--attach",
        ledger,
    ]);
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| !request[0].as_str().unwrap().starts_with("GET"))
        .map(|request| (request[0].clone(), request[1].clone()))
        .collect();
    let upload = |name: &str, kind: &str, bytes: u64| json!({"field": "attachment", "file": name, "type": kind, "bytes": bytes});
    // The comment exists before its files, which the forge keeps with it,
    // and links to files that are neither images nor videos.
    assert_eq!(
        sent,
        [
            (
                json!("POST repos/team/project/issues/8/comments"),
                json!({"body": "Before:\n\n`login.png`\n\n`demo.mp4`\n\n`ledger.tsv`"})
            ),
            (
                json!(format!("POST {comments}/assets?name=login.png")),
                upload("login.png", "image/png", 3)
            ),
            (
                json!(format!("POST {comments}/assets?name=demo.mp4")),
                upload("demo.mp4", "video/mp4", 5)
            ),
            (
                json!(format!("POST {comments}/assets?name=ledger.tsv")),
                upload("ledger.tsv", "text/tab-separated-values", 4)
            ),
            (
                json!(format!("PATCH {comments}")),
                json!({"body": format!(
                    "Before:\n\n![The error]({})\n\n<video src=\"{}\" controls></video>\n\n[ledger.tsv]({})",
                    url(1),
                    url(2),
                    url(5)
                )})
            ),
        ]
    );

    // An edit without a description adds the files after the current one.
    let alt = format!("{image}#Login screen");
    fixture.ok(&[
        "issue",
        "edit",
        "3",
        "--workspace",
        "topic",
        "--attach",
        &alt,
    ]);
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| !request[0].as_str().unwrap().starts_with("GET"))
        .map(|request| request[1].clone())
        .collect();
    assert_eq!(
        sent,
        [
            upload("login.png", "image/png", 3),
            json!({"body": format!("Steps\n\n![Login screen]({})", url(3))}),
        ]
    );

    // A new issue opens naming its files, which then go into its description.
    let body = format!("Seen with ![]({image})");
    let opened = fixture.ok(&[
        "issue",
        "open",
        "--workspace",
        "topic",
        "--title",
        "Flaky",
        "--body",
        &body,
        "--attach",
        &image,
    ]);
    assert_eq!(opened["created"], true);
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| !request[0].as_str().unwrap().starts_with("GET"))
        .map(|request| request[1].clone())
        .collect();
    assert_eq!(
        sent,
        [
            json!({"title": "Flaky", "body": "Seen with `login.png`"}),
            upload("login.png", "image/png", 3),
            json!({"body": format!("Seen with ![login.png]({})", url(4))}),
        ]
    );
}

#[test]
fn forgejo_sends_a_comments_files_again_while_the_forge_is_busy() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let [_, image, _] = attachment_files(&fixture);
    let comments = "repos/team/project/issues/comments/70";
    let busy = json!({"status": 503, "body": {"message": "busy"}});
    fixture.respond(json!({
        "GET settings/attachment": attachment_settings(true, "", 1),
        "GET repos/team/project/pulls/8": {"body": pull(8, "open", "Fix", &[])},
        "POST repos/team/project/issues/8/comments": {"status": 201, "body": {"id": 70}},
        format!("POST {comments}/assets?name=login.png"): [busy,
            {"status": 201, "body": {"browser_download_url": "/attachments/1"}}],
        // A proxy's 502 may follow a change the forge made, which only the
        // update is safe to repeat.
        format!("PATCH {comments}"): [{"status": 502, "body": "bad gateway"}, {"body": {}}],
    }));
    fixture.ok(&[
        "pr",
        "comment",
        "8",
        "--workspace",
        "topic",
        "--body",
        "Seen",
        "--attach",
        &image,
    ]);
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| !request[0].as_str().unwrap().starts_with("GET"))
        .map(|request| request[0].clone())
        .collect();
    let upload = json!(format!("POST {comments}/assets?name=login.png"));
    let edit = json!(format!("PATCH {comments}"));
    assert_eq!(
        sent,
        [
            json!("POST repos/team/project/issues/8/comments"),
            upload.clone(),
            upload,
            edit.clone(),
            edit,
        ]
    );
}

fn attachment_settings(enabled: bool, allowed_types: &str, max_size: u64) -> Value {
    json!({"body": {"enabled": enabled, "allowed_types": allowed_types,
        "max_size": max_size, "max_files": 5}})
}

#[test]
fn forgejo_refuses_files_its_settings_refuse_before_posting() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let [_, image, video] = attachment_files(&fixture);
    for (settings, file, refusal) in [
        (
            attachment_settings(false, "", 1),
            &image,
            "does not accept attached files",
        ),
        (
            attachment_settings(true, ".png", 1),
            &video,
            "accepts only these attached files: .png",
        ),
    ] {
        fixture.respond(json!({"GET settings/attachment": settings}));
        for args in [
            &["pr", "comment", "8", "--body", "Seen"][..],
            &["issue", "open", "--title", "Flaky"],
        ] {
            let refused =
                fixture.run(&[args, &["--workspace", "topic", "--attach", file]].concat());
            assert!(
                String::from_utf8_lossy(&refused.stderr).contains(refusal),
                "{refused:?}"
            );
            let sent: Vec<_> = fixture
                .requests()
                .into_iter()
                .map(|request| request[0].clone())
                .collect();
            assert_eq!(sent, [json!("GET settings/attachment")]);
        }
    }
}

#[test]
fn watches_read_every_page_of_a_discussion() {
    let fixture = Fixture::new(Forge::Forgejo, "[cleanup.auto]\nenabled=false\n");
    let issue = json!({"body": {"number": 8, "title": "Issue", "state": "open", "body": "",
        "labels": [], "pull_request": null}});
    let page = |ids: std::ops::RangeInclusive<u64>, last: &str| {
        json!({"body": ids.map(|id| json!({"id": id, "body": if id == 51 { last } else { "old" }}))
            .collect::<Vec<_>>()})
    };
    let respond = |last: &str| {
        fixture.respond(json!({
            "GET repos/team/project/issues/8": issue,
            "GET repos/team/project/issues/8/comments?limit=50&page=1": page(1..=50, last),
            "GET repos/team/project/issues/8/comments?limit=50&page=2": page(51..=51, last),
        }))
    };
    respond("first");
    fixture.ok(&["link", "issue", "8", "assigned", "--workspace", "topic"]);
    let first = fixture.ok(&["watch", "issue", "--workspace", "topic"]);
    assert_eq!(first["updates"][0]["kind"], "comment", "{first}");
    // Only the second page changes; the timeout is a hang guard.
    respond("edited");
    let edited = fixture.ok(&["watch", "issue", "--workspace", "topic", "--timeout", "600"]);
    assert_eq!(edited["updates"][0]["kind"], "comment", "{edited}");
}

#[test]
fn watches_deliver_item_activity_while_git_fails_in_the_worktree() {
    let fixture = Fixture::new(Forge::Forgejo, "[cleanup.auto]\nenabled=false\n");
    fixture.respond(json!({
        "GET repos/team/project/issues/8": {"body": {"number": 8, "title": "Issue",
            "state": "open", "body": "", "labels": [], "pull_request": null}},
        "GET repos/team/project/issues/8/comments?limit=50&page=1":
            {"body": [{"id": 1, "body": "Hello?"}]},
    }));
    fixture.ok(&["link", "issue", "8", "assigned", "--workspace", "topic"]);
    let workspace = fixture.ok(&["status", "topic"])["workspace"].clone();
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join(".git"), "gitdir: missing\n").unwrap();
    let result = fixture.ok(&["watch", "--workspace", "topic", "--timeout", "600"]);
    let updates = result["updates"].as_array().unwrap();
    assert!(
        updates.iter().any(|update| update["kind"] == "comment"),
        "{result}"
    );
    // The branch's conflict check needs Git, so it alone reports a failure.
    assert!(
        updates
            .iter()
            .any(|update| update["kind"] == "lookup_failed" && update["url"] == "topic"),
        "{result}"
    );
}

#[test]
fn watches_do_not_report_the_agents_own_comments_back() {
    let fixture = Fixture::new(Forge::Forgejo, "[cleanup.auto]\nenabled=false\n");
    let issue = |state: &str| {
        json!({"body": {"number": 8, "title": "Issue", "state": state, "body": "",
            "labels": [], "pull_request": null}})
    };
    let comments = |ids: &[u64]| {
        json!({"body": ids.iter().map(|id| json!({"id": id, "body": format!("comment {id}")}))
            .collect::<Vec<_>>()})
    };
    fixture.respond(json!({
        "GET repos/team/project/issues/8": issue("open"),
        "GET repos/team/project/issues/8/comments?limit=50&page=1": comments(&[1]),
    }));
    fixture.ok(&["link", "issue", "8", "assigned", "--workspace", "topic"]);
    let first = fixture.ok(&["watch", "issue", "--workspace", "topic"]);
    assert_eq!(first["updates"][0]["kind"], "comment", "{first}");

    fixture.respond(json!({
        "GET repos/team/project/issues/8": issue("open"),
        "POST repos/team/project/issues/8/comments": {"body": {"id": 2, "body": "comment 2"}},
        "GET repos/team/project/issues/8/comments?limit=50&page=1": comments(&[1, 2]),
    }));
    let note = fixture.root.path().join("note.md");
    fs::write(&note, "comment 2").unwrap();
    let body = note.to_str().unwrap();
    fixture.ok(&[
        "issue",
        "comment",
        "--workspace",
        "topic",
        "--body-file",
        body,
    ]);
    // A comment posted before attaching its files failed is the agent's too.
    let [_, image, _] = attachment_files(&fixture);
    fixture.respond(json!({
        "GET settings/attachment": attachment_settings(true, "", 1),
        "GET repos/team/project/issues/8": issue("open"),
        "POST repos/team/project/issues/8/comments": {"body": {"id": 3, "body": "`login.png`"}},
        "POST repos/team/project/issues/comments/3/assets?name=login.png":
            {"status": 422, "body": {"message": "refused"}},
    }));
    let failed = fixture.run(&[
        "issue",
        "comment",
        "--workspace",
        "topic",
        "--attach",
        &image,
    ]);
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("attaching its files failed"), "{stderr}");
    // Closing in the web interface is someone else's change; the agent's own
    // comments beside it are not reported.
    fixture.respond(json!({
        "GET repos/team/project/issues/8": issue("closed"),
        "GET repos/team/project/issues/8/comments?limit=50&page=1": comments(&[1, 2, 3]),
    }));
    let closed = fixture.ok(&["watch", "issue", "--workspace", "topic"]);
    let kinds: Vec<_> = closed["updates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|update| update["kind"].clone())
        .collect();
    assert_eq!(kinds, ["closed"], "{closed}");
}

/// The first page of an issue's discussion as `forge` reads it.
fn discussion_page(forge: Forge, number: u64) -> String {
    let page = match forge {
        Forge::GitHub => "per_page=100",
        Forge::Forgejo => "limit=50",
    };
    format!("GET repos/team/project/issues/{number}/comments?{page}&page=1")
}

#[test]
fn watches_read_unchanged_items_as_not_modified() {
    for forge in [Forge::GitHub, Forge::Forgejo] {
        let fixture = Fixture::new(forge, "[cleanup.auto]\nenabled=false\n");
        let respond = |state: &str| {
            fixture.respond(json!({
                "GET repos/team/project/issues/8": {"body": {"number": 8, "title": "Issue",
                    "state": state, "body": "", "labels": [], "pull_request": null}},
                discussion_page(forge, 8): {"body": [{"id": 1, "node_id": "IC_1", "body": "first"}]},
            }))
        };
        respond("open");
        fixture.ok(&["link", "issue", "8", "assigned", "--workspace", "topic"]);
        let first = fixture.ok(&["watch", "issue", "--workspace", "topic"]);
        assert_eq!(first["updates"][0]["kind"], "comment", "{first}");
        fixture.requests();
        // The remembered discussion stands for the one the forge did not send.
        respond("closed");
        let closed = fixture.ok(&["watch", "issue", "--workspace", "topic"]);
        assert_eq!(closed["updates"].as_array().unwrap().len(), 1, "{closed}");
        assert_eq!(closed["updates"][0]["kind"], "closed", "{closed}");
        let log = fs::read_to_string(fixture.root.path().join("forge/requests.jsonl")).unwrap();
        let discussion: Vec<Value> = log
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|request| request["key"].as_str().unwrap().contains("/comments"))
            .map(|request| request["status"].clone())
            .collect();
        assert!(!discussion.is_empty());
        assert!(discussion.iter().all(|status| status == 304), "{log}");
    }
}

#[test]
fn a_rate_limited_forge_slows_watches_instead_of_failing_lookups() {
    for forge in [Forge::GitHub, Forge::Forgejo] {
        let fixture = Fixture::new(forge, "[cleanup.auto]\nenabled=false\n");
        // GitHub refuses with 403 and says when its limit resets.
        let refusal = match forge {
            Forge::GitHub => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                json!({"status": 403, "headers": {"X-RateLimit-Remaining": "0",
                    "X-RateLimit-Reset": (now + 3000).to_string()},
                    "body": {"message": "API rate limit exceeded"}})
            }
            Forge::Forgejo => json!({"status": 429, "headers": {"Retry-After": "3000"},
                "body": {"message": "slow down"}}),
        };
        fixture.respond(json!({
            "GET repos/team/project/issues/8": {"body": {"number": 8, "title": "Issue",
                "state": "open", "body": "", "labels": [], "pull_request": null}},
            discussion_page(forge, 8): refusal,
        }));
        fixture.ok(&["link", "issue", "8", "assigned", "--workspace", "topic"]);
        let mut watch = fixture
            .command()
            .args(["--json", "watch", "issue", "--workspace", "topic"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let limited = || {
            let notifications = fixture.ok(&["alerts", "--keep-unread"]);
            notifications
                .as_array()
                .unwrap()
                .iter()
                .any(|notification| notification["kind"] == "forge_limited")
        };
        support::wait::wait_until("the forge limit to be noticed", limited);
        let running = watch.try_wait().unwrap().is_none();
        watch.kill().unwrap();
        let output = watch.wait_with_output().unwrap();
        assert!(running, "{}", String::from_utf8_lossy(&output.stdout));
        // The forge is asked once; until the limit ends, no request goes out.
        let discussion = fixture
            .requests()
            .into_iter()
            .filter(|request| request[0].as_str().unwrap().contains("/comments"))
            .count();
        assert_eq!(discussion, 1);
        let failures = fixture
            .events()
            .into_iter()
            .filter(|event| event.to_string().contains("lookup_failed"))
            .count();
        assert_eq!(failures, 0);
    }
}

#[test]
fn bare_numbers_select_the_issue_or_pr_they_name() {
    let fixture = Fixture::new(Forge::Forgejo, "[cleanup.auto]\nenabled=false\n");
    let issue = json!({"number": 8, "title": "Issue", "state": "open", "body": "",
        "labels": [], "pull_request": null});
    let pr_issue = json!({"number": 9, "title": "Fix", "state": "open", "body": "",
        "labels": [], "pull_request": {"merged": false}});
    fixture.respond(json!({
        "GET repos/team/project/issues/8": {"body": issue},
        "GET repos/team/project/issues/9": {"body": pr_issue},
        "GET repos/team/project/pulls/9": {"body": pull(9, "open", "Fix", &[])},
    }));
    fixture.ok(&["link", "8", "assigned", "--workspace", "topic"]);
    fixture.ok(&["link", "#9", "assigned", "--workspace", "topic"]);
    let links = &fixture.ok(&["status", "topic"])["workspace"]["links"];
    assert_eq!(links["issue"], format!("{}/issues/8", fixture.web));
    assert_eq!(links["prs"], json!([format!("{}/pulls/9", fixture.web)]));

    let marks = fixture.ok(&["ready", "#9", "--workspace", "topic"]);
    assert_eq!(marks[0]["kind"], "pr", "{marks}");
    fixture.ok(&["unlink", "8", "assigned", "--workspace", "topic"]);
    let links = &fixture.ok(&["status", "topic"])["workspace"]["links"];
    assert!(links["issue"].is_null(), "{links}");
    assert_eq!(links["prs"], json!([format!("{}/pulls/9", fixture.web)]));

    // Recorded relations name the kind without asking the forge, as forges
    // with separate issue and PR numbers need.
    fixture.ok(&["link", "pr", "10", "related", "--workspace", "topic"]);
    fixture.respond(json!({}));
    fixture.requests();
    fixture.ok(&["unlink", "10", "related", "--workspace", "topic"]);
    assert!(fixture.requests().is_empty());
    let links = &fixture.ok(&["status", "topic"])["workspace"]["links"];
    assert_eq!(links["related"], json!([]), "{links}");
}

#[test]
fn pr_and_issue_commands_act_on_linked_or_named_items() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let endpoint = "repos/team/project/pulls/8";
    let issue = json!({"number": 3, "title": "Bug", "state": "open", "labels": []});
    fixture.respond(json!({
        "pr view 8": {"body": {"number": 8, "state": "OPEN", "headRefName": "topic",
            "commits": []}},
        format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &["ready"])},
        format!("PATCH {endpoint}"): {"body": {}},
        "PUT repos/team/project/issues/8/labels": {"body": []},
        "POST graphql": {"body": {"data": {}}},
        "GET repos/team/project/issues/3": {"body": issue},
        "POST repos/team/project/issues/3/comments": {"body": {}},
    }));
    fixture.ok(&["link", "pr", "8", "assigned", "--workspace", "topic"]);
    fixture.requests();

    let nothing = fixture.run(&["pr", "edit", "--workspace", "topic"]);
    assert!(String::from_utf8_lossy(&nothing.stderr).contains("nothing to change"));
    let edited = fixture.ok(&[
        "pr",
        "edit",
        "--workspace",
        "topic",
        "--title=-New",
        "--add-label",
        "ready",
        "--draft",
    ]);
    assert_eq!(edited["url"], "https://github.com/team/project/pull/8");
    let keys: Vec<_> = fixture
        .requests()
        .into_iter()
        .map(|request| (request[0].clone(), request[1].clone()))
        .collect();
    assert_eq!(
        keys[1],
        (json!(format!("PATCH {endpoint}")), json!({"title": "-New"}))
    );
    assert_eq!(keys[2].1, json!({"labels": ["ready"]}));
    assert_eq!(keys[3].0, "POST graphql");
    assert_eq!(keys.len(), 5);

    let note = fixture.root.path().join("note.md");
    fs::write(&note, "Seen twice\n").unwrap();
    let commented = fixture.ok(&[
        "issue",
        "comment",
        "3",
        "--workspace",
        "topic",
        "--body-file",
        note.to_str().unwrap(),
    ]);
    assert_eq!(commented["url"], "https://github.com/team/project/issues/3");
    assert_eq!(
        fixture.requests()[1],
        json!(["POST repos/team/project/issues/3/comments", {"body": "Seen twice\n"}, null])
    );

    // Merging is the user's: a workspace process is refused before any request.
    let shoal = env!("CARGO_BIN_EXE_shoal");
    let merge = ["pr", "merge", "--method", "rebase"];
    let scoped = fixture.run(&[&["run", "--workspace", "topic", "--", shoal][..], &merge].concat());
    assert!(
        String::from_utf8_lossy(&scoped.stderr).contains("merging is the user's decision"),
        "{scoped:?}"
    );
    assert!(fixture.requests().is_empty());
}

#[test]
fn issue_edits_set_assignees_and_milestones_that_views_show() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let issue = |assignees: &[&str], milestone: Value| {
        json!({"number": 3, "title": "Bug", "state": "open", "labels": [], "body": "",
            "user": {"login": "ann"}, "created_at": "t0", "pull_request": null,
            "assignees": assignees.iter().map(|login| json!({"login": login})).collect::<Vec<_>>(),
            "milestone": milestone})
    };
    fixture.respond(json!({
        "GET repos/team/project/issues/3": [
            {"body": issue(&["kim"], Value::Null)},
            {"body": issue(&["sam"], json!({"id": 9, "title": "v2"}))},
        ],
        "GET repos/team/project/milestones?state=all&limit=50&page=1":
            {"body": [{"id": 8, "title": "v1"}, {"id": 9, "title": "v2"}]},
        "PATCH repos/team/project/issues/3": {"body": {}},
    }));
    let edited = fixture.ok(&[
        "issue",
        "edit",
        "3",
        "--workspace",
        "topic",
        "--add-assignee",
        "sam",
        "--remove-assignee",
        "kim",
        "--milestone",
        "v2",
    ]);
    assert_eq!(edited["assignees"], json!(["sam"]), "{edited}");
    assert_eq!(edited["milestone"], "v2");
    let patch: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| request[0] == "PATCH repos/team/project/issues/3")
        .map(|request| request[1].clone())
        .collect();
    assert_eq!(patch, [json!({"assignees": ["sam"], "milestone": 9})]);

    fixture.respond(json!({
        "GET repos/team/project/issues/3": {"body": issue(&["sam"], json!({"id": 9, "title": "v2"}))},
    }));
    let args = [
        "view",
        "issue",
        "3",
        "--workspace",
        "topic",
        "--no-comments",
    ];
    let view = &fixture.ok(&args)[0];
    assert_eq!(
        (&view["assignees"], &view["milestone"]),
        (&json!(["sam"]), &json!("v2")),
        "{view}"
    );
    let text = fixture.run(&args);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Assignees: sam\nMilestone: v2\n"), "{text}");

    let both = fixture.run(&[
        "issue",
        "edit",
        "3",
        "--workspace",
        "topic",
        "--milestone",
        "v2",
        "--clear-milestone",
    ]);
    assert!(!both.status.success());
}

#[test]
fn issues_open_with_labels_and_link_on_request() {
    for forge in [Forge::GitHub, Forge::Forgejo] {
        let fixture = Fixture::new(forge, "");
        let created = json!({"number": 40, "title": "Flaky", "state": "open",
            "labels": [{"name": "bug"}]});
        fixture.respond(json!({
            "POST repos/team/project/issues": {"status": 201, "body": {"number": 40}},
            "PUT repos/team/project/issues/40/labels": {"body": []},
            // Forgejo reads the unlabeled issue before labeling it.
            "GET repos/team/project/issues/40": match forge {
                Forge::GitHub => json!([{"body": created}]),
                Forge::Forgejo => json!([
                    {"body": {"number": 40, "title": "Flaky", "state": "open", "labels": []}},
                    {"body": created},
                ]),
            },
            "issue view 40": {"body": {"number": 40, "state": "OPEN", "title": "Flaky",
                "body": "", "comments": []}},
        }));
        let link = if forge == Forge::GitHub {
            &["--link"][..]
        } else {
            &[]
        };
        let open = [
            "issue",
            "open",
            "--workspace",
            "topic",
            "--title",
            "Flaky",
            "--label",
            "bug",
        ];
        let opened = fixture.ok(&[&open[..], link].concat());
        let url = format!("{}/issues/40", fixture.web);
        assert_eq!(opened["url"], url);
        assert_eq!(opened["labels"], json!(["bug"]));
        assert_eq!(opened["created"], true);
        assert_eq!(opened["linked"], forge == Forge::GitHub);
        // Linked or not, the issue is recorded as opened by the workspace.
        assert_eq!(
            fixture.ok(&["status", "topic"])["workspace"]["links"]["opened"],
            json!([{"kind": "issue", "url": url, "title": "Flaky"}])
        );
        let sent: Vec<_> = fixture
            .requests()
            .into_iter()
            .filter(|request| !request[0].as_str().unwrap().starts_with("GET"))
            .filter(|request| !request[0].as_str().unwrap().starts_with("issue view"))
            .map(|request| (request[0].clone(), request[1].clone()))
            .collect();
        match forge {
            Forge::GitHub => {
                assert_eq!(
                    sent,
                    [(
                        json!("POST repos/team/project/issues"),
                        json!({"title": "Flaky", "body": "", "labels": ["bug"]})
                    )]
                );
                let links = &fixture.ok(&["status", "topic"])["workspace"]["links"];
                assert_eq!(links["issue"], url);
                assert_eq!(links["issue_title"], "Flaky");
            }
            Forge::Forgejo => assert_eq!(
                sent,
                [
                    (
                        json!("POST repos/team/project/issues"),
                        json!({"title": "Flaky", "body": ""})
                    ),
                    (
                        json!("PUT repos/team/project/issues/40/labels"),
                        json!({"labels": ["bug"]})
                    ),
                ]
            ),
        }
    }
}

#[test]
fn pr_open_pushes_creates_and_links_or_finds_the_open_pr() {
    for forge in [Forge::GitHub, Forge::Forgejo] {
        let fixture = Fixture::new(forge, "");
        let root = fixture.root.path();
        let remote = root.join("remote.git");
        git(root, &["init", "--bare", "-q", remote.to_str().unwrap()]);
        git(
            &fixture.repo,
            &["config", "remote.origin.pushurl", remote.to_str().unwrap()],
        );
        // Linking a PR Shoal just read needs no lookup through the user's gh or fj.
        let list = match forge {
            Forge::GitHub => "GET repos/team/project/pulls?state=open&head=team%3Atopic",
            Forge::Forgejo => "GET repos/team/project/pulls?state=open&limit=50&page=1",
        };
        fixture.respond(json!({
            "issue view 34": {"body": {"number": 34, "state": "OPEN", "title": "Fix API timeout",
                "body": "", "comments": []}},
            list: {"body": []},
            "POST repos/team/project/pulls": {"status": 201, "body": {"number": 9}},
            "GET repos/team/project/pulls/9": {"body": pull(9, "open", "Fix", &[])},
        }));
        let expected = if forge == Forge::GitHub {
            fixture.ok(&["link", "issue", "34", "assigned", "--workspace", "topic"]);
            json!({"title": "Fix API timeout", "body": "Closes #34", "head": "topic",
                "base": "main", "draft": false})
        } else {
            json!({"title": "initial", "body": "", "head": "topic", "base": "main"})
        };
        fixture.requests();

        let opened = fixture.ok(&["pr", "open", "--workspace", "topic"]);
        let url = format!(
            "{}/{}/9",
            fixture.web,
            if forge == Forge::GitHub {
                "pull"
            } else {
                "pulls"
            }
        );
        assert_eq!(opened["url"], url);
        assert_eq!(
            (&opened["created"], &opened["linked"]),
            (&json!(true), &json!(true))
        );
        let created = fixture
            .requests()
            .into_iter()
            .find(|request| request[0] == "POST repos/team/project/pulls")
            .unwrap();
        assert_eq!(created[1], expected);
        let path = fixture.ok(&["status", "topic"])["workspace"]["path"].clone();
        assert_eq!(
            git(&remote, &["rev-parse", "refs/heads/topic"]),
            git(Path::new(path.as_str().unwrap()), &["rev-parse", "HEAD"])
        );
        let links = fixture.ok(&["status", "topic"])["workspace"]["links"]["prs"].clone();
        assert_eq!(links, json!([url]));

        // An open PR for the branch is linked instead of opening another.
        fixture.ok(&["unlink", "pr", "assigned", "--workspace", "topic"]);
        let mut responses: Value =
            serde_json::from_str(&fs::read_to_string(root.join("forge/responses.json")).unwrap())
                .unwrap();
        let mut fork = pull(5, "open", "Fork", &[]);
        fork["head"]["repo"]["full_name"] = json!("someone/project");
        responses[list] = json!({"body": [fork, pull(9, "open", "Fix", &[])]});
        fixture.respond(responses);
        let found = fixture.ok(&["pr", "open", "--workspace", "topic"]);
        assert_eq!(
            (&found["url"], &found["created"]),
            (&json!(url), &json!(false))
        );
        assert!(
            !fixture
                .requests()
                .iter()
                .any(|request| request[0] == "POST repos/team/project/pulls")
        );
    }
}

#[test]
fn pr_open_pushes_to_a_fork_and_opens_the_pr_from_it() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let root = fixture.root.path();
    let fork = root.join("fork.git");
    git(root, &["init", "--bare", "-q", fork.to_str().unwrap()]);
    let config = |args: &[&str]| git(&fixture.repo, args);
    config(&[
        "remote",
        "add",
        "fork",
        "https://github.com/someone/project",
    ]);
    config(&["config", "remote.fork.pushurl", fork.to_str().unwrap()]);
    config(&["config", "remote.pushDefault", "fork"]);
    let mut opened = pull(9, "open", "initial", &[]);
    opened["head"]["repo"]["full_name"] = json!("someone/project");
    fixture.respond(json!({
        "GET repos/team/project/pulls?state=open&head=someone%3Atopic": {"body": []},
        "POST repos/team/project/pulls": {"status": 201, "body": {"number": 9}},
        "GET repos/team/project/pulls/9": {"body": opened},
    }));
    fixture.requests();

    let result = fixture.ok(&["pr", "open", "--workspace", "topic"]);
    assert_eq!(result["created"], json!(true));
    let created = fixture
        .requests()
        .into_iter()
        .find(|request| request[0] == "POST repos/team/project/pulls")
        .unwrap();
    assert_eq!(created[1]["head"], "someone:topic");
    let path = fixture.ok(&["status", "topic"])["workspace"]["path"].clone();
    assert_eq!(
        git(&fork, &["rev-parse", "refs/heads/topic"]),
        git(Path::new(path.as_str().unwrap()), &["rev-parse", "HEAD"])
    );
}

#[test]
fn pr_open_refuses_unverified_or_switched_worktrees_before_pushing() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let root = fixture.root.path();
    let remote = root.join("remote.git");
    git(root, &["init", "--bare", "-q", remote.to_str().unwrap()]);
    git(
        &fixture.repo,
        &["config", "remote.origin.pushurl", remote.to_str().unwrap()],
    );
    let workspace = fixture.ok(&["status", "topic"])["workspace"].clone();
    let path = Path::new(workspace["path"].as_str().unwrap());
    let refused = |message: &str| {
        let output = fixture.run(&["pr", "open", "--workspace", "topic"]);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{output:?}"
        );
        let pushed = support::isolated(root, "git")
            .args([
                "-C",
                remote.to_str().unwrap(),
                "rev-parse",
                "-q",
                "--verify",
            ])
            .arg("refs/heads/topic")
            .output()
            .unwrap();
        assert!(!pushed.status.success());
        assert!(fixture.requests().is_empty());
    };
    let marker = Path::new(workspace["git_dir"].as_str().unwrap()).join("shoal-workspace");
    let owner = fs::read_to_string(&marker).unwrap();
    fs::write(&marker, "another-workspace\n").unwrap();
    refused("another Shoal workspace");
    fs::write(&marker, owner).unwrap();
    git(path, &["switch", "-q", "--detach"]);
    refused("not on its recorded branch");
}

#[test]
fn an_unconfirmed_github_merge_fails_and_keeps_the_branch() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let endpoint = "repos/team/project/pulls/8";
    fixture.respond(json!({
        format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
        format!("PUT {endpoint}/merge"): {"body": {"merged": false,
            "message": "Head branch was modified"}},
        "DELETE repos/team/project/git/refs/heads/topic": {"status": 204},
    }));
    let output = fixture.run(&[
        "pr",
        "merge",
        "8",
        "--workspace",
        "topic",
        "--method",
        "squash",
        "--delete-branch",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Head branch was modified"),
        "{output:?}"
    );
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .map(|request| request[0].clone())
        .collect();
    assert_eq!(
        sent,
        [
            json!(format!("GET {endpoint}")),
            json!(format!("PUT {endpoint}/merge"))
        ]
    );
    assert!(
        !fixture
            .events()
            .iter()
            .any(|event| event["kind"] == "item_changed")
    );
}

#[test]
fn delete_branch_deletes_an_ended_prs_branch_unless_it_moved() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let endpoint = "repos/team/project/pulls/8";
    let read = |state: &str| {
        let mut pull = pull(8, state, "Fix", &[]);
        pull["head"]["sha"] = json!("c0ffee");
        json!({"body": pull})
    };
    let branch = |commit: &str| json!({"body": {"commit": {"id": commit}}});
    let delete = |args: &[&str]| {
        let output =
            fixture.run(&[&["pr", "delete-branch", "8", "--workspace", "topic"], args].concat());
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        (output.status.success(), stdout, stderr)
    };
    fixture.respond(json!({
        format!("GET {endpoint}"): read("merged"),
        "GET repos/team/project/branches/topic": branch("c0ffee"),
        "DELETE repos/team/project/branches/topic": {"status": 204},
    }));
    let (ok, stdout, stderr) = delete(&[]);
    assert!(
        ok && stdout.starts_with("PR branch deleted: "),
        "{stdout}{stderr}"
    );
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .map(|request| json!([request[0], request[2]]))
        .collect();
    assert_eq!(
        sent,
        [
            json!([format!("GET {endpoint}"), "token usertoken"]),
            json!(["GET repos/team/project/branches/topic", "token usertoken"]),
            json!([
                "DELETE repos/team/project/branches/topic",
                "token usertoken"
            ]),
            json!([format!("GET {endpoint}"), "token usertoken"]),
        ]
    );

    // A branch pushed to after the PR closed keeps commits the PR lacks.
    fixture.respond(json!({
        format!("GET {endpoint}"): read("closed"),
        "GET repos/team/project/branches/topic": branch("beef00"),
    }));
    let (ok, _, stderr) = delete(&[]);
    assert!(
        !ok && stderr.contains("branch topic moved to beef00 after #8 was closed"),
        "{stderr}"
    );
    fixture.respond(json!({format!("GET {endpoint}"): read("open")}));
    let (ok, _, stderr) = delete(&[]);
    assert!(!ok && stderr.contains("#8 is open"), "{stderr}");
    let reads = fixture.requests();
    assert!(
        reads
            .iter()
            .all(|request| request[0].as_str().unwrap().starts_with("GET")),
        "{reads:?}"
    );

    // Deleting a branch is the user's: a workspace process is refused before
    // any request.
    let shoal = env!("CARGO_BIN_EXE_shoal");
    let scoped = fixture.run(&[
        "run",
        "--workspace",
        "topic",
        "--",
        shoal,
        "pr",
        "delete-branch",
        "8",
    ]);
    assert!(
        String::from_utf8_lossy(&scoped.stderr)
            .contains("deleting a PR's branch is the user's decision"),
        "{scoped:?}"
    );
    assert!(fixture.requests().is_empty());
}

#[test]
fn auto_merges_are_scheduled_and_cancelled_with_the_merge_command() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let endpoint = "repos/team/project/pulls/8";
    let responses = |read: Value| {
        json!({
            format!("GET {endpoint}"): read,
            format!("POST {endpoint}/merge"): {"body": {}},
            format!("DELETE {endpoint}/merge"): {"status": 204},
        })
    };
    let open = json!({"body": pull(8, "open", "Fix", &[])});
    fixture.respond(responses(open.clone()));
    let merge = |args: &[&str]| {
        let output = fixture.run(&[&["pr", "merge", "8", "--workspace", "topic"], args].concat());
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        (output.status.success(), text)
    };
    let (ok, text) = merge(&["--auto", "--method", "squash"]);
    assert!(ok && text.starts_with("Auto-merge enabled: "), "{text}");
    let (ok, text) = merge(&["--cancel-auto"]);
    assert!(ok && text.starts_with("Auto-merge canceled: "), "{text}");
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| !request[0].as_str().unwrap().starts_with("GET"))
        .map(|request| json!([request[0], request[1]]))
        .collect();
    assert_eq!(
        sent,
        [
            json!([format!("POST {endpoint}/merge"), {"Do": "squash",
                "delete_branch_after_merge": false, "merge_when_checks_succeed": true}]),
            json!([format!("DELETE {endpoint}/merge"), null]),
        ]
    );
    // The checks already pass, so the forge merges the PR at once.
    let merged = json!({"body": pull(8, "merged", "Fix", &[])});
    fixture.respond(responses(json!([open, merged])));
    let (ok, text) = merge(&["--auto", "--method", "merge"]);
    assert!(ok && text.starts_with("PR merged: "), "{text}");

    fixture.requests();
    for refused in [
        &["--auto"][..],
        &["--cancel-auto", "--method", "squash"],
        &["--cancel-auto", "--auto"],
    ] {
        assert!(!merge(refused).0, "{refused:?}");
    }
    assert!(fixture.requests().is_empty());
}

#[test]
fn lists_search_through_the_forge_as_the_user_or_the_scoped_agent() {
    let fixture = Fixture::new(Forge::GitHub, "");
    fixture.respond(json!({
        "issue list --repo=github.com/team/project --state=open --limit=30 --json=number,title,state,labels,assignees,milestone --search=flaky watch":
            {"body": [{"number": 40, "title": "Flaky watch", "state": "OPEN",
                "labels": [{"name": "bug"}]}]},
    }));
    let listed = fixture.ok(&["issue", "list", "--workspace", "topic", "flaky", "watch"]);
    assert_eq!(
        listed,
        json!([{"kind": "issue", "number": 40, "url": format!("{}/issues/40", fixture.web),
            "title": "Flaky watch", "state": "open", "labels": ["bug"], "assignees": [],
            "milestone": null}])
    );
    let output = fixture.run(&["issue", "list", "--workspace", "topic", "flaky", "watch"]);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "#40  open    Flaky watch  [bug]\n"
    );
    // Outside a workspace, a bad filter fails before a workspace is chosen.
    for filter in [&["--label", ""][..], &["--author", " "], &[" "]] {
        let output = fixture
            .command()
            .current_dir(fixture.root.path())
            .args(["issue", "list"])
            .args(filter)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("cannot be empty"), "{filter:?}: {stderr}");
    }
    // So does a repository that names none, for every command taking --repo.
    for args in [
        &["issue", "list"][..],
        &["pr", "list"],
        &["forge", "api", "repos/{owner}/{repo}"],
        &["repo", "labels"],
        &["forge", "capabilities"],
        &["issue", "open", "--title", "Bug"],
    ] {
        let output = fixture
            .command()
            .current_dir(fixture.root.path())
            .args(args)
            .args(["--repo", "team/../tool"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("names no repository"), "{args:?}: {stderr}");
    }
    // A listing longer than one protocol frame arrives in several.
    let long: Vec<Value> = (1..=400)
        .map(|number| {
            json!({"number": number, "title": "x".repeat(250), "state": "OPEN",
            "labels": [{"name": "bug"}]})
        })
        .collect();
    fixture.respond(json!({
        "issue list --repo=github.com/team/project --state=open --limit=400": {"body": long},
    }));
    let listed = fixture.ok(&["issue", "list", "--workspace", "topic", "--limit", "400"]);
    let numbers: Vec<u64> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["number"].as_u64().unwrap())
        .collect();
    assert_eq!(numbers, (1..=400).collect::<Vec<_>>());

    let fixture = Fixture::new(Forge::Forgejo, AGENT_TOKEN);
    // Forgejo's issue search lists PRs without their branches, so each is read.
    let search = "GET repos/team/project/issues?type=pulls&state=closed&labels=bug&created_by=dev&q=crash&limit=50&page=1";
    fixture.respond(json!({
        search: {"body": [{"number": 8, "pull_request": {}}, {"number": 9, "pull_request": {}}]},
        "GET repos/team/project/pulls/8": {"body": pull(8, "merged", "Fix crash", &["bug"])},
        "GET repos/team/project/pulls/9": {"body": pull(9, "closed", "Crash", &["bug"])},
    }));
    let listed = fixture.ok(&[
        "pr",
        "list",
        "--workspace",
        "topic",
        "--state",
        "closed",
        "--label",
        "bug",
        "--author",
        "dev",
        "--limit",
        "1",
        "crash",
    ]);
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed[0]["state"], "merged");
    assert_eq!(
        listed[0]["pr"],
        json!({"head": "topic", "base": "main", "draft": false})
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(
        requests
            .iter()
            .all(|request| request[2] == "token usertoken")
    );

    let scope = fixture.ok(&["work", "env", "topic"])["SHOAL_SCOPE_TOKEN"]
        .as_str()
        .unwrap()
        .to_owned();
    let method = json!({"list_items": {"workspace": "topic", "kind": "pr",
        "search": {"state": "closed", "labels": ["bug"], "author": "dev", "text": "crash",
            "limit": 5}}});
    let result = fixture.request_as(method, Some(&scope));
    assert_eq!(result["type"], "items", "{result}");
    assert_eq!(result["data"].as_array().unwrap().len(), 2);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert!(
        requests
            .iter()
            .all(|request| request[2] == "token agenttoken")
    );
}

#[test]
fn lists_search_another_repository_with_the_same_filters() {
    let fixture = Fixture::new(Forge::Forgejo, AGENT_TOKEN);
    let search =
        "GET repos/team/tool/issues?type=issues&state=all&labels=bug&q=crash&limit=50&page=1";
    fixture.respond(json!({
        search: {"body": [{"number": 5, "title": "Crash", "state": "closed",
            "labels": [{"name": "bug"}]}]},
    }));
    let list = [
        "issue",
        "list",
        "--repo",
        "team/tool",
        "--state",
        "all",
        "--label",
        "bug",
        "crash",
    ];
    let listed = fixture.ok(&[&list[..], &["--workspace", "topic"]].concat());
    let url = fixture.web.replace("team/project", "team/tool") + "/issues/5";
    assert_eq!(listed[0]["url"], url, "{listed}");
    // A workspace process searches it as the agent account.
    assert_eq!(scoped(&fixture, &list)[0]["url"], url);
    assert_eq!(
        fixture.requests(),
        [
            json!([search, null, "token usertoken"]),
            json!([search, null, "token agenttoken"]),
        ]
    );
}

#[test]
fn issue_open_with_link_refuses_before_creating_when_an_issue_is_linked() {
    let fixture = Fixture::new(Forge::GitHub, "");
    fixture.respond(json!({
        "issue view 34": {"body": {"number": 34, "state": "OPEN", "title": "Assignment",
            "body": "", "comments": []}},
    }));
    fixture.ok(&["link", "issue", "34", "assigned", "--workspace", "topic"]);
    fixture.requests();
    let output = fixture.run(&[
        "issue",
        "open",
        "--workspace",
        "topic",
        "--title",
        "Follow-up",
        "--link",
    ]);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("already has a linked issue"),
        "{output:?}"
    );
    assert!(fixture.requests().is_empty());
}

#[test]
fn scoped_check_logs_resolve_web_indices_and_use_the_agent_account() {
    let fixture = Fixture::new(Forge::Forgejo, AGENT_TOKEN);
    let mut responses = failed_check_responses();
    fixture.respond(responses.clone());
    let exported = fixture.ok(&["work", "env", "topic"]);
    let scope = exported["SHOAL_SCOPE_TOKEN"].as_str().unwrap();
    let method = json!({"check_log": {"workspace": "topic", "selection": {"kind": "pr", "input": "8"}, "check": "rust"}});
    let result = fixture.request_as(method.clone(), Some(scope));
    assert_eq!(result["type"], "check_log", "{result}");
    assert_eq!(result["data"]["failed_step"], "Main Test");
    assert_eq!(
        result["data"]["job_url"],
        format!("{}/actions/runs/2765/jobs/1", fixture.web)
    );
    assert_eq!(result["data"]["truncated"], false);
    assert!(
        result["data"]["text"]
            .as_str()
            .unwrap()
            .contains("assertion failed")
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 5);
    assert!(
        requests
            .iter()
            .all(|request| request[2] == "token agenttoken"),
        "{requests:?}"
    );
    assert_eq!(
        requests[4][0],
        "GET repos/team/project/actions/jobs/18345/logs"
    );

    // Never show a stale failure after the same check succeeds on this head.
    responses["GET repos/team/project/commits/head/statuses?limit=50&page=1"]["body"][0]["status"] =
        json!("success");
    fixture.respond(responses.clone());
    let result = fixture.request_as(method.clone(), Some(scope));
    assert!(
        result["data"]["message"]
            .as_str()
            .unwrap()
            .contains("has not failed"),
        "{result}"
    );
    assert_eq!(fixture.requests().len(), 2);

    // Scoped protocol clients cannot select another workspace or repository.
    fixture.ok(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "other",
        "--base",
        "HEAD",
    ]);
    let mut other = method.clone();
    other["check_log"]["workspace"] = json!("other");
    assert_eq!(
        fixture.request_as(other, Some(scope))["data"]["code"],
        "scope_denied"
    );
    let mut other = method.clone();
    other["check_log"]["selection"]["input"] =
        json!("https://elsewhere.example/team/project/pulls/8");
    assert!(
        fixture.request_as(other, Some(scope))["data"]["message"]
            .as_str()
            .unwrap()
            .contains("repository")
    );
    assert!(fixture.requests().is_empty());

    // Missing API support is an explicit failure, not an empty successful log.
    responses["GET repos/team/project/commits/head/statuses?limit=50&page=1"]["body"][0]["status"] =
        json!("failure");
    responses["GET repos/team/project/actions/jobs/18345/logs"] =
        json!({"status": 404, "body": {"message": "logs unavailable"}});
    fixture.respond(responses);
    let result = fixture.request_as(method, Some(scope));
    assert!(
        result["data"]["message"]
            .as_str()
            .unwrap()
            .contains("HTTP 404"),
        "{result}"
    );
}

fn failed_check_responses() -> Value {
    let target = "/team/project/actions/runs/2765/jobs/1";
    json!({
        "GET repos/team/project/pulls/8": {"body": {"head": {"sha": "head"}}},
        "GET repos/team/project/commits/head/statuses?limit=50&page=1": {"body": [
            {"context": "rust", "status": "failure", "target_url": target},
            {"context": "rust", "status": "success", "target_url": "/old"}
        ]},
        "GET repos/team/project/actions/runs?run_number=2765": {"body": {"workflow_runs": [
            {"id": 9331, "index_in_repo": 2765, "commit_sha": "head"}
        ]}},
        "GET repos/team/project/actions/runs/9331/jobs": {"body": [
            {"id": 18344, "run_id": 9331, "status": "success"},
            {"id": 18345, "run_id": 9331, "status": "failure", "task_id": 9850}
        ]},
        "GET repos/team/project/actions/jobs/18345/logs": {"body": "⭐ Run Main Test\nassertion failed\n⚙️ [runner]: exitcode '1': failure\n"}
    })
}

#[test]
fn check_logs_read_github_and_gitlab_jobs_as_the_agent_and_refuse_external_checks() {
    for gitlab in [false, true] {
        let fixture = Fixture::new(
            Forge::GitHub,
            "[agent.forge_clis]\ngh = '~/agent-gh'\nglab = '~/agent-glab'\n",
        );
        let root = fixture.root.path();
        let wrapper = "#!/bin/sh\nAGENT=agent exec python3 \"$HOME/forge.py\" \"$@\"\n";
        support::install_test_script(root.join("agent-gh"), wrapper);
        support::install_test_script(root.join("agent-glab"), wrapper);
        let external = "https://coverage.example/7";
        let (job, log, responses) = if gitlab {
            std::os::unix::fs::symlink(root.join("forge.py"), root.join("bin/glab")).unwrap();
            git(
                &fixture.repo,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "git@gitlab.com:team/project.git",
                ],
            );
            let project = "projects/team%2Fproject";
            let job = "https://gitlab.com/team/project/-/jobs/56";
            let log = format!("GET {project}/jobs/56/trace");
            let responses = json!({
                format!("GET {project}/merge_requests/8"): {"body": {"iid": 8, "title": "Fix",
                    "state": "opened", "description": "", "labels": [], "source_branch": "topic",
                    "target_branch": "main", "draft": false, "sha": "head",
                    "source_project_id": 1, "target_project_id": 1}},
                format!("GET {project}/repository/commits/head/statuses?per_page=100&page=1"): {"body": [
                    {"id": 1, "name": "rust", "status": "success", "target_url": job},
                    {"id": 2, "name": "rust", "status": "failed", "target_url": job},
                    {"id": 3, "name": "coverage", "status": "failed", "target_url": external}
                ]},
                log.clone(): {"body": "assertion failed\n"},
            });
            (job, log, responses)
        } else {
            let job = "https://github.com/team/project/actions/runs/12/job/34";
            let log = "GET repos/team/project/actions/jobs/34/logs".to_owned();
            let mut open = pull(8, "open", "Fix", &[]);
            open["head"]["sha"] = json!("head");
            let responses = json!({
                "GET repos/team/project/pulls/8": {"body": open},
                "GET repos/team/project/commits/head/check-runs?filter=latest&per_page=100": {"body": {
                    "total_count": 1, "check_runs": [
                        {"name": "rust", "status": "completed", "conclusion": "failure", "details_url": job}
                    ]}},
                "GET repos/team/project/commits/head/status?per_page=100": {"body": {
                    "total_count": 1, "statuses": [
                        {"context": "coverage", "state": "failure", "target_url": external}
                    ]}},
                log.clone(): {"body": "assertion failed\n"},
            });
            (job, log, responses)
        };
        fixture.respond(responses);
        let read = ["view", "pr", "8", "--workspace", "topic", "--check"];
        let result = fixture.ok(&[&read[..], &["rust"]].concat());
        assert_eq!(result["job_url"], job, "{result}");
        assert_eq!(result["text"], "assertion failed\n", "{result}");
        let requests = fixture.requests();
        assert_eq!(requests.last().unwrap()[0], log.as_str());
        assert!(
            requests.iter().all(|request| request[2] == "agent"),
            "{requests:?}"
        );

        let output = fixture.run(&[&read[..], &["coverage"]].concat());
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("check coverage is not a CI job in this repository"),
            "{error}"
        );
        assert!(
            !fixture.requests().iter().any(|request| request[0]
                .as_str()
                .unwrap()
                .ends_with("/logs")
                || request[0].as_str().unwrap().ends_with("/trace"))
        );
    }
}

#[test]
fn check_log_downloads_are_bounded_even_when_curl_ignores_max_filesize() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    fixture.respond(failed_check_responses());
    support::install_test_script(
        fixture.root.path().join("bin/curl"),
        r#"#!/bin/sh
for arg; do url=$arg; done
case "$url" in
 */actions/jobs/*/logs)
  exec python3 -c 'import sys
while True:
 sys.stdout.buffer.write(b"x" * 65536)
';;
 *) exec /usr/bin/curl "$@";;
esac
"#,
    );
    let result = fixture.request(json!({"check_log": {
        "workspace": "topic", "selection": {"kind": "pr", "input": "8"}, "check": "rust"
    }}));
    let message = result["data"]["message"].as_str().unwrap();
    assert!(message.contains("maximum 8 MiB"), "{result}");
    assert!(
        message.contains("output exceeded 8388612 bytes"),
        "{result}"
    );
}

#[test]
fn view_prints_check_logs_and_reports_lookup_errors() {
    for available in [true, false] {
        let fixture = Fixture::new(Forge::Forgejo, "");
        let mut responses = failed_check_responses();
        if !available {
            responses["GET repos/team/project/actions/jobs/18345/logs"] =
                json!({"status": 404, "body": {"message": "logs unavailable"}});
        }
        fixture.respond(responses);
        if available {
            let log = fixture.ok(&["view", "pr", "8", "--workspace", "topic", "--check", "rust"]);
            assert_eq!(log["failed_step"], "Main Test");
            let human =
                fixture.run(&["view", "pr", "8", "--workspace", "topic", "--check", "rust"]);
            assert!(human.status.success());
            let human = String::from_utf8(human.stdout).unwrap();
            assert!(human.contains("Failed step: Main Test\n"), "{human}");
            let job = format!("Job: {}/actions/runs/2765/jobs/1\n", fixture.web);
            assert!(human.contains(&job), "{human}");
            assert!(human.contains("assertion failed\n"), "{human}");
        } else {
            let result =
                fixture.run(&["view", "pr", "8", "--workspace", "topic", "--check", "rust"]);
            assert!(!result.status.success());
            assert!(String::from_utf8_lossy(&result.stderr).contains("HTTP 404"));
        }
    }
}

#[test]
fn view_automatically_includes_failed_logs_and_leaves_successful_checks_quiet() {
    for outcome in ["failure", "unavailable", "success"] {
        let fixture = Fixture::new(Forge::Forgejo, "");
        let mut responses = failed_check_responses();
        responses["GET repos/team/project/pulls/8"]["body"] = json!({
            "number": 8, "title": "Fix crash", "state": "open", "body": "PR description",
            "head": {"sha": "head", "ref": "topic", "repo": {"full_name": "team/project"}},
            "base": {"ref": "main"}, "mergeable": true
        });
        responses["GET repos/team/project/issues/8/comments"] =
            json!({"body": [{"body": "Please fix CI", "user": {"login": "reviewer"}}]});
        responses["GET repos/team/project/pulls/8/reviews?limit=50&page=1"] = json!({"body": []});
        responses["GET repos/team/project/issues/8/timeline?limit=50&page=1"] = json!({"body": []});
        if outcome == "unavailable" {
            responses["GET repos/team/project/actions/jobs/18345/logs"] =
                json!({"status": 404, "body": {"message": "logs unavailable"}});
        }
        let result = if outcome == "success" {
            "success"
        } else {
            "failure"
        };
        responses["GET repos/team/project/commits/head/status?limit=50"] = json!({"body": {
            "total_count": 3, "statuses": [
                {"context": "rust", "status": result},
                {"context": "gate", "status": "success"},
                {"context": "queued", "status": "pending"}
            ]
        }});
        fixture.respond(responses);
        fixture.ok(&["link", "pr", "8", "assigned", "--workspace", "topic"]);
        fixture.requests();
        for no_comments in [false, true] {
            let mut args = vec!["view", "--workspace", "topic"];
            if no_comments {
                args.push("--no-comments");
            }
            let views = fixture.ok(&args);
            assert_eq!(views[0]["title"], "Fix crash");
            assert_eq!(views[0]["body"], "PR description");
            if !no_comments {
                assert_eq!(views[0]["comments"][0]["body"], "Please fix CI");
            }
            match outcome {
                "failure" => {
                    let logs = views[0]["check_logs"].as_array().unwrap();
                    assert_eq!(logs.len(), 1);
                    assert_eq!(logs[0]["name"], "rust");
                    assert_eq!(logs[0]["failed_step"], "Main Test");
                    assert!(
                        logs[0]["text"]
                            .as_str()
                            .unwrap()
                            .contains("assertion failed")
                    );
                }
                "unavailable" => assert!(
                    views[0]["errors"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(
                            |error| error.as_str().unwrap().contains("rust: log unavailable:")
                                && error.as_str().unwrap().contains("HTTP 404")
                        ),
                    "{views}"
                ),
                _ => assert!(views[0].get("check_logs").is_none(), "{views}"),
            }
            let output = fixture.run(&args);
            assert!(output.status.success(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            assert_eq!(text.contains("assertion failed"), outcome == "failure");
            assert_eq!(
                text.contains("Failed step: Main Test"),
                outcome == "failure"
            );
        }
        if outcome == "success" {
            assert!(
                !fixture
                    .requests()
                    .iter()
                    .any(|request| request[0].as_str().unwrap().ends_with("/logs"))
            );
        }
    }
}

#[test]
fn watch_reports_failed_steps_or_log_lookup_errors_without_hiding_comments() {
    for outcome in ["available", "unavailable", "external"] {
        let fixture = Fixture::new(Forge::Forgejo, "");
        let mut responses = failed_check_responses();
        let statuses = "GET repos/team/project/commits/head/statuses?limit=50&page=1";
        match outcome {
            "unavailable" => {
                responses["GET repos/team/project/actions/jobs/18345/logs"] =
                    json!({"status": 404, "body": {"message": "logs unavailable"}});
            }
            "external" => {
                responses[statuses]["body"][0]["target_url"] = json!("https://ci.example/build/1");
            }
            _ => {}
        }
        responses["GET repos/team/project/pulls/8"]["body"] = json!({
            "number": 8, "title": "Fix crash", "state": "open",
            "head": {"sha": "head", "ref": "topic", "repo": {"full_name": "team/project"}},
            "base": {"ref": "main"}, "mergeable": true
        });
        responses["GET repos/team/project/commits/head/status?limit=50"] = json!({"body": {
            "total_count": 1, "statuses": [{"context": "rust", "status": "failure"}]
        }});
        responses["GET repos/team/project/issues/8/comments?limit=50&page=1"] =
            json!({"body": [{"id": 1, "body": "A review is ready", "user": {"login": "bot"}}]});
        responses["GET repos/team/project/pulls/8/reviews?limit=50&page=1"] = json!({"body": []});
        fixture.respond(responses);
        let result = fixture.ok(&[
            "watch",
            "pr",
            "8",
            "--workspace",
            "topic",
            "--timeout",
            "60",
        ]);
        let updates = result["updates"].as_array().unwrap();
        assert!(
            updates.iter().any(|update| update["kind"] == "comment"),
            "{result}"
        );
        let ci = updates
            .iter()
            .find(|update| update["kind"] == "ci_completed")
            .unwrap();
        let message = ci["message"].as_str().unwrap();
        match outcome {
            "external" => assert_eq!(message, "rust: Failure"),
            "available" => assert!(message.contains("failed step: Main Test"), "{message}"),
            _ => assert!(message.contains("log unavailable: HTTP 404"), "{message}"),
        }
        if outcome != "external" {
            assert!(message.starts_with("rust: Failure; "), "{message}");
            assert!(message.contains("--check 'rust'"), "{message}");
        }
        assert_eq!(ci["check"], json!({"name": "rust", "result": "failure"}));
    }
}

#[test]
fn github_views_and_watches_read_timed_out_jobs_but_not_commit_statuses() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let job = "https://github.com/team/project/actions/runs/12/job/34";
    let external = "https://ci.example/build/1";
    let mut open = pull(8, "open", "Fix", &[]);
    open["head"]["sha"] = json!("head");
    fixture.respond(json!({
        "pr view 8": {"body": {"number": 8, "state": "OPEN", "title": "Fix", "body": "",
            "author": {"login": "agent"}, "labels": [], "createdAt": "t0",
            "headRefName": "topic", "baseRefName": "main", "isDraft": false, "commits": []}},
        "GET repos/team/project/pulls/8": {"body": open},
        "GET repos/team/project/issues/8/comments?per_page=100&page=1": {"body": []},
        "GET repos/team/project/pulls/8/reviews?per_page=100&page=1": {"body": []},
        "GET repos/team/project/pulls/8/comments?per_page=100&page=1": {"body": []},
        "GET repos/team/project/commits/head/check-runs?filter=latest&per_page=100": {"body": {
            "total_count": 1, "check_runs": [
                {"name": "rust", "status": "completed", "conclusion": "timed_out", "details_url": job}
            ]}},
        "GET repos/team/project/commits/head/status?per_page=100": {"body": {
            "total_count": 1, "statuses": [
                {"context": "deploy", "state": "failure", "target_url": external}
            ]}},
        "GET repos/team/project/actions/jobs/34/logs": {"body": "assertion failed\n"},
    }));
    fixture.ok(&["link", "pr", "8", "assigned", "--workspace", "topic"]);
    let views = fixture.ok(&["view", "pr", "--no-comments", "--workspace", "topic"]);
    let logs = views[0]["check_logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1, "{views}");
    assert_eq!(logs[0]["name"], "rust");
    assert_eq!(logs[0]["text"], "assertion failed\n");
    let result = fixture.ok(&["watch", "pr", "--workspace", "topic", "--timeout", "60"]);
    let message = |name: &str| {
        result["updates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|update| update["check"]["name"] == name)
            .and_then(|update| update["message"].as_str())
            .unwrap_or_else(|| panic!("no update for {name}: {result}"))
            .to_owned()
    };
    let rust = message("rust");
    assert!(
        rust.starts_with("rust: TIMED_OUT; read the log with"),
        "{rust}"
    );
    assert!(rust.contains("--check 'rust'"), "{rust}");
    assert_eq!(message("deploy"), "deploy: FAILURE");
    assert_eq!(result["updates"].as_array().unwrap().len(), 2, "{result}");
}

#[test]
fn ci_actions_rerun_failed_jobs_and_cancel_running_runs_on_the_head() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let mut open = pull(8, "open", "Fix", &[]);
    open["head"]["sha"] = json!("head");
    let job = |run: u64, job: u64| {
        format!("https://github.com/team/project/actions/runs/{run}/job/{job}")
    };
    fixture.respond(json!({
        "GET repos/team/project/pulls/8": {"body": open},
        "GET repos/team/project/commits/head/check-runs?filter=latest&per_page=100": {"body": {
            "total_count": 5, "check_runs": [
                {"name": "rust", "status": "completed", "conclusion": "failure", "details_url": job(1, 11)},
                {"name": "coverage", "status": "completed", "conclusion": "failure", "details_url": "https://coverage.example/7"},
                {"name": "docs", "status": "completed", "conclusion": "cancelled", "details_url": job(1, 12)},
                {"name": "lint", "status": "in_progress", "conclusion": null, "details_url": job(2, 21)},
                {"name": "fmt", "status": "queued", "conclusion": null, "details_url": job(2, 22)}
            ]}},
        "GET repos/team/project/commits/head/status?per_page=100":
            {"body": {"total_count": 0, "statuses": []}},
        "POST repos/team/project/actions/runs/1/rerun-failed-jobs": {"status": 201},
        "POST repos/team/project/actions/jobs/11/rerun": {"status": 201},
        "POST repos/team/project/actions/runs/2/cancel": {"status": 202},
    }));
    let ci = |action: &str, checks: Value| {
        fixture.request(json!({"ci_action": {"workspace": "topic", "item": "8",
            "action": action, "checks": checks}}))
    };
    let posts = || {
        fixture
            .requests()
            .into_iter()
            .map(|request| request[0].as_str().unwrap().to_owned())
            .filter(|key| key.starts_with("POST"))
            .collect::<Vec<_>>()
    };
    let url = format!("{}/pull/8", fixture.web);

    let rerun = ci("rerun", json!([]));
    assert_eq!(rerun["type"], "ci_acted", "{rerun}");
    assert_eq!(
        rerun["data"],
        json!({"url": url, "action": "rerun", "checks": ["rust", "docs"]})
    );
    // Other providers' checks are left alone, and failed jobs of one run
    // rerun together, since rerunning one starts a new attempt of the run.
    assert_eq!(
        posts(),
        ["POST repos/team/project/actions/runs/1/rerun-failed-jobs"]
    );

    // Jobs of one run share its cancellation.
    let cancel = ci("cancel", json!(["lint", "fmt"]));
    assert_eq!(cancel["data"]["checks"], json!(["lint", "fmt"]), "{cancel}");
    assert_eq!(posts(), ["POST repos/team/project/actions/runs/2/cancel"]);
    let event = fixture
        .events()
        .into_iter()
        .rfind(|event| event["kind"] == "item_changed")
        .unwrap();
    assert_eq!(
        event["item"],
        json!({"kind": "pr", "url": url, "action": "cancel"})
    );

    // A named check must be in the state the action needs.
    let refused = ci("rerun", json!(["lint"]));
    assert!(
        refused["data"]["message"]
            .as_str()
            .unwrap()
            .contains("check lint is not failed"),
        "{refused}"
    );
    assert!(posts().is_empty());

    let rerun = fixture.ok(&[
        "ci",
        "rerun",
        "--pr",
        "8",
        "--check",
        "rust",
        "--workspace",
        "topic",
    ]);
    assert_eq!(rerun["checks"], json!(["rust"]), "{rerun}");
    assert_eq!(posts(), ["POST repos/team/project/actions/jobs/11/rerun"]);

    // A registered checkout acts too, but links nothing to default to.
    let repository = fixture.ok(&["status", "topic"])["workspace"]["repository_id"].clone();
    let in_checkout = |item: Value| {
        fixture.request(json!({"ci_action": {"checkout": repository, "item": item,
            "action": "rerun", "checks": ["rust"]}}))
    };
    let rerun = in_checkout(json!("8"));
    assert_eq!(rerun["data"]["checks"], json!(["rust"]), "{rerun}");
    assert_eq!(posts(), ["POST repos/team/project/actions/jobs/11/rerun"]);
    let refused = in_checkout(Value::Null);
    assert!(
        refused.to_string().contains("nothing is linked"),
        "{refused}"
    );
}

#[test]
fn forgejo_ci_cancels_runs_as_the_scoped_agent_but_cannot_rerun() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let mut open = pull(8, "open", "Fix", &[]);
    open["head"]["sha"] = json!("head");
    fixture.respond(json!({
        "GET repos/team/project/pulls/8": {"body": open},
        "GET repos/team/project/commits/head/status?limit=50": {"body": {"total_count": 3, "statuses": [
            {"context": "rust", "status": "pending", "target_url": "/team/project/actions/runs/2765/jobs/1"},
            {"context": "docs", "status": "pending", "target_url": "/team/project/actions/runs/2765/jobs/0"},
            {"context": "lint", "status": "failure", "target_url": "/team/project/actions/runs/2764/jobs/0"}
        ]}},
        "GET repos/team/project/actions/runs?run_number=2765": {"body": {"workflow_runs": [
            {"id": 9331, "index_in_repo": 2765, "commit_sha": "head"}
        ]}},
        "POST repos/team/project/actions/runs/9331/cancel": {"status": 204},
    }));
    let exported = fixture.ok(&["work", "env", "topic"]);
    let scope = exported["SHOAL_SCOPE_TOKEN"].as_str().unwrap();
    let ci = |action: &str| {
        fixture.request_as(
            json!({"ci_action": {"workspace": "topic", "item": "8", "action": action,
                "checks": []}}),
            Some(scope),
        )
    };
    let cancel = ci("cancel");
    assert_eq!(
        cancel["data"]["checks"],
        json!(["rust", "docs"]),
        "{cancel}"
    );
    let posts = fixture
        .requests()
        .into_iter()
        .filter(|request| request[0].as_str().unwrap().starts_with("POST"))
        .collect::<Vec<_>>();
    assert_eq!(
        posts,
        [json!([
            "POST repos/team/project/actions/runs/9331/cancel",
            null,
            "token usertoken"
        ])]
    );

    let rerun = ci("rerun");
    assert!(
        rerun["data"]["message"]
            .as_str()
            .unwrap()
            .contains("Shoal cannot rerun CI jobs on Forgejo or Gitea"),
        "{rerun}"
    );
    assert!(fixture.requests().is_empty());
}

#[test]
fn ci_runs_start_on_the_workspace_branch_with_inputs() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let dispatch = "POST repos/team/project/actions/workflows/deps.yml/dispatches";
    fixture.respond(json!({dispatch: {"status": 204}}));
    let started = fixture.ok(&[
        "ci",
        "run",
        "deps.yml",
        "--workspace",
        "topic",
        "--input",
        "level=full",
        "--input",
        "note=a=b",
    ]);
    assert_eq!(
        started,
        json!({"workflow": "deps.yml", "branch": "topic"}),
        "{started}"
    );
    assert_eq!(
        writes(&fixture),
        [json!([dispatch, {"ref": "topic", "inputs": {"level": "full", "note": "a=b"}}])]
    );

    // Workspace processes start runs only on their own branch.
    let exported = fixture.ok(&["work", "env", "topic"]);
    let scope = exported["SHOAL_SCOPE_TOKEN"].as_str().unwrap();
    let start = |branch: &str| {
        fixture.request_as(
            json!({"start_ci_run": {"workspace": "topic", "workflow": "deps.yml",
                "branch": branch, "inputs": {}}}),
            Some(scope),
        )
    };
    let refused = start("main");
    assert!(
        refused["data"]["message"]
            .as_str()
            .unwrap()
            .contains("only on their workspace's branch"),
        "{refused}"
    );
    assert!(fixture.requests().is_empty());
    let started = start("topic");
    assert_eq!(started["type"], "ci_run_started", "{started}");
    assert_eq!(
        writes(&fixture),
        [json!([dispatch, {"ref": "topic", "inputs": {}}])]
    );
    let unscoped = fixture.request(json!({"start_ci_run": {"workspace": "topic",
        "workflow": "deps.yml", "branch": "main", "inputs": {}}}));
    assert_eq!(unscoped["data"]["branch"], "main", "{unscoped}");

    // Bad arguments are refused before the workspace picker.
    for (args, error) in [
        (&["--branch", " "], "branch must be nonempty"),
        (&["--input", " =x"], "expected NAME=VALUE"),
    ] {
        let output = fixture.run(&[&["ci", "run", "deps.yml"][..], args].concat());
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{output:?}"
        );
    }
}

#[test]
fn workflow_links_start_runs_in_their_repository_on_their_branch() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let own = "POST repos/team/project/actions/workflows/deps.yml/dispatches";
    let other = "POST repos/other/tool/actions/workflows/ci.yml/dispatches";
    fixture.respond(json!({
        own: {"status": 204},
        other: {"status": 204},
        "GET repos/other/tool": {"body": {"full_name": "other/tool", "default_branch": "trunk"}},
    }));
    // A workflow's page in the workspace's repository runs on its branch.
    let page = "https://github.com/team/project/actions/workflows/deps.yml";
    let started = fixture.ok(&["ci", "run", page, "--workspace", "topic"]);
    assert_eq!(
        started,
        json!({"workflow": "deps.yml", "branch": "topic"}),
        "{started}"
    );
    // A workflow file's URL also names the branch.
    let file = "https://github.com/team/project/blob/release/1/.github/workflows/deps.yml";
    let started = fixture.ok(&["ci", "run", file, "--workspace", "topic"]);
    assert_eq!(started["branch"], "release/1", "{started}");
    assert_eq!(
        writes(&fixture),
        [
            json!([own, {"ref": "topic", "inputs": {}}]),
            json!([own, {"ref": "release/1", "inputs": {}}])
        ]
    );
    // Outside a workspace, another repository's run starts on its default
    // branch.
    let started = fixture.ok(&[
        "ci",
        "run",
        "https://github.com/other/tool/actions/workflows/ci.yml",
    ]);
    assert_eq!(
        started,
        json!({"workflow": "ci.yml", "branch": "trunk"}),
        "{started}"
    );
    assert_eq!(
        writes(&fixture),
        [json!([other, {"ref": "trunk", "inputs": {}}])]
    );
    let output = fixture.run(&["ci", "run", "https://github.com/team/project/pull/8"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expected a workflow URL"),
        "{output:?}"
    );
}

#[test]
fn forgejo_workflow_links_of_another_repository_link_the_run_for_the_user_only() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let dispatch = "POST repos/other/project/actions/workflows/deps.yml/dispatches";
    fixture.respond(json!({
        "GET repos/other/project": {"body": {"default_branch": "main"}},
        dispatch: {"status": 201, "body": {"id": 50, "run_number": 5}},
    }));
    let other = fixture.web.replace("/team/project", "/other/project");
    let page = format!("{other}/actions?workflow=deps.yml");
    let started = fixture.ok(&["ci", "run", &page, "--workspace", "topic"]);
    assert_eq!(
        (&started["branch"], &started["url"], &started["linked"]),
        (
            &json!("main"),
            &json!(format!("{other}/actions/runs/5")),
            &json!(true)
        ),
        "{started}"
    );
    let links = &fixture.ok(&["status", "topic"])["workspace"]["links"];
    assert_eq!(links["opened"][0]["url"], started["url"], "{links}");
    assert_eq!(
        writes(&fixture),
        [json!([dispatch, {"ref": "main", "inputs": {}, "return_run_info": true}])]
    );

    // Workspace processes start runs only in their repository, on their branch.
    let exported = fixture.ok(&["work", "env", "topic"]);
    let scope = exported["SHOAL_SCOPE_TOKEN"].as_str().unwrap();
    let own_file = format!(
        "{}/src/branch/main/.forgejo/workflows/deps.yml",
        fixture.web
    );
    for (workflow, error) in [
        (page.as_str(), "of their own repository"),
        (own_file.as_str(), "only on their workspace's branch"),
    ] {
        let refused = fixture.request_as(
            json!({"start_ci_run": {"workflow": workflow, "branch": null, "inputs": {}}}),
            Some(scope),
        );
        assert!(
            refused["data"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains(error),
            "{workflow}: {refused}"
        );
    }
    assert!(writes(&fixture).is_empty());
}

#[test]
fn forgejo_ci_runs_link_the_run_they_start() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let dispatch = "POST repos/team/project/actions/workflows/deps.yml/dispatches";
    fixture.respond(json!({
        dispatch: {"status": 201, "body": {"id": 9331, "run_number": 2765, "jobs": ["audit"]}},
    }));
    let started = fixture.ok(&["ci", "run", "deps.yml", "--workspace", "topic"]);
    assert_eq!(
        started["url"],
        format!("{}/actions/runs/2765", fixture.web),
        "{started}"
    );
    assert_eq!(
        writes(&fixture),
        [json!([dispatch, {"ref": "topic", "inputs": {}, "return_run_info": true}])]
    );
    // The run is linked as opened, so watches follow it.
    assert_eq!(started["linked"], true, "{started}");
    let links = &fixture.ok(&["status", "topic"])["workspace"]["links"];
    assert_eq!(
        links["opened"],
        json!([{"kind": "ci", "url": started["url"], "title": "deps.yml"}]),
        "{links}"
    );
    let output = fixture.run(&["ci", "run", "--workspace", "topic"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("starts a named workflow"),
        "{output:?}"
    );
}

#[test]
fn linked_runs_are_viewed_and_watched_until_they_finish() {
    let fixture = Fixture::new(Forge::Forgejo, "[cleanup.auto]\nenabled=false\n");
    let respond = |status: &str| {
        fixture.respond(json!({
            "GET repos/team/project/actions/runs?run_number=2765": {"body": {"workflow_runs": [
                {"id": 9331, "index_in_repo": 2765, "prettyref": "main",
                    "workflow_id": "flaky.yml", "status": status}
            ]}},
            "GET repos/team/project/actions/runs/9331/jobs": {"body": [
                {"id": 18345, "name": "test", "run_id": 9331, "status": status}
            ]},
            "GET repos/team/project/actions/jobs/18345/logs":
                {"body": "⭐ Run Tests\nboom\n⚙️ [runner]: exitcode '1': failure\n"},
        }))
    };
    respond("running");
    let url = format!("{}/actions/runs/2765", fixture.web);
    // Assigned items drive completion, so a run is only opened or related.
    let output = fixture.run(&["link", "ci", "2765", "assigned", "--workspace", "topic"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot be assigned"),
        "{output:?}"
    );
    let job = format!("{url}/jobs/0");
    fixture.ok(&["link", &job, "related", "--workspace", "topic"]);
    let viewed = fixture.ok(&["view", "--workspace", "topic"]);
    assert_eq!(
        (&viewed[0]["kind"], &viewed[0]["url"], &viewed[0]["state"]),
        (&json!("ci"), &json!(url), &json!("running")),
        "{viewed}"
    );

    // A running run has nothing to report, so this wait can only time out.
    let running = fixture.ok(&["watch", "ci", "--workspace", "topic", "--timeout", "1"]);
    assert_eq!(running["updates"], json!([]), "{running}");
    respond("failure");
    let finished = fixture.ok(&["watch", "ci", "--workspace", "topic", "--timeout", "600"]);
    let update = &finished["updates"][0];
    assert_eq!(
        (&update["url"], &update["kind"], &update["message"]),
        (
            &json!(url),
            &json!("ci_completed"),
            &json!(format!(
                "Run failed: test (failed step: Tests); read it with shoal view ci {url}"
            ))
        ),
        "{finished}"
    );
    // The run's watch has ended, but it stays linked.
    let output = fixture.run(&["watch", "ci", "--workspace", "topic", "--timeout", "600"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no linked items"),
        "{output:?}"
    );
    let viewed = fixture.ok(&["view", "ci", "--workspace", "topic"]);
    assert_eq!(viewed[0]["state"], "failed", "{viewed}");
    let output = fixture.run(&["ready", "ci", "--workspace", "topic"]);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not marked ready"),
        "{output:?}"
    );
    fixture.ok(&["unlink", "ci", "2765", "related", "--workspace", "topic"]);
    let links = &fixture.ok(&["status", "topic"])["workspace"]["links"];
    assert_eq!(links["related"], json!([]), "{links}");
    // Unlinking drops the run's cursor, so linking it again watches it afresh.
    fixture.ok(&["link", "ci", "2765", "opened", "--workspace", "topic"]);
    let again = fixture.ok(&["watch", "ci", "--workspace", "topic", "--timeout", "600"]);
    assert_eq!(again["updates"][0]["kind"], "ci_completed", "{again}");
}

#[test]
fn github_reports_runs_waiting_for_approval_and_approves_them_as_the_user() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let mut open = pull(8, "open", "Fix", &[]);
    open["head"]["sha"] = json!("head");
    let run = |id: u64, conclusion: &str| {
        json!({"id": id, "head_branch": "contributor", "status": "completed",
            "conclusion": conclusion, "path": ".github/workflows/ci.yml",
            "html_url": format!("https://github.com/team/project/actions/runs/{id}")})
    };
    let url = |id: u64| format!("https://github.com/team/project/actions/runs/{id}");
    fixture.respond(json!({
        "pr view 8": {"body": {"number": 8, "state": "OPEN", "title": "Fix", "body": "",
            "author": {"login": "contributor"}, "labels": [], "createdAt": "t0",
            "headRefName": "topic", "baseRefName": "main", "isDraft": false,
            "headRefOid": "head", "baseRefOid": "base", "commits": [], "comments": [],
            "reviews": [], "mergeable": "MERGEABLE", "statusCheckRollup": []}},
        "GET repos/team/project/pulls/8/comments": {"body": [[]]},
        "GET repos/team/project/pulls/8": {"body": open},
        "GET repos/team/project/actions/runs?head_sha=head&status=action_required&per_page=50&page=1":
            {"body": {"total_count": 1, "workflow_runs": [run(21, "action_required")]}},
        "GET repos/team/project/actions/runs/21": {"body": run(21, "action_required")},
        "GET repos/team/project/actions/runs/22": {"body": run(22, "success")},
        "POST repos/team/project/actions/runs/21/approve": {"status": 201},
    }));
    let waiting = json!([{"url": url(21), "workflow": "ci.yml"}]);
    let view = &fixture.ok(&["view", "pr", "8", "--no-comments", "--workspace", "topic"])[0];
    assert_eq!(view["pr"]["checks"], json!([]), "{view}");
    assert_eq!(view["pr"]["waiting_runs"], waiting, "{view}");
    let output = fixture.run(&["view", "pr", "8", "--no-comments", "--workspace", "topic"]);
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("1 CI run waits for approval; approve it with shoal ci approve --pr 8"),
        "{output:?}"
    );
    fixture.ok(&["link", "pr", "8", "assigned", "--workspace", "topic"]);
    let status = fixture.ok(&["status", "topic"]);
    assert_eq!(status["prs"][0]["waiting_runs"], waiting, "{status}");
    fixture.requests();

    // Approving lets a contributor's code run, so it is the user's decision.
    let shoal = env!("CARGO_BIN_EXE_shoal");
    let approve = ["ci", "approve", "--pr", "8"];
    let scoped =
        fixture.run(&[&["run", "--workspace", "topic", "--", shoal][..], &approve].concat());
    assert!(
        String::from_utf8_lossy(&scoped.stderr)
            .contains("approving CI runs is the user's decision"),
        "{scoped:?}"
    );
    assert!(fixture.requests().is_empty());

    let approved = fixture.ok(&["ci", "approve", "--workspace", "topic"]);
    assert_eq!(approved, json!({"approved": [url(21)]}));
    let posts = fixture
        .requests()
        .into_iter()
        .filter(|request| request[0].as_str().unwrap().starts_with("POST"))
        .collect::<Vec<_>>();
    assert_eq!(
        posts,
        [json!([
            "POST repos/team/project/actions/runs/21/approve",
            null,
            null
        ])]
    );
    let output = fixture.run(&["ci", "approve", "22", "--workspace", "topic"]);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not wait for approval"),
        "{output:?}"
    );
    assert!(writes(&fixture).is_empty());
}

#[test]
fn a_watched_run_waiting_for_approval_reports_once_it_has_run() {
    let fixture = Fixture::new(Forge::GitHub, "[cleanup.auto]\nenabled=false\n");
    let url = "https://github.com/team/project/actions/runs/21";
    let respond = |conclusion: &str| {
        fixture.respond(json!({
            "GET repos/team/project/actions/runs/21": {"body": {"id": 21,
                "head_branch": "contributor", "status": "completed", "conclusion": conclusion,
                "html_url": url}},
        }))
    };
    // GitHub reports a held run as completed, but it has yet to run.
    respond("action_required");
    fixture.ok(&["link", "ci", url, "related", "--workspace", "topic"]);
    let held = fixture.ok(&["watch", "ci", "--workspace", "topic", "--timeout", "1"]);
    assert_eq!(held["updates"], json!([]), "{held}");
    respond("success");
    let finished = fixture.ok(&["watch", "ci", "--workspace", "topic", "--timeout", "600"]);
    assert_eq!(
        finished["updates"][0]["message"],
        format!("Run finished; read it with shoal view ci {url}"),
        "{finished}"
    );
}

#[test]
fn forgejo_lists_runs_waiting_for_approval_but_leaves_approving_to_its_web_ui() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let mut open = pull(8, "open", "Fix", &[]);
    open["head"]["sha"] = json!("head");
    let run = |number: u64, need_approval: bool| {
        json!({"id": 9330 + number, "index_in_repo": number, "prettyref": "contributor",
            "workflow_id": "ci.yml", "status": "blocked", "need_approval": need_approval})
    };
    fixture.respond(json!({
        "GET repos/team/project/pulls/8": {"body": open},
        "GET repos/team/project/commits/head/status?limit=50": {"body": {"total_count": 0, "statuses": []}},
        "GET repos/team/project/pulls/8/reviews?limit=50&page=1": {"body": []},
        "GET repos/team/project/issues/8/timeline?limit=50&page=1": {"body": []},
        // The waiting run is on the second page, after a full first page.
        "GET repos/team/project/actions/runs?limit=50&page=1&head_sha=head": {"body": {
            "workflow_runs": (2700..2750).map(|number| run(number, false)).collect::<Vec<_>>(),
            "total_count": 51}},
        "GET repos/team/project/actions/runs?limit=50&page=2&head_sha=head":
            {"body": {"workflow_runs": [run(2766, true)], "total_count": 51}},
    }));
    let view = &fixture.ok(&["view", "pr", "8", "--no-comments", "--workspace", "topic"])[0];
    assert_eq!(
        view["pr"]["waiting_runs"],
        json!([{"url": format!("{}/actions/runs/2766", fixture.web), "workflow": "ci.yml"}]),
        "{view}"
    );
    let output = fixture.run(&["view", "pr", "8", "--no-comments", "--workspace", "topic"]);
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("1 CI run waits for approval; approve it in the forge's web UI"),
        "{output:?}"
    );
    fixture.requests();
    let output = fixture.run(&["ci", "approve", "--pr", "8", "--workspace", "topic"]);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "Shoal cannot approve CI runs on Forgejo or Gitea; approve them in the web UI"
        ),
        "{output:?}"
    );
    assert!(fixture.requests().is_empty());
}

#[test]
fn ci_run_actions_rerun_and_cancel_runs_of_the_workspace_branch() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let run = |id: u64, branch: &str| {
        json!({"body": {"id": id, "head_branch": branch, "status": "completed",
            "conclusion": "failure",
            "html_url": format!("https://github.com/team/project/actions/runs/{id}")}})
    };
    fixture.respond(json!({
        "GET repos/team/project/actions/runs/12": run(12, "topic"),
        "GET repos/team/project/actions/runs/13": run(13, "main"),
        "POST repos/team/project/actions/runs/12/rerun-failed-jobs": {"status": 201},
        "POST repos/team/project/actions/runs/12/cancel": {"status": 202},
        "POST repos/team/project/actions/runs/13/rerun": {"status": 201},
    }));
    let url = "https://github.com/team/project/actions/runs/12";
    let rerun = fixture.ok(&[
        "ci",
        "rerun",
        &format!("{url}/job/34"),
        "--failed",
        "--workspace",
        "topic",
    ]);
    assert_eq!(rerun, json!({"url": url, "action": "rerun"}));
    assert_eq!(
        writes(&fixture),
        [json!([
            "POST repos/team/project/actions/runs/12/rerun-failed-jobs",
            null
        ])]
    );

    // Workspace processes act only on runs of their own branch.
    let exported = fixture.ok(&["work", "env", "topic"]);
    let scope = exported["SHOAL_SCOPE_TOKEN"].as_str().unwrap();
    let act = |run: &str, action: &str, scope| {
        fixture.request_as(
            json!({"ci_run_action": {"workspace": "topic", "run": run, "action": action,
                "failed": false}}),
            scope,
        )
    };
    let cancel = act("12", "cancel", Some(scope));
    assert_eq!(cancel["type"], "ci_run_acted", "{cancel}");
    assert_eq!(
        writes(&fixture),
        [json!([
            "POST repos/team/project/actions/runs/12/cancel",
            null
        ])]
    );
    let refused = act("13", "rerun", Some(scope));
    assert!(
        refused["data"]["message"]
            .as_str()
            .unwrap()
            .contains("only on CI runs of their workspace's branch"),
        "{refused}"
    );
    assert!(writes(&fixture).is_empty());
    let unscoped = act("13", "rerun", None);
    assert_eq!(unscoped["type"], "ci_run_acted", "{unscoped}");
    assert_eq!(
        writes(&fixture),
        [json!([
            "POST repos/team/project/actions/runs/13/rerun",
            null
        ])]
    );

    // Workspace processes act on no run of another repository, and nothing
    // names a run but its URL or number.
    let other = act(
        "https://github.com/other/project/actions/runs/12",
        "cancel",
        Some(scope),
    );
    assert!(
        other["data"]["message"]
            .as_str()
            .unwrap()
            .contains("only on CI runs and workflows of their own repository"),
        "{other}"
    );
    assert!(fixture.requests().is_empty());
    let output = fixture.run(&["ci", "cancel", "ci.yml"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("by its web URL"),
        "{output:?}"
    );
}

#[test]
fn runs_of_another_repository_are_read_linked_and_acted_on_by_the_user() {
    let fixture = Fixture::new(Forge::Forgejo, "[cleanup.auto]\nenabled=false\n");
    fixture.respond(json!({
        "GET repos/other/project/actions/runs?run_number=7": {"body": {"workflow_runs": [
            {"id": 70, "index_in_repo": 7, "prettyref": "main", "workflow_id": "deps.yml",
                "status": "failure"}
        ]}},
        "GET repos/other/project/actions/runs/70/jobs": {"body": [
            {"id": 701, "name": "audit", "run_id": 70, "status": "failure"}
        ]},
        "GET repos/other/project/actions/jobs/701/logs": {"body": "advisory found\n"},
        "POST repos/other/project/actions/runs/70/cancel": {"status": 204},
    }));
    let other = format!(
        "{}/actions/runs/7",
        fixture.web.replace("/team/project", "/other/project")
    );

    // Outside a workspace, the run's URL names its repository.
    let view = fixture.ok(&["view", "ci", &other]);
    assert_eq!(
        (&view["url"], &view["branch"]),
        (&json!(other), &json!("main"))
    );
    assert_eq!(view["logs"][0]["text"], "advisory found\n", "{view}");
    let cancelled = fixture.ok(&["ci", "cancel", &other]);
    assert_eq!(cancelled["url"], other, "{cancelled}");
    assert_eq!(
        writes(&fixture),
        [json!([
            "POST repos/other/project/actions/runs/70/cancel",
            null
        ])]
    );

    // Inside one, it is viewed, linked and watched like a run of its own.
    let viewed = fixture.ok(&["view", &other, "--workspace", "topic"]);
    assert_eq!(viewed[0]["url"], other, "{viewed}");
    fixture.ok(&["link", &other, "related", "--workspace", "topic"]);
    let links = &fixture.ok(&["status", "topic"])["workspace"]["links"];
    assert_eq!(
        links["related"],
        json!([{"kind": "ci", "url": other, "title": "deps.yml"}]),
        "{links}"
    );
    fixture.ok(&["unlink", &other, "related", "--workspace", "topic"]);

    // Workspace processes read, link and act only within their repository.
    let exported = fixture.ok(&["work", "env", "topic"]);
    let scope = exported["SHOAL_SCOPE_TOKEN"].as_str().unwrap();
    for method in [
        json!({"view_ci_run": {"run": other}}),
        json!({"ci_run_jobs": {"workspace": "topic", "run": other}}),
        json!({"ci_run_action": {"run": other, "action": "cancel", "failed": false}}),
        json!({"link_item": {"workspace": "topic", "kind": "ci", "url": other,
            "relation": "related"}}),
    ] {
        let refused = fixture.request_as(method.clone(), Some(scope));
        assert!(
            refused["data"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("of their own repository"),
            "{method}: {refused}"
        );
    }
    assert!(writes(&fixture).is_empty());
}

#[test]
fn forgejo_ci_run_actions_cancel_by_run_number_but_cannot_rerun() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    fixture.respond(json!({
        "GET repos/team/project/actions/runs?run_number=2765": {"body": {"workflow_runs": [
            {"id": 9331, "index_in_repo": 2765, "prettyref": "topic", "status": "running"}
        ]}},
        "POST repos/team/project/actions/runs/9331/cancel": {"status": 204},
    }));
    let url = format!("{}/actions/runs/2765", fixture.web);
    let cancel = fixture.ok(&["ci", "cancel", &url, "--workspace", "topic"]);
    assert_eq!(cancel["url"], url, "{cancel}");
    assert_eq!(
        writes(&fixture),
        [json!([
            "POST repos/team/project/actions/runs/9331/cancel",
            null
        ])]
    );
    let output = fixture.run(&["ci", "rerun", "2765", "--workspace", "topic"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Shoal cannot rerun CI jobs on Forgejo or Gitea"),
        "{output:?}"
    );
    assert!(fixture.requests().is_empty());
}

#[test]
fn forgejo_ci_runs_are_viewed_with_failed_logs_and_listed_by_branch_or_pr() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let mut head_pull = pull(8, "open", "Fix", &[]);
    head_pull["head"]["sha"] = json!("head");
    let run = json!({"id": 9331, "index_in_repo": 2765, "prettyref": "topic",
        "workflow_id": "deps.yml", "status": "failure"});
    fixture.respond(json!({
        "GET repos/team/project/actions/runs?run_number=2765": {"body": {"workflow_runs": [run]}},
        "GET repos/team/project/actions/runs/9331/jobs": {"body": [
            {"id": 18344, "name": "build", "run_id": 9331, "status": "success"},
            {"id": 18345, "name": "audit", "run_id": 9331, "status": "failure"}
        ]},
        "GET repos/team/project/actions/jobs/18345/logs":
            {"body": "⭐ Run Audit\nadvisory found\n⚙️ [runner]: exitcode '1': failure\n"},
        "GET repos/team/project/actions/jobs/18344/logs": {"body": "built\n"},
        "GET repos/team/project/actions/runs?limit=5&page=1&ref=refs%2Fheads%2Ftopic&workflow_id=deps.yml":
            {"body": {"workflow_runs": [run], "total_count": 1}},
        "GET repos/team/project/pulls/8": {"body": head_pull},
        "GET repos/team/project/actions/runs?limit=10&page=1&head_sha=head":
            {"body": {"workflow_runs": [run], "total_count": 1}},
    }));
    let url = format!("{}/actions/runs/2765", fixture.web);
    let view = fixture.ok(&["view", "ci", "2765", "--workspace", "topic"]);
    assert_eq!(view["url"], url, "{view}");
    assert_eq!(
        (&view["state"], &view["branch"], &view["workflow"]),
        (&json!("failed"), &json!("topic"), &json!("deps.yml"))
    );
    assert_eq!(
        view["jobs"],
        json!([
            {"id": 18344, "name": "build", "state": "finished", "url": format!("{url}/jobs/0")},
            {"id": 18345, "name": "audit", "state": "failed", "url": format!("{url}/jobs/1")}
        ])
    );
    // Only the failed job's log is read.
    let logs = view["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1, "{view}");
    assert_eq!(logs[0]["name"], "audit");
    assert_eq!(logs[0]["failed_step"], "Audit");
    assert_eq!(logs[0]["job_url"], format!("{url}/jobs/1"));

    let log = fixture.ok(&["view", "ci", &url, "--job", "build", "--workspace", "topic"]);
    assert_eq!(log["text"], "built\n", "{log}");

    // The workspace's branch is listed unless another is named.
    let runs = fixture.ok(&[
        "ci",
        "list",
        "--workflow",
        "deps.yml",
        "--limit",
        "5",
        "--workspace",
        "topic",
    ]);
    assert_eq!(
        runs,
        json!([{"url": url, "branch": "topic", "workflow": "deps.yml", "state": "failed"}])
    );
    // A PR's runs are those on its head commit.
    let runs = fixture.ok(&["ci", "list", "--pr", "#8", "--workspace", "topic"]);
    assert_eq!(runs[0]["url"], url, "{runs}");
    let output = fixture.run(&["ci", "list", "--pr", "8", "--branch", "topic"]);
    assert!(!output.status.success());
    let output = fixture.run(&["ci", "list", "--limit", "0"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("limit must be between 1 and 100"),
        "{output:?}"
    );
}

#[test]
fn ci_run_views_read_every_page_of_jobs_and_tell_apart_jobs_of_one_name() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let job = |id: u64, name: &str, conclusion: &str| json!({"id": id, "name": name, "status": "completed", "conclusion": conclusion});
    // Eleven pages: a successful "test" job first, and a failed one last.
    let mut responses = json!({
        "GET repos/team/project/actions/runs/12": {"body": {"id": 12, "head_branch": "topic",
            "status": "completed", "conclusion": "failure",
            "html_url": "https://github.com/team/project/actions/runs/12"}},
        "GET repos/team/project/actions/jobs/1001/logs": {"body": "boom\n"},
    });
    for page in 1..=11_u64 {
        let jobs = (page * 100 - 99..=(page * 100).min(1001))
            .map(|id| match id {
                1 => job(id, "test", "success"),
                1001 => job(id, "test", "failure"),
                _ => job(id, &format!("shard {id}"), "success"),
            })
            .collect::<Vec<_>>();
        responses
            [format!("GET repos/team/project/actions/runs/12/jobs?per_page=100&page={page}")] =
            json!({"body": {"total_count": 1001, "jobs": jobs}});
    }
    fixture.respond(responses);
    let view = fixture.ok(&["view", "ci", "12", "--workspace", "topic"]);
    assert_eq!(view["jobs"].as_array().unwrap().len(), 1001);
    // The failed job on the last page is the one whose log is read, though a
    // successful job shares its name.
    let logs = view["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1, "{view}");
    assert_eq!(
        (&logs[0]["name"], &logs[0]["text"]),
        (&json!("test"), &json!("boom\n"))
    );

    let output = fixture.run(&["view", "ci", "12", "--job", "test", "--workspace", "topic"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("select one by its ID"),
        "{output:?}"
    );
    let log = fixture.ok(&[
        "view",
        "ci",
        "12",
        "--job-id",
        "1001",
        "--workspace",
        "topic",
    ]);
    assert_eq!(log["text"], "boom\n", "{log}");

    // A run's job log and a PR's check log are selected apart.
    for (args, error) in [
        (
            &["view", "ci", "12", "--check", "test"][..],
            "use --job for a run's",
        ),
        (&["view", "8", "--job", "test"], "view ci RUN --job NAME"),
        (
            &["ci", "rerun", "12", "--check", "test"],
            "cannot be used with",
        ),
        (&["ci", "rerun", "--failed"], "<RUN>"),
    ] {
        let output = fixture.run(args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{output:?}"
        );
    }
}

/// Run a Shoal command inside the workspace, as its agent would.
fn scoped(fixture: &Fixture, args: &[&str]) -> Value {
    let shoal = env!("CARGO_BIN_EXE_shoal");
    let output = fixture.run(
        &[
            &["run", "--workspace", "topic", "--", shoal, "--json"][..],
            args,
        ]
        .concat(),
    );
    assert!(output.status.success(), "{args:?}: {output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The keys of requests that change something on the forge.
fn writes(fixture: &Fixture) -> Vec<Value> {
    fixture
        .requests()
        .into_iter()
        .filter(|request| !request[0].as_str().unwrap().starts_with("GET"))
        .map(|request| json!([request[0], request[1]]))
        .collect()
}

/// The workspace's new agent messages.
fn messages(fixture: &Fixture) -> Vec<String> {
    fixture
        .ok(&["messages", "topic"])
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["message"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn proposed_issues_open_on_approval_with_the_users_edits() {
    let fixture = Fixture::new(Forge::GitHub, "[permissions]\nissue_open = 'propose'\n");
    let created = json!({"number": 40, "title": "Flaky test", "state": "open", "labels": []});
    fixture.respond(json!({
        "POST repos/team/project/issues": {"status": 201, "body": {"number": 40}},
        "GET repos/team/project/issues/40": {"body": created},
    }));
    let open = [
        "issue",
        "open",
        "--title",
        "Flaky",
        "--body",
        "Seen twice",
        "--label",
        "bug",
    ];
    let proposed = scoped(&fixture, &open)["proposed"].clone();
    assert_eq!(proposed["target"], "issue_open");
    assert_eq!(proposed["status"], "pending");
    assert_eq!(proposed["specification"]["issue"]["title"], "Flaky");
    assert_eq!(proposed["specification"]["branch"], "topic");
    // Repeating the command returns the same proposal; asking to link it is
    // another proposal.
    assert_eq!(scoped(&fixture, &open)["proposed"]["id"], proposed["id"]);
    let linked = scoped(&fixture, &[&open[..], &["--link"]].concat())["proposed"].clone();
    assert_ne!(linked["id"], proposed["id"]);
    assert_eq!(linked["specification"]["link"], true);
    fixture.ok(&["approvals", "--deny", &linked["id"].to_string()]);
    messages(&fixture);
    assert!(writes(&fixture).is_empty());
    let listed = fixture.ok(&["approvals"]);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["workspace"], "topic");

    // A worktree replaced while the proposal waits is refused before
    // anything is sent, and the proposal stays.
    let workspace = fixture.ok(&["status", "topic"])["workspace"].clone();
    let marker = Path::new(workspace["git_dir"].as_str().unwrap()).join("shoal-workspace");
    let owner = fs::read_to_string(&marker).unwrap();
    fs::write(&marker, "another-workspace\n").unwrap();
    let id = proposed["id"].to_string();
    let refused = fixture.run(&["approvals", "--approve", &id]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("another Shoal workspace"),
        "{refused:?}"
    );
    assert!(writes(&fixture).is_empty());
    fs::write(&marker, owner).unwrap();

    let approved = fixture.ok(&["approvals", "--approve", &id, "--title", "Flaky test"]);
    let url = format!("{}/issues/40", fixture.web);
    assert_eq!(approved["status"], "approved");
    assert_eq!(approved["url"], url);
    assert_eq!(
        writes(&fixture),
        [json!(["POST repos/team/project/issues",
            {"title": "Flaky test", "body": "Seen twice", "labels": ["bug"]}])]
    );
    assert_eq!(fixture.ok(&["approvals"]), json!([]));
    assert_eq!(
        fixture.ok(&["status", "topic"])["workspace"]["links"]["opened"],
        json!([{"kind": "issue", "url": url, "title": "Flaky test"}])
    );
    assert_eq!(
        messages(&fixture),
        [format!(
            "Approval {id} to open issue \"Flaky test\" was approved: {url} was opened."
        )]
    );

    // The user opens directly whatever the setting says.
    let direct = fixture.ok(&[
        "issue",
        "open",
        "--workspace",
        "topic",
        "--title",
        "Flaky test",
    ]);
    assert_eq!(direct["created"], true);
}

#[test]
fn denied_and_duplicate_issue_proposals_reach_the_agent_and_outlive_the_workspace() {
    let fixture = Fixture::new(Forge::GitHub, "[permissions]\nissue_open = 'propose'\n");
    let issue = |number: u64, title: &str| json!({"body": {"number": number, "title": title, "state": "open", "labels": []}});
    let mut responses = json!({
        "POST repos/team/project/issues": {"status": 201, "body": {"number": 40}},
        "GET repos/team/project/issues/40": issue(40, "Flaky"),
        "GET repos/team/project/issues/12": issue(12, "Known"),
        "POST repos/team/project/issues/12/comments": {"status": 201, "body": {"id": 1}},
        // The first attempt to add the duplicate's finding fails.
        "POST repos/team/project/issues/40/comments": {"status": 500,
            "body": {"message": "unavailable"}},
    });
    fixture.respond(responses.clone());
    let propose = |title: &str, body: &str| {
        let open = ["issue", "open", "--title", title, "--body", body];
        scoped(&fixture, &open)["proposed"]["id"].to_string()
    };
    let flaky = propose("Flaky", "");
    let again = propose("Flaky again", "Also on CI");
    let known = propose("Known", "");
    let noise = propose("Noise", "");

    fixture.ok(&["approvals", "--deny", &noise, "--reason", "Not a bug"]);
    let denied = fixture.ok(&["approvals", "--deny", &known, "--duplicate-of", "12"]);
    assert_eq!(denied["url"], format!("{}/issues/12", fixture.web));
    let finding = |title: &str, body: &str| {
        format!(
            "Workspace topic proposed this as a separate issue, which was marked as a duplicate.\n\n### {title}\n{body}"
        )
    };
    assert_eq!(
        writes(&fixture),
        [json!(["POST repos/team/project/issues/12/comments",
            {"body": finding("Known", "")}])]
    );
    fixture.ok(&[
        "approvals",
        "--deny",
        &again,
        "--duplicate-of-approval",
        &flaky,
    ]);
    assert!(writes(&fixture).is_empty());
    assert_eq!(
        messages(&fixture),
        [
            format!(
                "Approval {noise} to open issue \"Noise\" was denied. Do not open it another way. Reason: Not a bug"
            ),
            format!(
                "Approval {known} to open issue \"Known\" was denied as a duplicate of {}/issues/12; its text was added there as a comment. Do not open it another way.",
                fixture.web
            ),
            format!(
                "Approval {again} to open issue \"Flaky again\" was denied as a duplicate of approval {flaky}; its text is added as a comment if that issue is opened. Do not open it another way."
            ),
        ]
    );

    // The remaining proposal belongs to the repository, so it stays when the
    // workspace goes and opens through the repository's checkout.
    fixture.ok(&["rm", "topic", "--yes"]);
    let listed = fixture.ok(&["approvals"]);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"].to_string(), flaky);
    let inbox = fixture.ok(&["inbox"]);
    assert_eq!(inbox[0]["kind"], "approval", "{inbox}");
    assert_eq!(inbox.as_array().unwrap().len(), 1);
    // Another repository's inbox leaves it out.
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "other\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let filtered = |repo: &Path| fixture.ok(&["inbox", "--repo", repo.to_str().unwrap()]);
    assert_eq!(filtered(&other), json!([]));
    assert_eq!(filtered(&fixture.repo).as_array().unwrap().len(), 1);
    // Invalid details are refused before the picker would ask which request.
    let blank = fixture.run(&["approvals", "--approve", "--title", " "]);
    assert!(
        String::from_utf8_lossy(&blank.stderr).contains("the title cannot be empty"),
        "{blank:?}"
    );
    let comment = json!(["POST repos/team/project/issues/40/comments",
        {"body": finding("Flaky again", "\nAlso on CI\n")}]);
    let failed = fixture.run(&["approvals", "--approve", &flaky]);
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("approve again to retry"),
        "{failed:?}"
    );
    assert_eq!(
        writes(&fixture),
        [
            json!(["POST repos/team/project/issues", {"title": "Flaky", "body": "", "labels": []}]),
            comment.clone(),
        ]
    );
    // The request keeps the opened issue, so approving again only finishes it.
    let url = format!("{}/issues/40", fixture.web);
    assert_eq!(fixture.ok(&["approvals"])[0]["url"], url);
    responses["POST repos/team/project/issues/40/comments"] =
        json!({"status": 201, "body": {"id": 2}});
    fixture.respond(responses);
    let approved = fixture.ok(&["approvals", "--approve", &flaky]);
    assert_eq!(approved["url"], url);
    assert_eq!(writes(&fixture), [comment]);
    assert_eq!(fixture.ok(&["approvals"]), json!([]));
}

#[test]
fn proposed_prs_wait_for_approval_at_their_commit() {
    let fixture = Fixture::new(Forge::GitHub, "[permissions]\npr_open = 'propose'\n");
    let root = fixture.root.path();
    let remote = root.join("remote.git");
    git(root, &["init", "--bare", "-q", remote.to_str().unwrap()]);
    git(
        &fixture.repo,
        &["config", "remote.origin.pushurl", remote.to_str().unwrap()],
    );
    fixture.respond(json!({
        "GET repos/team/project/pulls?state=open&head=team%3Atopic": {"body": []},
        "POST repos/team/project/pulls": {"status": 201, "body": {"number": 9}},
        "GET repos/team/project/pulls/9": {"body": pull(9, "open", "Fix", &[])},
    }));
    let workspace = fixture.ok(&["status", "topic"])["workspace"].clone();
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    let head = || git(&path, &["rev-parse", "HEAD"]).trim().to_owned();

    // The branch is pushed and the PR waits for approval at that commit.
    let first = scoped(&fixture, &["pr", "open"])["proposed"].clone();
    assert_eq!(first["target"], "pr_open");
    assert_eq!(first["specification"]["commit"], head());
    assert_eq!(first["specification"]["pull"]["head"], "topic");
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/topic"]).trim(),
        head()
    );
    assert!(writes(&fixture).is_empty());
    assert_eq!(
        fixture.ok(&["status", "topic"])["cleanup_reason"]["reason"],
        "pr_proposal"
    );

    // A newer commit outdates the request; the agent is told to propose again.
    git(
        &path,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "more",
        ],
    );
    let first_id = first["id"].to_string();
    let stale = fixture.run(&["approvals", "--approve", &first_id]);
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("is outdated"),
        "{stale:?}"
    );
    assert!(writes(&fixture).is_empty());
    assert!(messages(&fixture)[0].contains("Run shoal pr open again"));
    let second = scoped(&fixture, &["pr", "open"])["proposed"].clone();
    assert_ne!(second["id"], first["id"]);
    assert_eq!(second["specification"]["commit"], head());

    // A commit pushed past Shoal outdates the request too.
    let identity = [
        "-c",
        "user.name=Shoal Test",
        "-c",
        "user.email=shoal@example.invalid",
    ];
    let tree = [
        "commit-tree",
        "-p",
        "HEAD",
        "-m",
        "elsewhere",
        "HEAD^{tree}",
    ];
    let pushed = git(&path, &[&identity[..], &tree].concat());
    let target = format!("{}:refs/heads/topic", pushed.trim());
    git(&path, &["push", "-q", remote.to_str().unwrap(), &target]);
    let second_id = second["id"].to_string();
    let stale = fixture.run(&["approvals", "--approve", &second_id]);
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("is outdated"),
        "{stale:?}"
    );
    assert!(writes(&fixture).is_empty());
    assert!(messages(&fixture)[0].contains("Run shoal pr open again"));
    git(&path, &["reset", "-q", "--hard", pushed.trim()]);
    let second = scoped(&fixture, &["pr", "open"])["proposed"].clone();
    assert_eq!(second["specification"]["commit"], head());

    let approved = fixture.ok(&[
        "approvals",
        "--approve",
        &second["id"].to_string(),
        "--title",
        "Fix",
    ]);
    let url = format!("{}/pull/9", fixture.web);
    assert_eq!(approved["url"], url);
    assert_eq!(
        writes(&fixture),
        [json!(["POST repos/team/project/pulls",
            {"title": "Fix", "body": "", "head": "topic", "base": "main", "draft": false}])]
    );
    assert_eq!(
        fixture.ok(&["status", "topic"])["workspace"]["links"]["prs"],
        json!([url])
    );
    assert!(messages(&fixture)[0].ends_with(&format!("{url} was opened and linked.")));
    // Outdated requests stay until the workspace goes, so their numbers are
    // not reused for newer ones.
    let listed = fixture.ok(&["approvals"]);
    let statuses: Vec<_> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["status"])
        .collect();
    assert_eq!(statuses, ["outdated", "outdated"]);
}

#[test]
fn a_proposed_fork_pr_opens_only_at_the_commit_the_fork_has() {
    let fixture = Fixture::new(Forge::GitHub, "[permissions]\npr_open = 'propose'\n");
    let root = fixture.root.path();
    let config = |args: &[&str]| git(&fixture.repo, args);
    for owner in ["alice", "bob"] {
        let bare = root.join(format!("{owner}.git"));
        git(root, &["init", "--bare", "-q", bare.to_str().unwrap()]);
        let url = format!("https://github.com/{owner}/project");
        config(&["remote", "add", owner, &url]);
        config(&[
            "config",
            &format!("remote.{owner}.pushurl"),
            bare.to_str().unwrap(),
        ]);
    }
    config(&["config", "remote.pushDefault", "alice"]);
    let workspace = fixture.ok(&["status", "topic"])["workspace"].clone();
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    let head = git(&path, &["rev-parse", "HEAD"]).trim().to_owned();
    let mut opened = pull(9, "open", "Fix", &[]);
    opened["head"]["repo"]["full_name"] = json!("alice/project");
    let respond = |fork_commit: &str| {
        fixture.respond(json!({
            "GET repos/team/project/pulls?state=open&head=alice%3Atopic": {"body": []},
            "GET repos/team/project/pulls?state=open&head=bob%3Atopic": {"body": []},
            "GET repos/alice/project/branches/topic": {"body": {"commit": {"sha": fork_commit}}},
            "POST repos/team/project/pulls": {"status": 201, "body": {"number": 9}},
            "GET repos/team/project/pulls/9": {"body": opened},
        }))
    };
    let outdated = |id: &Value| {
        let stale = fixture.run(&["approvals", "--approve", &id.to_string()]);
        assert!(
            String::from_utf8_lossy(&stale.stderr).contains("is outdated"),
            "{stale:?}"
        );
        assert!(writes(&fixture).is_empty());
    };
    respond(&head);
    let proposed = scoped(&fixture, &["pr", "open"])["proposed"].clone();
    assert_eq!(
        proposed["specification"]["pull"]["head_repository"],
        "alice/project"
    );

    // The approved commit in another fork does not stand in for the proposed one.
    let bob = root.join("bob.git");
    git(
        &path,
        &["push", "-q", bob.to_str().unwrap(), "HEAD:refs/heads/topic"],
    );
    config(&["config", "remote.pushDefault", "bob"]);
    // Proposing again from the other fork replaces the pending proposal.
    let reproposed = scoped(&fixture, &["pr", "open"])["proposed"].clone();
    assert_ne!(reproposed["id"], proposed["id"]);
    assert_eq!(
        reproposed["specification"]["pull"]["head_repository"],
        "bob/project"
    );
    outdated(&proposed["id"]);
    config(&["config", "remote.pushDefault", "alice"]);

    // The fork's branch on the forge decides, whatever the push URL reached.
    respond(&"0".repeat(40));
    let proposed = scoped(&fixture, &["pr", "open"])["proposed"].clone();
    outdated(&proposed["id"]);

    respond(&head);
    let proposed = scoped(&fixture, &["pr", "open"])["proposed"].clone();
    fixture.ok(&[
        "approvals",
        "--approve",
        &proposed["id"].to_string(),
        "--title",
        "Fix",
    ]);
    let created = writes(&fixture);
    assert_eq!(created[0][1]["head"], "alice:topic", "{created:?}");
}

#[test]
fn proposed_issues_keep_their_attachments_until_decided() {
    let fixture = Fixture::new(Forge::GitHub, "[permissions]\nissue_open = 'propose'\n");
    let [note, image, _] = attachment_files(&fixture);
    let uploads = "POST https://uploads.github.com/user-attachments/assets";
    fixture.respond(json!({
        "GET repos/team/project": {"body": {"id": 42}},
        format!("{uploads}?name=login.png&content_type=image%2Fpng&repository_id=42"):
            {"body": {"url": "https://github.com/user-attachments/assets/1"}},
        "POST repos/team/project/issues": {"status": 201, "body": {"number": 40}},
        "GET repos/team/project/issues/40": {"body": {"number": 40, "title": "Login",
            "state": "open", "labels": []}},
    }));
    let open = [
        "issue",
        "open",
        "--title",
        "Login",
        "--body-file",
        &note,
        "--attach",
        &image,
    ];
    let proposed = scoped(&fixture, &open)["proposed"].clone();
    // The copies are named by the proposal, so proposing again finds it.
    assert_eq!(scoped(&fixture, &open)["proposed"]["id"], proposed["id"]);
    let kept = proposed["specification"]["issue"]["attachments"][0]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(kept.contains("/state/proposals/"), "{kept}");
    assert!(
        proposed["specification"]["issue"]["body"]
            .as_str()
            .unwrap()
            .contains(&kept)
    );

    // Changing a file proposes anew and leaves the earlier copy alone.
    fs::write(&image, "png2").unwrap();
    let changed = scoped(&fixture, &open)["proposed"].clone();
    assert_ne!(changed["id"], proposed["id"]);
    assert_eq!(fs::read_to_string(&kept).unwrap(), "png");
    fixture.ok(&["approvals", "--deny", &changed["id"].to_string()]);

    // The agent's files are gone by the time the user approves.
    fs::remove_dir_all(fixture.root.path().join("shots")).unwrap();
    fixture.ok(&["approvals", "--approve", &proposed["id"].to_string()]);
    let sent: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| request[0].as_str().unwrap().starts_with("POST"))
        .map(|request| request[1].clone())
        .collect();
    assert_eq!(
        sent,
        [
            json!({"file": "login.png", "type": "image/png", "bytes": 3}),
            json!({"title": "Login", "labels": [],
                "body": "Before:\n\n![The error](https://github.com/user-attachments/assets/1)\n"}),
        ]
    );
    let left = fs::read_dir(fixture.root.path().join("state/proposals")).unwrap();
    assert_eq!(left.count(), 0);
}

#[test]
fn an_issue_approval_that_fails_after_opening_finishes_on_retry() {
    let fixture = Fixture::new(Forge::Forgejo, "[permissions]\nissue_open = 'propose'\n");
    let mut responses = json!({
        "POST repos/team/project/issues": {"status": 201, "body": {"number": 40}},
        // Forgejo labels the issue after opening it; the first attempt fails.
        "PUT repos/team/project/issues/40/labels": {"status": 500,
            "body": {"message": "unavailable"}},
        "GET repos/team/project/issues/40": {"body": {"number": 40, "title": "Flaky",
            "state": "open", "labels": [{"name": "bug"}]}},
    });
    fixture.respond(responses.clone());
    let open = ["issue", "open", "--title", "Flaky", "--label", "bug"];
    let id = scoped(&fixture, &open)["proposed"]["id"].to_string();
    let failed = fixture.run(&["approvals", "--approve", &id]);
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("approve again to retry"),
        "{failed:?}"
    );
    let url = format!("{}/issues/40", fixture.web);
    assert_eq!(fixture.ok(&["approvals"])[0]["url"], url);
    let posted: Vec<_> = writes(&fixture).iter().map(|w| w[0].clone()).collect();
    assert_eq!(
        posted,
        [
            "POST repos/team/project/issues",
            "PUT repos/team/project/issues/40/labels"
        ]
    );

    responses["PUT repos/team/project/issues/40/labels"] = json!({"body": []});
    fixture.respond(responses);
    assert_eq!(fixture.ok(&["approvals", "--approve", &id])["url"], url);
    assert_eq!(
        writes(&fixture),
        [json!(["PUT repos/team/project/issues/40/labels", {"labels": ["bug"]}])]
    );
}

#[test]
fn a_pr_approval_that_fails_after_opening_finishes_on_retry() {
    let fixture = Fixture::new(Forge::GitHub, "[permissions]\npr_open = 'propose'\n");
    let root = fixture.root.path();
    let remote = root.join("remote.git");
    git(root, &["init", "--bare", "-q", remote.to_str().unwrap()]);
    git(
        &fixture.repo,
        &["config", "remote.origin.pushurl", remote.to_str().unwrap()],
    );
    let mut responses = json!({
        "GET repos/team/project/pulls?state=open&head=team%3Atopic": {"body": []},
        "POST repos/team/project/pulls": {"status": 201, "body": {"number": 9}},
        "GET repos/team/project/pulls/9": {"body": pull(9, "open", "initial", &["bug"])},
        // Labeling the new PR fails the first time.
        "PUT repos/team/project/issues/9/labels": {"status": 500,
            "body": {"message": "unavailable"}},
    });
    fixture.respond(responses.clone());
    let id = scoped(&fixture, &["pr", "open", "--label", "bug"])["proposed"]["id"].to_string();
    let failed = fixture.run(&["approvals", "--approve", &id]);
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("approve again to retry"),
        "{failed:?}"
    );
    let url = format!("{}/pull/9", fixture.web);
    assert_eq!(fixture.ok(&["approvals"])[0]["url"], url);
    let posted: Vec<_> = writes(&fixture).iter().map(|w| w[0].clone()).collect();
    assert_eq!(
        posted,
        [
            "POST repos/team/project/pulls",
            "PUT repos/team/project/issues/9/labels"
        ]
    );

    responses["PUT repos/team/project/issues/9/labels"] = json!({"body": []});
    fixture.respond(responses);

    // A workspace renamed meanwhile does not get the old branch's PR linked.
    let shoal = env!("CARGO_BIN_EXE_shoal");
    let rename = |from: &str, to: &str| {
        let renamed = fixture.run(&[
            "run",
            "--workspace",
            from,
            "--",
            shoal,
            "--json",
            "branch",
            "rename",
            to,
        ]);
        assert!(renamed.status.success(), "{renamed:?}");
    };
    rename("topic", "renamed");
    let refused = fixture.run(&["approvals", "--approve", &id]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("does not have the workspace branch"),
        "{refused:?}"
    );
    rename("renamed", "topic");
    assert_eq!(fixture.ok(&["approvals", "--approve", &id])["url"], url);
    assert_eq!(
        writes(&fixture),
        [json!(["PUT repos/team/project/issues/9/labels", {"labels": ["bug"]}])]
    );
    assert_eq!(
        fixture.ok(&["status", "topic"])["workspace"]["links"]["prs"],
        json!([url])
    );
}

#[test]
fn a_finding_that_reached_the_forge_is_not_posted_again() {
    let fixture = Fixture::new(Forge::GitHub, "[permissions]\nissue_open = 'propose'\n");
    let issue = json!({"body": {"number": 40, "title": "Flaky", "state": "open", "labels": []}});
    fixture.respond(json!({
        "POST repos/team/project/issues": {"status": 201, "body": {"number": 40}},
        // Approval reads the issue it opened and the comment's item; reading
        // it after the comment is posted fails.
        "GET repos/team/project/issues/40": [issue, issue,
            {"status": 500, "body": {"message": "unavailable"}}],
        "POST repos/team/project/issues/40/comments": {"status": 201, "body": {"node_id": "C_1"}},
    }));
    let propose = |title: &str| {
        let open = ["issue", "open", "--title", title];
        scoped(&fixture, &open)["proposed"]["id"].to_string()
    };
    let flaky = propose("Flaky");
    let again = propose("Flaky again");
    fixture.ok(&[
        "approvals",
        "--deny",
        &again,
        "--duplicate-of-approval",
        &flaky,
    ]);
    let approved = fixture.ok(&["approvals", "--approve", &flaky]);
    // The user hears what the comment's posting could not finish.
    assert!(
        approved["warnings"][0]
            .as_str()
            .unwrap()
            .contains("unavailable"),
        "{approved}"
    );
    let posted: Vec<_> = writes(&fixture).iter().map(|w| w[0].clone()).collect();
    assert_eq!(
        posted,
        [
            "POST repos/team/project/issues",
            "POST repos/team/project/issues/40/comments"
        ]
    );
    assert_eq!(fixture.ok(&["approvals"]), json!([]));
}

#[test]
fn issues_open_from_a_registered_checkout_under_its_settings() {
    let fixture = Fixture::new(
        Forge::GitHub,
        "[permissions]\nissue_open = 'propose'\n[issue]\nrequired_labels = ['type/']\n",
    );
    fixture.respond(json!({
        "POST repos/team/project/issues": {"status": 201, "body": {"number": 40}},
        "GET repos/team/project/issues/40": {"body": {"number": 40, "title": "Flaky",
            "state": "open", "labels": [{"name": "type/bug"}]}},
    }));
    let open_in = |directory: &Path, args: &[&str]| {
        let open = ["issue", "open", "--title", "Flaky"];
        let output = fixture
            .command()
            .current_dir(directory)
            .args([&open[..], args].concat())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        (output, stderr)
    };

    // Outside a workspace and a checkout, nothing names the repository.
    let (_, stderr) = open_in(fixture.root.path(), &["--label", "type/bug"]);
    assert!(
        stderr.contains("no current workspace or registered checkout"),
        "{stderr}"
    );
    // The repository's required labels apply as in a workspace.
    let (_, stderr) = open_in(&fixture.repo, &["--label", "bug"]);
    assert!(stderr.contains("type/"), "{stderr}");
    // Opening from a checkout assigns the issue to no workspace.
    let (_, stderr) = open_in(&fixture.repo, &["--label", "type/bug", "--link"]);
    assert!(stderr.contains("--link needs a workspace"), "{stderr}");
    // A workspace process acts from its own workspace.
    let repository = fixture.ok(&["status", "topic"])["workspace"]["repository_id"].clone();
    let method = json!({"open_issue": {"checkout": repository, "repository": null,
        "link": false, "issue": {"title": "Flaky", "body": "", "labels": ["type/bug"]}}});
    let scope = fixture.ok(&["work", "env", "topic"])["SHOAL_SCOPE_TOKEN"].clone();
    let scoped = fixture.request_as(method, scope.as_str());
    assert!(
        scoped
            .to_string()
            .contains("not from a registered checkout"),
        "{scoped}"
    );
    assert!(writes(&fixture).is_empty());

    // The user's chat opens directly, whatever `issue_open` says.
    let (output, stderr) = open_in(&fixture.repo, &["--label", "type/bug", "--json"]);
    assert!(output.status.success(), "{stderr}");
    let opened: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(opened["linked"], false);
    assert_eq!(opened["url"], format!("{}/issues/40", fixture.web));
    assert_eq!(
        writes(&fixture),
        [json!(["POST repos/team/project/issues",
            {"title": "Flaky", "body": "", "labels": ["type/bug"]}])]
    );
    assert_eq!(
        fixture.ok(&["status", "topic"])["workspace"]["links"]["opened"],
        json!([])
    );
}

#[test]
fn item_commands_act_from_a_registered_checkout() {
    let fixture = Fixture::new(Forge::GitHub, "");
    fixture.respond(json!({
        "GET repos/team/project/issues/40": {"body": {"number": 40, "title": "Flaky",
            "state": "open", "labels": []}},
        "POST repos/team/project/issues/40/comments": {"status": 201, "body": {"id": 1}},
        "issue list --repo=github.com/team/project --state=open --limit=30 --json=number,title,state,labels,assignees,milestone --search=flaky":
            {"body": [{"number": 40, "title": "Flaky", "state": "OPEN", "labels": []}]},
        "GET repos/team/project/pulls/8": {"body": pull(8, "open", "Fix", &[])},
        "GET repos/team/project": {"body": {"owner": {"login": "team", "type": "User"}}},
        "GET repos/team/project/assignees?per_page=100&page=1": {"body": [{"login": "kim"}]},
        "POST graphql": {"body": {"data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": false, "endCursor": null},
            "nodes": [{"id": "T_1", "isResolved": false,
                "comments": {"nodes": [{"databaseId": 5}, {"databaseId": 6}]}}]}}}}}},
    }));
    let in_checkout = |args: &[&str]| {
        fixture
            .command()
            .current_dir(&fixture.repo)
            .args(args)
            .output()
            .unwrap()
    };

    let commented = in_checkout(&["--json", "issue", "comment", "40", "--body", "Seen"]);
    assert!(commented.status.success(), "{commented:?}");
    assert_eq!(
        writes(&fixture),
        [json!(["POST repos/team/project/issues/40/comments", {"body": "Seen"}])]
    );
    let listed = in_checkout(&["--json", "issue", "list", "flaky"]);
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed[0]["url"], format!("{}/issues/40", fixture.web));
    let viewed = in_checkout(&["--json", "view", "issue", "40", "--no-comments"]);
    let viewed: Value = serde_json::from_slice(&viewed.stdout).unwrap();
    assert_eq!(viewed[0]["url"], format!("{}/issues/40", fixture.web));
    // Commands that read the PR before acting read it from the checkout too.
    let reviewers = in_checkout(&["--json", "pr", "reviewers", "8"]);
    let reviewers: Value = serde_json::from_slice(&reviewers.stdout).unwrap();
    assert_eq!(reviewers[0]["name"], "kim", "{reviewers}");
    let resolved = in_checkout(&["--json", "pr", "resolve", "6", "8"]);
    assert!(resolved.status.success(), "{resolved:?}");
    // Another repository's reads work there as in a workspace.
    fixture.respond(json!({
        "GET repos/team/tool/labels?per_page=100&page=1": {"body": [{"name": "bug"}]},
        "GET repos/team/tool": {"body": {"name": "tool"}},
    }));
    let labels = in_checkout(&["--json", "repo", "labels", "--repo", "team/tool"]);
    let labels: Value = serde_json::from_slice(&labels.stdout).unwrap();
    assert_eq!(labels[0]["name"], "bug", "{labels}");
    let api = [
        "--json",
        "forge",
        "api",
        "repos/{owner}/{repo}",
        "--repo",
        "team/tool",
    ];
    let read: Value = serde_json::from_slice(&in_checkout(&api).stdout).unwrap();
    assert_eq!(read, json!({"name": "tool"}));
    // A checkout links nothing, so the item must be named.
    for args in [&["issue", "close"][..], &["view"]] {
        let refused = in_checkout(args);
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("nothing is linked"),
            "{args:?}: {refused:?}"
        );
    }
}

#[test]
fn item_commands_act_in_a_named_repository_without_a_workspace() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let mut open = pull(8, "open", "Fix", &[]);
    open["head"]["sha"] = json!("head");
    fixture.respond(json!({
        "GET repos/team/project/issues/40": {"body": {"number": 40, "title": "Flaky",
            "state": "open", "labels": []}},
        "POST repos/team/project/issues/40/comments": {"status": 201, "body": {"id": 1}},
        "issue list --repo=github.com/team/project --state=open --limit=30 --json=number,title,state,labels,assignees,milestone --search=flaky":
            {"body": [{"number": 40, "title": "Flaky", "state": "OPEN", "labels": []}]},
        "issue view 40": {"body": {"number": 40, "state": "OPEN", "title": "Flaky",
            "body": "", "comments": []}},
        "GET repos/team/project/labels?per_page=100&page=1": {"body": [{"name": "bug"}]},
        "GET repos/team/project/pulls/8": {"body": open},
        "GET repos/team/project/commits/head/check-runs?filter=latest&per_page=100": {"body": {
            "total_count": 1, "check_runs": [{"name": "rust", "status": "completed",
                "conclusion": "failure",
                "details_url": "https://github.com/team/project/actions/runs/1/job/11"}]}},
        "GET repos/team/project/commits/head/status?per_page=100":
            {"body": {"total_count": 0, "statuses": []}},
        "POST repos/team/project/actions/jobs/11/rerun": {"status": 201},
    }));
    // Outside every workspace and checkout, as a client such as Quay runs.
    let outside = |args: &[&str]| {
        fixture
            .command()
            .current_dir(fixture.root.path())
            .args(args)
            .output()
            .unwrap()
    };
    let read = |args: &[&str]| {
        let output = outside(&[&["--json"][..], args].concat());
        assert!(output.status.success(), "{args:?}: {output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };

    read(&["issue", "comment", "40", "--body", "Seen", "--repo", "repo"]);
    assert_eq!(
        writes(&fixture),
        [json!(["POST repos/team/project/issues/40/comments", {"body": "Seen"}])]
    );
    let listed = read(&["issue", "list", "flaky", "--repo", "repo"]);
    assert_eq!(listed[0]["url"], format!("{}/issues/40", fixture.web));
    let labels = read(&["repo", "labels", "--repo", "repo"]);
    assert_eq!(labels[0]["name"], "bug", "{labels}");
    // An item URL of a registered repository names it as well.
    let url = format!("{}/issues/40", fixture.web);
    let viewed = read(&["view", &url, "--no-comments"]);
    assert_eq!(viewed[0]["title"], "Flaky", "{viewed}");
    fixture.requests();
    // So do CI actions on a PR's checks, which change no workspace.
    let rerun = read(&[
        "ci", "rerun", "--pr", "8", "--check", "rust", "--repo", "repo",
    ]);
    assert_eq!(rerun["checks"], json!(["rust"]), "{rerun}");
    assert_eq!(
        writes(&fixture),
        [json!([
            "POST repos/team/project/actions/jobs/11/rerun",
            null
        ])]
    );
    assert!(
        !fixture
            .events()
            .iter()
            .any(|event| event["kind"] == "item_changed"),
        "{:?}",
        fixture.events()
    );

    let refused = |args: &[&str], message: &str| {
        let output = outside(args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{args:?}: {stderr}");
    };
    // Nothing is linked without a workspace, so the PR must be named.
    refused(&["ci", "rerun", "--repo", "repo"], "nothing is linked");
    refused(
        &["issue", "close", "40", "--repo", "elsewhere"],
        "repository is not registered",
    );
    refused(
        &[
            "issue",
            "close",
            "40",
            "--repo",
            "repo",
            "--workspace",
            "topic",
        ],
        "cannot be used with",
    );
    // A workspace process acts only from its own workspace.
    let shoal = env!("CARGO_BIN_EXE_shoal");
    let close = ["issue", "close", "40", "--repo", "repo"];
    refused(
        &[&["run", "--workspace", "topic", "--", shoal][..], &close].concat(),
        "not from a registered checkout",
    );
    // Where --repo names any repository, it stays a forge repository for one.
    let list = ["issue", "list", "--repo", "repo"];
    refused(
        &[&["run", "--workspace", "topic", "--", shoal][..], &list].concat(),
        "repo names no repository",
    );
    assert!(writes(&fixture).is_empty());

    // A path names the repository from the current directory, and a name
    // that also names another registration is ambiguous.
    read(&[
        "issue", "comment", "40", "--body", "Seen", "--repo", "./repo",
    ]);
    assert_eq!(writes(&fixture).len(), 1);
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "other\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap(), "--name", "repo"]);
    refused(&["issue", "close", "40", "--repo", "repo"], "ambiguous");
}

#[test]
fn checkout_merges_go_out_without_the_agent_account() {
    // A missing agent wrapper leaves the user's own logins usable.
    let fixture = Fixture::new(Forge::Forgejo, "[agent.forge_clis]\nfj = '~/missing-fj'\n");
    let endpoint = "repos/team/project/pulls/8";
    fixture.respond(json!({
        format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
        format!("POST {endpoint}/merge"): {"body": {}},
    }));
    let repository = fixture.ok(&["status", "topic"])["workspace"]["repository_id"].clone();
    let merged = fixture.request(json!({"item_action": {"checkout": repository, "kind": "pr",
        "item": "8", "action": {"action": "merge", "method": "rebase", "delete_branch": false}}}));
    assert_eq!(merged["type"], "item", "{merged}");
    assert_eq!(writes(&fixture)[0][0], format!("POST {endpoint}/merge"));
}

#[test]
fn approving_a_proposal_warns_about_the_repository_conventions() {
    let fixture = Fixture::new(
        Forge::GitHub,
        "[permissions]\nissue_open = 'propose'\n[issue]\nrequired_labels = ['type/']\n",
    );
    // The forge reports the opened issue without the label it was given.
    fixture.respond(json!({
        "POST repos/team/project/issues": {"status": 201, "body": {"number": 40}},
        "GET repos/team/project/issues/40": {"body": {"number": 40, "title": "Flaky",
            "state": "open", "labels": []}},
    }));
    let open = ["issue", "open", "--title", "Flaky", "--label", "type/bug"];
    let id = scoped(&fixture, &open)["proposed"]["id"].to_string();
    let approved = fixture.ok(&["approvals", "--approve", &id]);
    assert!(
        approved["warnings"][0].as_str().unwrap().contains("type/"),
        "{approved}"
    );
}

#[test]
fn issues_for_other_repositories_wait_for_approval_unless_settings_say_otherwise() {
    // The workspace's required labels do not apply to another repository.
    let fixture = Fixture::new(Forge::GitHub, "[issue]\nrequired_labels = ['type/']\n");
    fixture.respond(json!({
        "POST repos/team/tool/issues": {"status": 201, "body": {"number": 5}},
        "GET repos/team/tool/issues/5": {"body": {"number": 5, "title": "Wrong finding",
            "state": "open", "labels": [{"name": "bug"}]}},
    }));
    let open = [
        "issue",
        "open",
        "--repo",
        "team/tool",
        "--title",
        "Wrong finding",
        "--label",
        "bug",
    ];
    let proposed = scoped(&fixture, &open)["proposed"].clone();
    assert_eq!(
        proposed["specification"]["repository"],
        "https://github.com/team/tool"
    );
    // Naming it by URL is the same proposal.
    let by_url = [
        &["issue", "open", "--repo", "git@github.com:team/tool.git"][..],
        &open[4..],
    ]
    .concat();
    assert_eq!(scoped(&fixture, &by_url)["proposed"]["id"], proposed["id"]);
    assert!(writes(&fixture).is_empty());

    // Another repository's issue cannot be the workspace's assignment.
    let shoal = env!("CARGO_BIN_EXE_shoal");
    let exec = ["run", "--workspace", "topic", "--", shoal];
    let linked = fixture.run(&[&exec[..], &open, &["--link"]].concat());
    assert!(
        String::from_utf8_lossy(&linked.stderr).contains("cannot be linked"),
        "{linked:?}"
    );

    let id = proposed["id"].to_string();
    let approved = fixture.ok(&["approvals", "--approve", &id]);
    let url = "https://github.com/team/tool/issues/5";
    assert_eq!(approved["url"], url);
    assert_eq!(
        writes(&fixture),
        [json!(["POST repos/team/tool/issues",
            {"title": "Wrong finding", "body": "", "labels": ["bug"]}])]
    );
    assert_eq!(
        fixture.ok(&["status", "topic"])["workspace"]["links"]["opened"],
        json!([{"kind": "issue", "url": url, "title": "Wrong finding"}])
    );
    assert_eq!(
        messages(&fixture),
        [format!(
            "Approval {id} to open issue \"Wrong finding\" in https://github.com/team/tool was approved: {url} was opened."
        )]
    );

    // A repository can turn it off for agents; the user still opens directly.
    let repo = fixture.repo.to_str().unwrap();
    fixture.ok(&[
        "config",
        "set",
        "permissions.issue_other_repositories",
        "deny",
        "--repo",
        repo,
    ]);
    let denied = fixture.run(&[&exec[..], &open].concat());
    assert!(
        String::from_utf8_lossy(&denied.stderr).contains("other_repositories is \"deny\""),
        "{denied:?}"
    );
    assert!(writes(&fixture).is_empty());
    let direct = fixture.ok(&[&open[..], &["--workspace", "topic"]].concat());
    assert_eq!(direct["url"], url);
}

#[test]
fn agents_open_issues_directly_on_another_forge_when_allowed() {
    let fixture = Fixture::new(
        Forge::Forgejo,
        "[permissions]\nissue_other_repositories = 'direct'\n",
    );
    fs::create_dir_all(fixture.root.path().join("bin")).unwrap();
    std::os::unix::fs::symlink(
        fixture.root.path().join("forge.py"),
        fixture.root.path().join("bin/gh"),
    )
    .unwrap();
    fixture.respond(json!({
        "POST repos/them/tool/issues": {"status": 201, "body": {"number": 5}},
        "GET repos/them/tool/issues/5": {"body": {"number": 5, "title": "Crash",
            "state": "open", "labels": []}},
        "GET repos/them/tool/labels?per_page=100&page=1": {"body": [{"name": "bug"}]},
    }));
    let open = [
        "issue",
        "open",
        "--repo",
        "https://github.com/them/tool",
        "--title",
        "Crash",
    ];
    let opened = scoped(&fixture, &open);
    let url = "https://github.com/them/tool/issues/5";
    assert_eq!(opened["url"], url);
    assert_eq!(opened["created"], true);
    assert_eq!(opened["linked"], false);
    // The workspace's commands cannot label an issue there, so nothing
    // suggests it.
    assert!(opened.get("warnings").is_none(), "{opened}");
    assert_eq!(
        writes(&fixture),
        [json!(["POST repos/them/tool/issues", {"title": "Crash", "body": "", "labels": []}])]
    );
    assert_eq!(
        fixture.ok(&["status", "topic"])["workspace"]["links"]["opened"],
        json!([{"kind": "issue", "url": url, "title": "Crash"}])
    );
}

#[test]
fn api_requests_reach_any_path_and_agents_only_read() {
    let fixture = Fixture::new(Forge::Forgejo, AGENT_TOKEN);
    // Longer than one protocol frame, so it arrives in pieces.
    let branches: Vec<Value> = (0..400)
        .map(|n| json!({"name": format!("b{n}"), "commit": {"message": "\"x\"".repeat(100)}}))
        .collect();
    fixture.respond(json!({
        "GET repos/team/project/branches?limit=400&page=1": {"body": branches},
        "PATCH repos/team/project": {"body": {"description": "new"}},
    }));
    // Forgejo applies `limit` only with `page`, so a read that sets only a
    // limit asks for the first page.
    let path = "/repos/{owner}/{repo}/branches?limit=400";
    let listed = fixture.ok(&["forge", "api", path, "--workspace", "topic"]);
    assert_eq!(listed, json!(branches));
    let output = fixture.run(&["forge", "api", path, "--workspace", "topic"]);
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(printed.starts_with("[\n  {\n    \"commit\""), "{printed}");
    let body = fixture.root.path().join("body.json");
    fs::write(&body, r#"{"description": "new"}"#).unwrap();
    let body = body.to_str().unwrap();
    let edited = fixture.ok(&[
        "forge",
        "api",
        "repos/{owner}/{repo}",
        "-X",
        "patch",
        "--input",
        body,
        "--workspace",
        "topic",
    ]);
    assert_eq!(edited, json!({"description": "new"}));
    let read = "GET repos/team/project/branches?limit=400&page=1";
    assert_eq!(
        fixture.requests(),
        [
            json!([read, null, "token usertoken"]),
            json!([read, null, "token usertoken"]),
            json!(["PATCH repos/team/project", {"description": "new"}, "token usertoken"]),
        ]
    );

    // Outside a workspace, a bad path fails before a workspace is chosen.
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args(["forge", "api", "repos/..%2f..%2fuser"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("`.` or `..` segments"), "{stderr}");

    // `{owner}` and `{repo}` name the repository `--repo` names.
    fixture.respond(json!({"GET repos/team/tool": {"body": {"name": "tool"}}}));
    let other = [
        "forge",
        "api",
        "repos/{owner}/{repo}",
        "--repo",
        "team/tool",
        "--workspace",
        "topic",
    ];
    assert_eq!(fixture.ok(&other), json!({"name": "tool"}));
    assert_eq!(
        fixture.requests(),
        [json!(["GET repos/team/tool", null, "token usertoken"])]
    );

    // curl sends braces and brackets as given instead of expanding them.
    fixture.respond(json!({"GET repos/team/project/{a,b}[1-2]": {"body": {"one": true}}}));
    let globbed = fixture.ok(&[
        "forge",
        "api",
        "repos/{owner}/{repo}/{a,b}[1-2]",
        "--workspace",
        "topic",
    ]);
    assert_eq!(globbed, json!({"one": true}));
    assert_eq!(
        fixture.requests(),
        [json!([
            "GET repos/team/project/{a,b}[1-2]",
            null,
            "token usertoken"
        ])]
    );
    fixture.respond(json!({
        "GET repos/team/project/branches?limit=400&page=1": {"body": branches},
    }));

    // The agent reads as itself, and writes only when the repository lets it.
    assert_eq!(scoped(&fixture, &["forge", "api", path]), json!(branches));
    assert_eq!(
        fixture.requests(),
        [json!([read, null, "token agenttoken"])]
    );
    let shoal = env!("CARGO_BIN_EXE_shoal");
    let output = fixture.run(&[
        "run",
        "--workspace",
        "topic",
        "--",
        shoal,
        "forge",
        "api",
        "repos/{owner}/{repo}",
        "--input",
        body,
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[permissions] api_writes = \"direct\""),
        "{stderr}"
    );
    assert!(fixture.requests().is_empty());
}

#[test]
fn downloads_read_files_on_the_forge_with_its_login() {
    let fixture = Fixture::new(Forge::Forgejo, AGENT_TOKEN);
    let origin = fixture.web.strip_suffix("/team/project").unwrap();
    fixture.respond(json!({
        "GET /attachments/a1": {"body": "PNG", "headers": {"Content-Type": "image/png"}},
    }));
    let url = format!("{origin}/attachments/a1");
    let saved = fixture.root.path().join("shot.png");
    let saved_arg = saved.to_str().unwrap();
    let written = fixture.ok(&[
        "forge",
        "download",
        &url,
        "--output",
        saved_arg,
        "--workspace",
        "topic",
    ]);
    assert_eq!(
        written,
        json!({"path": saved, "bytes": 3, "content_type": "image/png"})
    );
    assert_eq!(fs::read(&saved).unwrap(), b"PNG");
    let output = fixture.run(&["forge", "download", &url, "-o", "-", "--workspace", "topic"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"PNG");
    // A workspace process downloads as its agent account, and a chat in the
    // registered checkout with the user's logins.
    scoped(&fixture, &["forge", "download", &url, "-o", saved_arg]);
    let in_checkout = fixture
        .command()
        .current_dir(&fixture.repo)
        .args(["forge", "download", &url, "-o", saved_arg])
        .output()
        .unwrap();
    assert!(in_checkout.status.success(), "{in_checkout:?}");
    assert_eq!(
        fixture.requests(),
        [
            json!(["GET /attachments/a1", null, "token usertoken"]),
            json!(["GET /attachments/a1", null, "token usertoken"]),
            json!(["GET /attachments/a1", null, "token agenttoken"]),
            json!(["GET /attachments/a1", null, "token usertoken"]),
        ]
    );

    // A malformed URL fails before a workspace is chosen.
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args(["forge", "download", "not-a-url", "-o", saved_arg])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not an HTTP or HTTPS URL"), "{stderr}");

    // The login goes to no other host.
    let output = fixture.run(&[
        "forge",
        "download",
        "https://example.com/shot.png",
        "-o",
        saved_arg,
        "--workspace",
        "topic",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not on the repository's forge"), "{stderr}");
    assert!(fixture.requests().is_empty());
}

#[test]
fn gitlab_downloads_read_an_upload_from_its_api_route() {
    // The scripted gh answers as glab on a GitLab remote too.
    let fixture = Fixture::new(Forge::GitHub, "[agent.forge_clis]\nglab = '~/agent-glab'\n");
    let root = fixture.root.path();
    std::os::unix::fs::symlink(root.join("forge.py"), root.join("bin/glab")).unwrap();
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "git@gitlab.com:team/project.git",
        ],
    );
    let wrapper = "#!/bin/sh\nAGENT=agent exec python3 \"$HOME/forge.py\" \"$@\"\n";
    support::install_test_script(root.join("agent-glab"), wrapper);
    let endpoint = "projects/team%2Fproject/uploads/0123abcd/shot.png";
    fixture.respond(json!({
        format!("GET {endpoint}"): {"body": "PNG", "headers": {"Content-Type": "image/png"}},
    }));
    let url = "https://gitlab.com/team/project/uploads/0123abcd/shot.png";
    let saved = root.join("shot.png");
    let saved_arg = saved.to_str().unwrap();
    let written = scoped(&fixture, &["forge", "download", url, "-o", saved_arg]);
    assert_eq!(
        written,
        json!({"path": saved, "bytes": 3, "content_type": "image/png"})
    );
    assert_eq!(fs::read(&saved).unwrap(), b"PNG");
    let requests: Vec<_> = fixture
        .requests()
        .into_iter()
        .map(|request| (request[0].clone(), request[2].clone()))
        .collect();
    assert_eq!(
        requests,
        [(json!(format!("GET {endpoint}")), json!("agent"))]
    );
}
