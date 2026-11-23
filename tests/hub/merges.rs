//! Merging a space another joined machine runs into the default branch of
//! that machine's checkout, and undoing that merge, from the hub.
use std::{fs, path::PathBuf};

use serde_json::Value;

use super::{
    directory::{join, listed, machine, workspace},
    routing::{Tree, tree_across_machines},
    support::{fixture::Base, git, git_with_identity, printed, shoal_binary},
};

/// The hub and a laptop joined to it, with a workspace on the laptop that
/// the hub lists and a commit there of its own, by ID, with its worktree.
fn laptop_work(laptop: Base) -> (Base, Base, String, PathBuf) {
    let hub = machine(true, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    let there = workspace(&laptop, "on-laptop");
    listed(&hub, &there, &laptop_id);
    let path = commit_in(&laptop, &there);
    (hub, laptop, there, path)
}

/// Commit a file in the worktree of `machine`'s workspace `id`, returning
/// the worktree.
fn commit_in(machine: &Base, id: &str) -> PathBuf {
    let path: PathBuf = machine.ok(&["status", id])["workspace"]["path"]
        .as_str()
        .unwrap()
        .into();
    fs::write(path.join("notes"), "from the laptop\n").unwrap();
    git(&path, &["add", "notes"]);
    git_with_identity(&path, &["commit", "-q", "-m", "notes"]);
    path
}

#[test]
fn the_user_s_terminal_merges_and_unmerges_a_workspace_on_another_machine() {
    let (hub, laptop, there, path) = laptop_work(machine(false, "app"));
    let main = || git(&laptop.repo, &["rev-parse", "main"]);
    let (before, head) = (main(), git(&path, &["rev-parse", "HEAD"]));
    let merged = hub.ok(&["merge", "-s", &there]);
    assert_eq!(merged["updated"], true, "{merged}");
    assert_eq!(main(), head);
    // The hub's own checkout is no part of it.
    assert_ne!(git(&hub.repo, &["rev-parse", "main"]), head);
    let unmerged = hub.ok(&["unmerge", "-s", &there]);
    assert_eq!(unmerged["commit"], before.trim(), "{unmerged}");
    assert_eq!(main(), before);
}

#[test]
fn a_coordinator_merges_its_child_on_another_machine_once_approved() {
    let Tree {
        hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    let path = commit_in(&laptop, &child);
    let head = git(&path, &["rev-parse", "HEAD"]);
    let merge = || {
        let mut command = hub.command();
        command.args(["run", "--workspace", &coordinator, "--", shoal_binary()]);
        hub.output(command.args(["--json", "merge", "-s", &child]))
    };
    let pending = merge();
    assert_eq!(pending.status.code(), Some(2), "{}", printed(&pending));
    let pending: Value = serde_json::from_slice(&pending.stdout).unwrap();
    assert_eq!(pending["request"]["target"], "merge", "{pending}");
    assert_eq!(pending["request"]["specification"]["head"], head.trim());
    let number = pending["request"]["number"].as_str().unwrap();
    hub.ok(&["approvals", "--approve", number]);
    let merged = merge();
    assert!(merged.status.success(), "{}", printed(&merged));
    // The move cloned the child's repository on the laptop, whose refs its
    // worktree shares.
    assert_eq!(git(&path, &["rev-parse", "main"]), head);
}
