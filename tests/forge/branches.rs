//! `shoal branch ls` and `shoal branch delete` against the scripted Forgejo.
use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{Fixture, Forge};
use crate::support::{git, git_with_identity};

/// A commit no checkout has, as the forge may report one.
const UNFETCHED: &str = "f00df00df00df00df00df00df00df00df00df00d";

/// A Forgejo fixture whose checkout has a branch the default branch took by
/// a squash merge, one it took by a rebase merge and one it lacks, and whose
/// `topic` workspace has a commit of its own; `config` adds to the global
/// config.
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
        branches.respond(&[], &[]);
        branches
    }

    /// Have the forge list the fixture's branches and `more`, each as
    /// `(name, commit, protected)`, with the open `pulls`, each as
    /// `(number, head, head repository, base)`.
    fn respond(&self, more: &[(&str, &str, bool)], pulls: &[(u64, &str, &str, &str)]) {
        let mut listed: Vec<_> = (self.heads.iter())
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
        self.fixture.respond(json!({
            "GET repos/team/project/branches?limit=50&page=1": {"body": branches},
            "GET repos/team/project/pulls?state=open&limit=50&page=1": {"body": pulls},
            "GET repos/team/project": {"body": {"default_branch": "main"}},
        }));
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
