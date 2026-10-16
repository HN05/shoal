//! Issue and PR actions against a scripted forge: `gh` on GitHub, an HTTP
//! server on Forgejo, both from `fixtures/forge.py`.
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
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    time::Duration,
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

impl Fixture {
    fn new(forge: Forge, config: &str) -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        fs::create_dir_all(root.path().join(".config/shoal")).unwrap();
        fs::write(root.path().join(".config/shoal/config.toml"), config).unwrap();
        fs::create_dir_all(root.path().join("forge")).unwrap();
        fs::write(root.path().join("forge/responses.json"), "{}").unwrap();
        let script = root.path().join("forge.py");
        support::install_test_script(&script, FORGE);
        let repo = init_repo(root.path(), "repo", &[("tracked", "committed\n")]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let daemon = DaemonGuard::start(root.path(), &mut support::cli(root.path()));
        let mut fixture = Self {
            _daemon: daemon,
            server: None,
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
                let host = fixture.serve();
                let keys = if cfg!(target_os = "macos") {
                    "Library/Application Support/forgejo-cli.forgejo-cli/keys.json"
                } else {
                    ".local/share/forgejo-cli/keys.json"
                };
                fixture.write_keys(&fixture.root.path().join(keys), &host, "usertoken");
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

    /// Start the Forgejo server and return its `host:port`.
    fn serve(&mut self) -> String {
        let port_file = self.root.path().join("forge/port");
        let child = support::isolated(self.root.path(), "python3")
            .arg(self.root.path().join("forge.py"))
            .args(["serve", port_file.to_str().unwrap()])
            .spawn()
            .unwrap();
        self.server = Some(child);
        // A hang guard only: the server writes its port once it listens.
        support::wait::wait_until("forge server to start", || port_file.exists());
        format!("127.0.0.1:{}", fs::read_to_string(port_file).unwrap())
    }

    fn write_keys(&self, path: &Path, host: &str, token: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            json!({"hosts": {host: {"type": "Application", "token": token}}}).to_string(),
        )
        .unwrap();
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
        let send = |protocol: u64, method: Value| {
            let mut stream =
                UnixStream::connect(self.root.path().join("state/daemon.sock")).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            writeln!(
                stream,
                "{}",
                json!({"protocol": protocol, "id": 1, "method": method, "scope": scope})
            )
            .unwrap();
            let mut response = String::new();
            BufReader::new(stream).read_line(&mut response).unwrap();
            serde_json::from_str::<Value>(&response).unwrap()
        };
        let protocol = send(0, json!("status"))["protocol"].as_u64().unwrap();
        send(protocol, method)
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
                "labels": ["bug", "ready"], "pr": {"head": "topic", "base": "main", "draft": false}})
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
fn changes_use_the_agent_account_and_merges_the_users() {
    for forge in [Forge::GitHub, Forge::Forgejo] {
        let config =
            "[agent_auth]\ngh = '~/agent-gh'\nfj = '~/agent-fj'\nfj_home = '~/agent-home'\n";
        let fixture = Fixture::new(forge, config);
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
            Forge::Forgejo => {
                let host = fixture.web.split('/').nth(2).unwrap().to_owned();
                let keys = if cfg!(target_os = "macos") {
                    "agent-home/Library/Application Support/forgejo-cli.forgejo-cli/keys.json"
                } else {
                    "agent-home/.local/share/forgejo-cli/keys.json"
                };
                fixture.write_keys(&root.join(keys), &host, "agenttoken");
                (json!("token agenttoken"), json!("token usertoken"))
            }
        };
        let endpoint = "repos/team/project/pulls/8";
        fixture.respond(json!({
            format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
            "POST repos/team/project/issues/8/comments": {"body": {}},
            format!("PUT {endpoint}/merge"): {"body": {"merged": true}},
            format!("POST {endpoint}/merge"): {"body": {}},
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
        let auth: Vec<_> = fixture
            .requests()
            .into_iter()
            .filter(|request| !request[0].as_str().unwrap().starts_with("GET"))
            .map(|request| request[2].clone())
            .collect();
        assert_eq!(auth, [agent, user]);
    }

    // An fj wrapper whose home Shoal does not know cannot stand in for the agent.
    let fixture = Fixture::new(Forge::Forgejo, "[agent_auth]\nfj = '~/agent-fj'\n");
    support::install_test_script(fixture.root.path().join("agent-fj"), "#!/bin/sh\n");
    let refused = fixture.request(json!({"item_action": {"workspace": "topic", "kind": "pr",
        "item": "8", "action": {"action": "close"}}}));
    assert!(
        refused["data"]["message"]
            .as_str()
            .unwrap()
            .contains("agent_auth.fj_home"),
        "{refused}"
    );
    assert!(fixture.requests().is_empty());
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
        String::from_utf8_lossy(&refused.stderr).contains("no API for resolving"),
        "{refused:?}"
    );
    assert!(
        fixture
            .requests()
            .iter()
            .all(|request| request[0].as_str().unwrap().starts_with("GET")),
    );
}

#[test]
fn pr_and_issue_commands_act_on_linked_or_named_items() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let endpoint = "repos/team/project/pulls/8";
    let issue = json!({"number": 3, "title": "Bug", "state": "open", "labels": []});
    fixture.respond(json!({
        "pr view 8": {"body": {"number": 8, "state": "OPEN", "headRefName": "topic",
            "commits": []}},
        format!("GET {endpoint}"): {"body": pull(8, "open", "Fix", &[])},
        format!("PATCH {endpoint}"): {"body": {}},
        "PUT repos/team/project/issues/8/labels": {"body": []},
        "POST graphql": {"body": {"data": {}}},
        "GET repos/team/project/issues/3": {"body": issue},
        "POST repos/team/project/issues/3/comments": {"body": {}},
    }));
    fixture.ok(&["link", "pr", "8", "--workspace", "topic"]);
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
    let scoped = fixture.run(&[&["exec", "topic", "--", shoal][..], &merge].concat());
    assert!(
        String::from_utf8_lossy(&scoped.stderr).contains("merging is the user's decision"),
        "{scoped:?}"
    );
    assert!(fixture.requests().is_empty());
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
                let links = &fixture.ok(&["inspect", "topic"])["workspace"]["links"];
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
            fixture.ok(&["link", "issue", "34", "--workspace", "topic"]);
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
        let path = fixture.ok(&["inspect", "topic"])["workspace"]["path"].clone();
        assert_eq!(
            git(&remote, &["rev-parse", "refs/heads/topic"]),
            git(Path::new(path.as_str().unwrap()), &["rev-parse", "HEAD"])
        );
        let links = fixture.ok(&["inspect", "topic"])["workspace"]["links"]["prs"].clone();
        assert_eq!(links, json!([url]));

        // An open PR for the branch is linked instead of opening another.
        fixture.ok(&["unlink", "pr", "--workspace", "topic"]);
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
fn pr_open_refuses_unverified_or_switched_worktrees_before_pushing() {
    let fixture = Fixture::new(Forge::GitHub, "");
    let root = fixture.root.path();
    let remote = root.join("remote.git");
    git(root, &["init", "--bare", "-q", remote.to_str().unwrap()]);
    git(
        &fixture.repo,
        &["config", "remote.origin.pushurl", remote.to_str().unwrap()],
    );
    let workspace = fixture.ok(&["inspect", "topic"])["workspace"].clone();
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
fn issue_open_with_link_refuses_before_creating_when_an_issue_is_linked() {
    let fixture = Fixture::new(Forge::GitHub, "");
    fixture.respond(json!({
        "issue view 34": {"body": {"number": 34, "state": "OPEN", "title": "Assignment",
            "body": "", "comments": []}},
    }));
    fixture.ok(&["link", "issue", "34", "--workspace", "topic"]);
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
    let fixture = Fixture::new(Forge::Forgejo, "[agent_auth]\nfj_home = '~/agent'\n");
    let host = fixture
        .web
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    let keys = if cfg!(target_os = "macos") {
        "agent/Library/Application Support/forgejo-cli.forgejo-cli/keys.json"
    } else {
        "agent/.local/share/forgejo-cli/keys.json"
    };
    fixture.write_keys(&fixture.root.path().join(keys), host, "agenttoken");
    let mut responses = failed_check_responses();
    fixture.respond(responses.clone());
    let exported = fixture.ok(&["env", "topic"]);
    let scope = exported["SHOAL_SCOPE_TOKEN"].as_str().unwrap();
    let method = json!({"check_log": {"workspace": "topic", "selection": {"kind": "pr", "input": "8"}, "check": "rust"}});
    let result = fixture.request_as(method.clone(), Some(scope));
    assert_eq!(result["type"], "check_log", "{result}");
    assert_eq!(result["data"]["failed_step"], "Main Test");
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
        fixture.ok(&["link", "pr", "8", "--workspace", "topic"]);
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
                    .any(|request| request[0].as_str().unwrap().contains("/actions/"))
            );
        }
    }
}

#[test]
fn watch_reports_failed_steps_or_log_lookup_errors_without_hiding_comments() {
    for available in [true, false] {
        let fixture = Fixture::new(Forge::Forgejo, "");
        let mut responses = failed_check_responses();
        if !available {
            responses["GET repos/team/project/actions/jobs/18345/logs"] =
                json!({"status": 404, "body": {"message": "logs unavailable"}});
        }
        responses["GET repos/team/project/pulls/8"]["body"] = json!({
            "number": 8, "title": "Fix crash", "state": "open",
            "head": {"sha": "head", "ref": "topic", "repo": {"full_name": "team/project"}},
            "base": {"ref": "main"}, "mergeable": true
        });
        responses["GET repos/team/project/commits/head/status?limit=50"] = json!({"body": {
            "total_count": 1, "statuses": [{"context": "rust", "status": "failure"}]
        }});
        responses["GET repos/team/project/issues/8/comments"] =
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
        assert!(message.starts_with("rust: Failure; "), "{message}");
        assert!(message.contains("--check 'rust'"), "{message}");
        if available {
            assert!(message.contains("failed step: Main Test"), "{message}");
        } else {
            assert!(message.contains("log unavailable: HTTP 404"), "{message}");
        }
        assert_eq!(ci["check"], json!({"name": "rust", "result": "failure"}));
    }
}
