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
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    thread,
    time::{Duration, Instant},
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
        fs::write(&script, FORGE).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
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
        let deadline = Instant::now() + Duration::from_secs(60);
        while !port_file.exists() {
            assert!(Instant::now() < deadline, "forge server did not start");
            thread::sleep(Duration::from_millis(20));
        }
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
        let send = |protocol: u64, method: Value| {
            let mut stream =
                UnixStream::connect(self.root.path().join("state/daemon.sock")).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            writeln!(
                stream,
                "{}",
                json!({"protocol": protocol, "id": 1, "method": method})
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
            fs::write(root.join(name), format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o755)).unwrap();
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
            format!("PUT {endpoint}/merge"): {"body": {}},
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
    fs::write(fixture.root.path().join("agent-fj"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(
        fixture.root.path().join("agent-fj"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
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
