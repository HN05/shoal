use super::*;
use crate::test_support::{
    Adopted, Detached, copy_test_binary, isolated_command, run_until_stopped_in, runs_until_stopped,
};

/// This test binary, which runs until stopped as [`runs_until_stopped`]
/// tells it to.
fn this_binary() -> PathBuf {
    std::env::current_exe().unwrap()
}

/// A workspace's directories under `root`, which hold no process yet.
fn workspace(root: &Path) -> Space {
    let worktree = root.join("worktree");
    let tmp_dir = root.join("tmp");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&tmp_dir).unwrap();
    Space {
        id: "workspace-id".into(),
        directories: vec![worktree.clone(), tmp_dir],
        programs: Some(worktree),
    }
}

async fn untracked(space: &Space, tracked: Vec<Identity>) -> Vec<Untracked> {
    let mut found = survey(vec![space.clone()], tracked).await.unwrap();
    found.remove(&space.id).unwrap()
}

/// Whether `found` lists `identity`, and as a leftover or not.
fn listed(found: &[Untracked], identity: &Identity) -> Option<bool> {
    found
        .iter()
        .find(|process| process.identity == *identity)
        .map(|process| process.leftover)
}

#[tokio::test]
async fn detached_processes_in_the_worktree_are_leftovers_and_attached_ones_are_not_found() {
    let root = tempfile::tempdir().unwrap();
    let space = workspace(root.path());
    let worktree = space.programs.clone().unwrap();
    let detached = Detached::start(&worktree, &root.path().join("pids")).await;
    let attached = isolated_command("sleep", &worktree)
        .arg("300")
        .spawn()
        .unwrap();
    let attached = super::super::capture(attached.id().unwrap())
        .unwrap()
        .unwrap();

    let found = untracked(&space, vec![]).await;

    assert_eq!(listed(&found, &detached.shell), Some(true), "{found:?}");
    assert_eq!(listed(&found, &detached.child), Some(true), "{found:?}");
    // A process a live parent keeps, such as a shell the user opened there,
    // is not the space's.
    assert_eq!(listed(&found, &attached), None, "{found:?}");
}

#[tokio::test]
async fn the_temporary_directory_and_programs_built_in_the_worktree_tie_detached_processes() {
    let root = tempfile::tempdir().unwrap();
    let space = workspace(root.path());
    let tmp_dir = space.directories[1].clone();
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let built = space.programs.as_ref().unwrap().join("built");
    copy_test_binary(&built).await;

    let in_tmp = Adopted::start(&tmp_dir, "sleep 300", &[], &root.path().join("tmp-pid")).await;
    let running_built = Adopted::start(
        &elsewhere,
        &runs_until_stopped(&built),
        &[],
        &root.path().join("built-pid"),
    )
    .await;
    let unrelated = Adopted::start(&elsewhere, "sleep 300", &[], &root.path().join("pid")).await;

    let found = untracked(&space, vec![]).await;

    assert_eq!(listed(&found, &in_tmp.identity), Some(true), "{found:?}");
    assert_eq!(
        listed(&found, &running_built.identity),
        Some(true),
        "{found:?}"
    );
    assert_eq!(listed(&found, &unrelated.identity), None, "{found:?}");
}

#[tokio::test]
async fn the_identity_marker_finds_attached_processes_and_ties_detached_ones() {
    let root = tempfile::tempdir().unwrap();
    let space = workspace(root.path());
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let marker = [(crate::env::WORKSPACE_ID, space.id.as_str())];
    let detached = Adopted::start(
        &elsewhere,
        &runs_until_stopped(&this_binary()),
        &marker,
        &root.path().join("pid"),
    )
    .await;
    let attached = run_until_stopped_in(&elsewhere)
        .envs(marker)
        .spawn()
        .unwrap();
    let attached = super::super::capture(attached.id().unwrap())
        .unwrap()
        .unwrap();

    let found = untracked(&space, vec![]).await;

    assert_eq!(listed(&found, &detached.identity), Some(true), "{found:?}");
    assert_eq!(listed(&found, &attached), Some(false), "{found:?}");
}

#[tokio::test]
async fn a_leftover_includes_what_it_started_anywhere() {
    let root = tempfile::tempdir().unwrap();
    let space = workspace(root.path());
    let worktree = space.programs.clone().unwrap();
    let pids = root.path().join("pids");
    // The shell runs in the worktree; its child moved out of it.
    let script = format!(
        "sh -c '(cd / && exec sleep 300) & echo $$ $! > {pids}.tmp; mv {pids}.tmp {pids}; wait' \
         >/dev/null 2>&1 &",
        pids = pids.display()
    );
    let status = isolated_command("sh", &worktree)
        .args(["-c", &script])
        .status()
        .await
        .unwrap();
    assert!(status.success());
    let detached = Detached::recorded(&pids).await;

    let found = untracked(&space, vec![]).await;

    assert_eq!(listed(&found, &detached.shell), Some(true), "{found:?}");
    assert_eq!(listed(&found, &detached.child), Some(true), "{found:?}");
}

#[tokio::test]
async fn tracked_processes_their_descendants_and_their_ancestors_are_not_found() {
    let root = tempfile::tempdir().unwrap();
    let space = workspace(root.path());
    let worktree = space.programs.clone().unwrap();
    let detached = Detached::start(&worktree, &root.path().join("pids")).await;

    let found = untracked(&space, vec![detached.shell.clone()]).await;
    assert_eq!(listed(&found, &detached.shell), None, "{found:?}");
    assert_eq!(listed(&found, &detached.child), None, "{found:?}");

    let found = untracked(&space, vec![detached.child.clone()]).await;
    assert_eq!(listed(&found, &detached.shell), None, "{found:?}");
    assert_eq!(listed(&found, &detached.child), None, "{found:?}");
}

#[tokio::test]
async fn a_chat_finds_processes_by_its_marker_and_temporary_directory() {
    let root = tempfile::tempdir().unwrap();
    let tmp_dir = root.path().join("tmp");
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&tmp_dir).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    let chat = Space {
        id: "chat-id".into(),
        directories: vec![tmp_dir.clone()],
        programs: None,
    };
    let marker = [(crate::env::CHAT, "chat-id")];
    let marked = Adopted::start(
        &elsewhere,
        &runs_until_stopped(&this_binary()),
        &marker,
        &root.path().join("pid"),
    )
    .await;
    let in_tmp = Adopted::start(&tmp_dir, "sleep 300", &[], &root.path().join("tmp-pid")).await;

    let found = untracked(&chat, vec![]).await;

    assert_eq!(listed(&found, &marked.identity), Some(true), "{found:?}");
    assert_eq!(listed(&found, &in_tmp.identity), Some(true), "{found:?}");
}

/// The daemon runs the survey, which never lists it, and the hooks it runs
/// carry a workspace's identity; its other children do not stay its.
#[tokio::test]
async fn hooks_the_surveying_process_runs_are_not_found() {
    let root = tempfile::tempdir().unwrap();
    let space = workspace(root.path());
    let start = |hook: bool| {
        let mut command = run_until_stopped_in(root.path());
        command.env(crate::env::WORKSPACE_ID, &space.id);
        if hook {
            command.env(crate::env::HOOK, "post_setup");
        }
        command.spawn().unwrap()
    };
    let (hook, other) = (start(true), start(false));
    let capture = |pid| super::super::capture(pid).unwrap().unwrap();
    let (hook, other) = (
        (capture(hook.id().unwrap()), hook),
        (capture(other.id().unwrap()), other),
    );
    let surveying = capture(std::process::id());

    let found = untracked(&space, vec![]).await;
    assert_eq!(listed(&found, &hook.0), Some(false), "{found:?}");
    let found = untracked(&space, vec![surveying]).await;
    assert_eq!(listed(&found, &hook.0), None, "{found:?}");
    assert_eq!(listed(&found, &other.0), Some(false), "{found:?}");
}

/// A setuid program, such as an agent's `sudo -u`, runs as another user, so
/// it is never found itself, but what it starts as this user still descends
/// from the agent that started it.
#[test]
fn ancestry_passes_through_setuid_programs() {
    use super::super::{Evidence, Inventory, Lineage, Marks, Observed, owners::Owners};
    let process = |pid: u32, parent: u32, evidence| {
        let lineage = Lineage {
            identity: Identity {
                pid,
                birth: format!("1:{pid}"),
                unique: None,
            },
            unique: None,
            parent: None,
            parent_pid: parent,
            adopted: false,
        };
        (pid, Observed { lineage, evidence })
    };
    let marked = || {
        Evidence::Marked(Marks {
            workspace: Some("workspace-id".into()),
            ..Marks::default()
        })
    };
    let (agent, sudo, child) = (1_000_000, 1_000_001, 1_000_002);
    let owners = Owners::default();
    let inventory = Inventory {
        observed: HashMap::from([
            process(agent, 1, marked()),
            process(sudo, agent, Evidence::Elevated),
            process(child, sudo, marked()),
        ]),
        ..Inventory::new(&owners)
    };
    let space = Space {
        id: "workspace-id".into(),
        directories: Vec::new(),
        programs: None,
    };
    let tracked = inventory.observed[&agent].lineage.identity.clone();

    let survey = Survey::of(&inventory, &[tracked]);
    assert!(survey.untracked(&space).is_empty());

    let survey = Survey::of(&inventory, &[]);
    let found: Vec<u32> = survey
        .untracked(&space)
        .iter()
        .map(|process| process.identity.pid)
        .collect();
    assert!(found.contains(&child), "{found:?}");
    assert!(!found.contains(&sudo), "{found:?}");
}
