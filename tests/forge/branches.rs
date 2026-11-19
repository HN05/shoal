//! `shoal branch ls` and `shoal branch delete` against the scripted Forgejo.
use std::{collections::BTreeMap, path::PathBuf, process::Output};

use serde_json::{Value, json};

use super::{Fixture, Forge};
use crate::support::{git, git_with_identity, printed};

/// A commit no checkout has, as the forge may report one.
const UNFETCHED: &str = "f00df00df00df00df00df00df00df00df00df00d";

/// A Forgejo fixture whose checkout has a branch the default branch took by
/// a squash merge, one it took by a rebase merge and one it lacks, and whose
/// `topic` workspace has a commit of its own, each pushed to the forge's
/// repository; `config` adds to the global config.
struct Branches {
    fixture: Fixture,
    /// Each branch's commit.
    heads: BTreeMap<&'static str, String>,
}

impl Branches {
    fn new(config: &str) -> Self {
        let fixture = Fixture::new(Forge::Forgejo, config);
        let checkout = fixture.repo.clone();
        let mut heads = BTreeMap::new();
        for (branch, file) in [("squashed", "a"), ("rebased", "b"), ("unmerged", "c")] {
            git(&checkout, &["switch", "-q", "-c", branch, "main"]);
            commit(&checkout, file);
            heads.insert(branch, head(&checkout));
        }
        git(&checkout, &["switch", "-q", "main"]);
        git(&checkout, &["merge", "-q", "--squash", "squashed"]);
        git_with_identity(&checkout, &["commit", "-q", "-m", "Squash a"]);
        git_with_identity(&checkout, &["cherry-pick", "rebased"]);
        heads.insert("main", head(&checkout));
        let topic = fixture.topic();
        commit(&topic, "d");
        heads.insert("topic", head(&topic));
        let branches = Self { fixture, heads };
        git(
            branches.fixture.root.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                branches.remote().to_str().unwrap(),
            ],
        );
        branches.publish();
        branches.respond(&[], &[]);
        branches
    }

    /// The forge's repository, which takes pushes.
    fn remote(&self) -> PathBuf {
        self.fixture.root.path().join("forge/git/team/project.git")
    }

    /// Push each branch to the forge's repository at its commit.
    fn publish(&self) {
        let remote = self.remote();
        let refspecs = (self.heads.iter()).map(|(name, head)| format!("{head}:refs/heads/{name}"));
        let args = ["push", "--quiet", "--force", remote.to_str().unwrap()];
        let args: Vec<String> = args.into_iter().map(String::from).chain(refspecs).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git(&self.fixture.repo, &args);
    }

    /// The commit `branch` points to in the forge's repository, if it is
    /// there.
    fn pushed(&self, branch: &str) -> Option<String> {
        let refs = git(
            &self.remote(),
            &[
                "for-each-ref",
                "--format=%(objectname)",
                &format!("refs/heads/{branch}"),
            ],
        );
        refs.lines().next().map(str::to_owned)
    }

    /// Have the forge list the fixture's branches and `more`, each as
    /// `(name, commit, protected)`, with the open `pulls`, each as
    /// `(number, head, head repository, base)`.
    fn respond(&self, more: &[(&str, &str, bool)], pulls: &[(u64, &str, &str, &str)]) {
        self.respond_at(&self.heads, more, pulls);
    }

    /// [`Self::respond`] with the branches at `heads` instead.
    fn respond_at(
        &self,
        heads: &BTreeMap<&str, String>,
        more: &[(&str, &str, bool)],
        pulls: &[(u64, &str, &str, &str)],
    ) {
        let mut listed: Vec<_> = (heads.iter())
            .map(|(name, head)| (*name, head.as_str(), *name == "main"))
            .collect();
        listed.extend_from_slice(more);
        let branches: Vec<_> = (listed.iter())
            .map(|(name, head, protected)| {
                json!({"name": name, "protected": protected,
                    "commit": {"id": head, "timestamp": "2026-11-18T10:00:00Z"}})
            })
            .collect();
        let pulls: Vec<_> = (pulls.iter())
            .map(|(number, head, repository, base)| {
                json!({"number": number, "head": {"ref": head, "repo": {"full_name": repository}},
                    "base": {"ref": base}})
            })
            .collect();
        let mut responses = json!({
            "GET repos/team/project/branches?limit=50&page=1": {"body": branches},
            "GET repos/team/project/pulls?state=open&limit=50&page=1": {"body": pulls},
            "GET repos/team/project": {"body": {"default_branch": "main"}},
        });
        for (name, head, _) in &listed {
            let path = format!("repos/team/project/branches/{name}");
            responses[format!("GET {path}")] = json!({"body": {"commit": {"id": head}}});
            responses[format!("DELETE {path}")] = json!({"status": 204});
        }
        self.fixture.respond(responses);
    }

    /// What `branch ls` lists from the `topic` workspace with `args`, by
    /// branch name.
    fn listed(&self, args: &[&str]) -> BTreeMap<String, Value> {
        let topic = self.fixture.id("topic");
        let listed = self
            .fixture
            .ok(&[&["branch", "ls", "--space", &topic], args].concat());
        (listed.as_array().unwrap().iter())
            .map(|branch| (branch["name"].as_str().unwrap().to_owned(), branch.clone()))
            .collect()
    }

    /// `branch delete` from the `topic` workspace by the user with `args`.
    fn delete(&self, args: &[&str]) -> Output {
        let topic = self.fixture.id("topic");
        let args = [&["branch", "delete", "--space", &topic][..], args].concat();
        self.fixture.run(&args)
    }

    /// `branch delete` with `args` by a process of the space `space`, with
    /// its scope.
    fn delete_as(&self, space: &str, args: &[&str]) -> Output {
        let id = self.fixture.id(space);
        let env = self.fixture.ok(&["internal", "env", &id]);
        let mut command = self.fixture.command();
        for (name, value) in env.as_object().unwrap() {
            command.env(name, value.as_str().unwrap());
        }
        let path = self.fixture.ok(&["status", &id])["workspace"]["path"].clone();
        command.current_dir(PathBuf::from(path.as_str().unwrap()));
        let args = [&["--json", "branch", "delete"][..], args].concat();
        self.fixture.output(command.args(args))
    }

    /// The branches the forge was asked to delete since the last call.
    fn deleted(&self) -> Vec<String> {
        self.deletions()
            .into_iter()
            .map(|(branch, _)| branch)
            .collect()
    }

    /// Each branch a push asked the forge to delete since the last call,
    /// with the login it signed in with.
    fn deletions(&self) -> Vec<(String, String)> {
        let pushes = self.fixture.requests();
        let pushes = (pushes.iter()).filter(|request| request[0] == "PUSH team/project");
        let deletions = pushes.flat_map(|push| {
            let login = push[2].as_str().unwrap().to_owned();
            let updates = push[1].as_array().unwrap().clone();
            updates.into_iter().filter_map(move |update| {
                let (_, deleted) = update
                    .as_str()?
                    .split_once(" 0000000000000000000000000000000000000000 refs/heads/")?;
                Some((deleted.to_owned(), login.clone()))
            })
        });
        deletions.collect()
    }
}

fn commit(repo: &std::path::Path, file: &str) {
    std::fs::write(repo.join(file), format!("{file}\n")).unwrap();
    git(repo, &["add", file]);
    git_with_identity(repo, &["commit", "-q", "-m", file]);
}

fn head(repo: &std::path::Path) -> String {
    git(repo, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// What `listed` says decides whether a branch can go, by branch name.
fn decided(listed: &BTreeMap<String, Value>) -> BTreeMap<&str, Value> {
    (listed.iter())
        .map(|(name, branch)| {
            let decided = json!([
                branch["merged"],
                branch["default"],
                branch["protected"],
                branch["open_prs"],
                branch["space"]["state"]
            ]);
            (name.as_str(), decided)
        })
        .collect()
}

#[test]
fn branch_ls_says_what_decides_whether_each_branch_can_go() {
    let branches = Branches::new("");
    branches.respond(
        &[("gone", UNFETCHED, false)],
        &[(7, "topic", "team/project", "main")],
    );
    let listed = branches.listed(&[]);
    assert_eq!(
        decided(&listed),
        BTreeMap::from([
            ("gone", json!([null, false, false, [], null])),
            ("main", json!([true, true, true, [], null])),
            ("rebased", json!([true, false, false, [], null])),
            ("squashed", json!([true, false, false, [], null])),
            ("topic", json!([false, false, false, [7], "active"])),
            ("unmerged", json!([false, false, false, [], null])),
        ])
    );
    let topic = &listed["topic"];
    assert_eq!(
        (&topic["head"], &topic["space"]["id"]),
        (
            &json!(branches.heads["topic"]),
            &json!(branches.fixture.id("topic"))
        ),
        "{topic}"
    );
    // The user reads with their own login.
    let reads = branches.fixture.requests();
    assert!(
        reads.iter().all(|request| request[2] == "token usertoken"),
        "{reads:?}"
    );

    let names = |args: &[&str]| -> Vec<String> { branches.listed(args).into_keys().collect() };
    assert_eq!(names(&["--merged"]), ["main", "rebased", "squashed"]);
    assert_eq!(names(&["--unmerged"]), ["topic", "unmerged"]);
    let rows = branches
        .fixture
        .run(&["branch", "ls", "--space", &branches.fixture.id("topic")]);
    let text = String::from_utf8_lossy(&rows.stdout);
    let row = |name: &str| {
        text.lines()
            .find(|line| line.starts_with(name))
            .unwrap_or("")
    };
    assert!(row("topic ").contains("PR #7"), "{text}");
    assert!(row("main ").contains("default"), "{text}");
}

#[test]
fn branch_delete_deletes_none_while_one_cannot_go() {
    let branches = Branches::new("");
    branches.respond(
        &[
            ("kept", &branches.heads["main"].clone(), true),
            ("gone", UNFETCHED, false),
        ],
        &[
            (7, "topic", "team/project", "main"),
            (8, "unmerged", "someone/project", "rebased"),
        ],
    );
    // Each refused branch keeps the merged one named with it.
    for refused in [
        "main", "kept", "topic", "rebased", "unmerged", "gone", "missing",
    ] {
        let output = branches.delete(&["squashed", refused]);
        assert!(!output.status.success(), "{refused}: {}", printed(&output));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(&format!("{refused}: ")), "{stderr}");
        assert_eq!(branches.deleted(), Vec::<String>::new(), "{refused}");
    }
    // An open PR from a fork's branch of the same name keeps no branch here,
    // and --force deletes what the default branch lacks.
    branches.respond(&[], &[(9, "unmerged", "someone/project", "main")]);
    let output = branches.delete(&["squashed", "unmerged", "--force"]);
    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(branches.deleted(), ["squashed", "unmerged"]);

    // A branch that moved since it was read is not deleted.
    branches.respond(&[], &[]);
    let mut responses: Value = serde_json::from_str(
        &std::fs::read_to_string(branches.fixture.root.path().join("forge/responses.json"))
            .unwrap(),
    )
    .unwrap();
    responses["GET repos/team/project/branches/rebased"] =
        json!({"body": {"commit": {"id": branches.heads["main"]}}});
    branches.fixture.respond(responses);
    // Nor is one named before it.
    let moved = branches.delete(&["squashed", "rebased"]);
    assert!(!moved.status.success(), "{}", printed(&moved));
    assert_eq!(branches.deleted(), Vec::<String>::new());
}

#[test]
fn branch_delete_keeps_a_branch_pushed_to_after_its_check() {
    let branches = Branches::new("");
    // The forge still reports the commit Shoal reads and checks, while a
    // push has moved the branch.
    let later = &branches.heads["unmerged"];
    git(
        &branches.remote(),
        &["update-ref", "refs/heads/squashed", later],
    );
    let output = branches.delete(&["squashed"]);
    assert!(!output.status.success(), "{}", printed(&output));
    assert_eq!(branches.deleted(), Vec::<String>::new());
    assert_eq!(branches.pushed("squashed").as_ref(), Some(later));

    // The user's deletion signs in with their own login.
    let deleted = branches.delete(&["rebased"]);
    assert!(deleted.status.success(), "{}", printed(&deleted));
    assert_eq!(
        branches.deletions(),
        [("rebased".to_owned(), "token:usertoken".to_owned())]
    );
    assert_eq!(branches.pushed("rebased"), None);
}

#[test]
fn an_agent_deletes_branches_as_its_push_permission_allows() {
    let config = |push: &str| format!("[permissions]\npush = '{push}'\n{}", super::AGENT_TOKEN);
    let denied = Branches::new(&config("deny"));
    let output = denied.delete_as("topic", &["squashed"]);
    assert!(!output.status.success(), "{}", printed(&output));
    assert_eq!(denied.deleted(), Vec::<String>::new());
    drop(denied);

    let branches = Branches::new(&config("propose"));
    let pending = branches.delete_as("topic", &["squashed", "rebased"]);
    assert_eq!(pending.status.code(), Some(2), "{}", printed(&pending));
    assert_eq!(branches.deleted(), Vec::<String>::new());
    // One request per branch, each naming the commit it deletes.
    let requests = branches.fixture.ok(&["approvals"]);
    let requests = requests.as_array().unwrap();
    assert_eq!(requests.len(), 2, "{requests:?}");
    for branch in ["squashed", "rebased"] {
        let head = &branches.heads[branch];
        let named = (requests.iter())
            .any(|request| request.to_string().contains(&format!("{branch} at {head}")));
        assert!(named, "{branch}: {requests:?}");
    }
    for request in requests {
        let number = request["number"].as_str().unwrap();
        branches.fixture.ok(&["approvals", "--approve", number]);
    }
    // An approval binds the commit: a branch that moved asks again.
    let mut moved = branches.heads.clone();
    moved.insert("rebased", branches.heads["main"].clone());
    branches.respond_at(&moved, &[], &[]);
    let again = branches.delete_as("topic", &["squashed", "rebased"]);
    assert_eq!(again.status.code(), Some(2), "{}", printed(&again));
    assert_eq!(branches.deleted(), Vec::<String>::new());
    branches.respond(&[], &[]);
    // A login of the user's own in the home's netrc goes unused.
    std::fs::write(
        branches.fixture.root.path().join(".netrc"),
        "machine 127.0.0.1 login netrc password usernetrc\n",
    )
    .unwrap();
    let deleted = branches.delete_as("topic", &["squashed", "rebased"]);
    assert!(deleted.status.success(), "{}", printed(&deleted));
    let agent = |branch: &str| (branch.to_owned(), "token:agenttoken".to_owned());
    assert_eq!(branches.deletions(), [agent("squashed"), agent("rebased")]);
}

#[test]
fn an_agent_deletes_an_active_spaces_branch_only_within_its_scope() {
    let mut branches = Branches::new("[permissions]\npush = 'direct'\n");
    let topic = branches.fixture.id("topic");
    let repo = branches.fixture.repo.to_str().unwrap().to_owned();
    let created = ["create", &repo, "--branch", "child", "--parent", &topic];
    let child = branches.fixture.ok(&created);
    let child = PathBuf::from(child["path"].as_str().unwrap());
    commit(&child, "e");
    branches.heads.insert("child", head(&child));
    for name in ["other", "finished"] {
        branches.fixture.ok(&["create", &repo, "--branch", name]);
        let path =
            branches.fixture.ok(&["status", &branches.fixture.id(name)])["workspace"]["path"]
                .as_str()
                .unwrap()
                .to_owned();
        commit(std::path::Path::new(&path), name);
        branches
            .heads
            .insert(name, head(std::path::Path::new(&path)));
    }
    branches
        .fixture
        .ok(&["done", &branches.fixture.id("finished")]);
    branches.publish();
    branches.respond(&[], &[]);

    let elsewhere = branches.delete_as("topic", &["child", "other", "--force"]);
    assert!(!elsewhere.status.success(), "{}", printed(&elsewhere));
    let stderr = String::from_utf8_lossy(&elsewhere.stderr);
    assert!(
        stderr.contains("other: ") && !stderr.contains("child: "),
        "{stderr}"
    );
    assert_eq!(branches.deleted(), Vec::<String>::new());

    // A workspace whose pushed branch merged goes on in its newer local
    // commits.
    let mut pushed = branches.heads.clone();
    pushed.insert("other", branches.heads["main"].clone());
    branches.respond_at(&pushed, &[], &[]);
    let behind = branches.delete_as("topic", &["other"]);
    assert!(!behind.status.success(), "{}", printed(&behind));
    assert_eq!(branches.deleted(), Vec::<String>::new());
    branches.respond(&[], &[]);

    // Its own branch, one below it and a finished space's go, unmerged ones
    // only with --force.
    let unforced = branches.delete_as("topic", &["topic"]);
    assert!(!unforced.status.success(), "{}", printed(&unforced));
    let deleted = branches.delete_as("topic", &["topic", "child", "finished", "--force"]);
    assert!(deleted.status.success(), "{}", printed(&deleted));
    assert_eq!(branches.deleted(), ["topic", "child", "finished"]);
}

#[test]
fn a_chat_deletes_branches_of_its_repository_as_its_agent_account() {
    let branches = Branches::new(&format!(
        "[permissions]\npush = 'direct'\n{}",
        super::AGENT_TOKEN
    ));
    let fixture = &branches.fixture;
    let (mut agent, chat) = fixture.run_chat("planning", &[]);
    let scope = fixture.request(json!({"issue_scope": {"space": chat}}));
    let token = scope["data"].as_str().unwrap().to_owned();
    branches.respond(&[], &[]);
    let output = fixture.output(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("SHOAL_SPACE", &chat)
            .env("SHOAL_SCOPE_TOKEN", &token)
            .args(["--json", "branch", "delete", "squashed"]),
    );
    agent.kill().unwrap();
    agent.wait().unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(
        branches.deletions(),
        [("squashed".to_owned(), "token:agenttoken".to_owned())]
    );
}
