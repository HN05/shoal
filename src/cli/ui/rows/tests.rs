use super::*;
use crate::{forge::items::link::ItemKind, state::WorkspaceState};

fn workspace(repository: &str, name: &str, branch: &str, state: WorkspaceState) -> Workspace {
    Workspace {
        number: 1,
        ..Workspace::new_record(
            repository.into(),
            name.into(),
            format!("/work/{name}").into(),
            Some(branch.into()),
            state,
        )
    }
}

#[test]
fn queue_status_names_the_work_a_launch_waits_for() {
    let mut entry = QueueEntry {
        mode: None,
        id: "entry".into(),
        kind: QueueKind::Launch,
        repository_id: "repo".into(),
        name: "ui".into(),
        agent: "claude".into(),
        issue_title: None,
        state: crate::state::QueueState::Queued,
        position: Some(2),
        created_at: 0,
        workspace_id: None,
        execution: None,
        opens: None,
        waiting_for: Vec::new(),
        waits_for_update: false,
        base: None,
        job: None,
    };
    assert_eq!(queue_status_cell(&entry).0, "queued, position 2");
    entry.waiting_for = vec!["api".into(), "https://forge.example/o/r/issues/7".into()];
    assert_eq!(
        queue_status_cell(&entry).0,
        "queued, position 2, after api, https://forge.example/o/r/issues/7"
    );
    entry.waiting_for.clear();
    entry.mode = Some("review".into());
    assert_eq!(
        queue_status_cell(&entry).0,
        "queued, review mode, position 2"
    );
}

#[test]
fn queue_status_names_a_restore_that_waits_for_an_update() {
    let mut entry = QueueEntry {
        mode: None,
        id: "entry".into(),
        kind: QueueKind::Restore,
        repository_id: "repo".into(),
        name: "ui".into(),
        agent: "claude".into(),
        issue_title: None,
        state: crate::state::QueueState::Queued,
        position: Some(1),
        created_at: 0,
        workspace_id: Some("ui".into()),
        execution: Some("execution".into()),
        opens: None,
        waiting_for: Vec::new(),
        waits_for_update: true,
        base: None,
        job: None,
    };
    assert_eq!(
        queue_status_cell(&entry).0,
        "queued, position 1, after an update"
    );
    entry.state = crate::state::QueueState::Starting;
    entry.position = None;
    assert!(!queue_status_cell(&entry).0.contains("update"));
}

#[test]
fn usage_shows_cpu_once_measured_and_memory_in_binary_units() {
    let usage = |cpu_percent, memory_bytes| WorkspaceUsage {
        cpu_percent,
        memory_bytes,
        processes: 3,
    };
    assert_eq!(usage_label(&usage(None, 640 << 20)), "640 MiB");
    assert_eq!(
        usage_label(&usage(Some(212.4), 3 << 29)),
        "212% CPU, 1.5 GiB"
    );
    let plain = Palette::stdout(true);
    let mut workspaces = vec![
        workspace("a", "busy", "busy", WorkspaceState::Ready),
        workspace("a", "quiet", "quiet", WorkspaceState::Ready),
    ];
    workspaces[0].usage = Some(usage(Some(50.0), 1 << 20));
    workspaces[0].links.issue_title = Some("Title".into());
    workspaces[1].links.issue_title = Some("Title".into());
    let rows = workspace_tree_rows(&workspaces, &[], &HashSet::new(), plain, None);
    assert_eq!(
        rows,
        [
            "○ 1  busy   inactive  50% CPU, 1 MiB  Title",
            "○ 1  quiet  inactive                  Title",
        ]
    );
}

#[test]
fn tree_rows_list_children_below_their_parent() {
    let plain = Palette::stdout(true);
    let mut workspaces = vec![
        workspace("a", "child", "child", WorkspaceState::Ready),
        workspace("a", "grandchild", "grandchild", WorkspaceState::Ready),
        workspace("a", "orphan", "orphan", WorkspaceState::Ready),
        workspace("a", "parent", "parent", WorkspaceState::Ready),
    ];
    let reference = |w: &Workspace| crate::model::SpaceRef {
        id: w.id.clone(),
        name: w.name.to_string(),
        kind: crate::model::SpaceKind::Workspace,
        branch: w.branch.clone(),
    };
    workspaces[0].parent = Some(reference(&workspaces[3]));
    workspaces[1].parent = Some(reference(&workspaces[0]));
    // A parent left out of the list leaves its child at the top level.
    workspaces[2].parent = Some(crate::model::SpaceRef {
        id: "unlisted".into(),
        name: "unlisted".into(),
        kind: crate::model::SpaceKind::Workspace,
        branch: Some("unlisted".into()),
    });
    let rows = workspace_tree_rows(&workspaces, &[], &HashSet::new(), plain, None);
    let expected = [
        "○ 1  orphan ",
        "○ 1  parent ",
        "○ 1  └ child ",
        "○ 1    └ grandchild ",
    ];
    assert_eq!(rows.len(), expected.len());
    for (row, start) in rows.iter().zip(expected) {
        assert!(row.starts_with(start), "{row:?}");
    }
}

#[test]
fn a_ci_jobs_workspace_lists_under_the_workspace_its_run_links() {
    let plain = Palette::stdout(true);
    let pull = workspace("a", "pull", "pull", WorkspaceState::Ready);
    let mut job = workspace("a", "ci-rust-4", "unused", WorkspaceState::Ready);
    job.branch = None;
    job.usage = Some(crate::model::WorkspaceUsage {
        cpu_percent: Some(250.0),
        memory_bytes: 3 << 29,
        processes: 4,
    });
    job.job = Some(Box::new(
        serde_json::from_value(serde_json::json!({
            "job": {"handle": "h", "id": 4, "run": 1, "name": "rust", "labels": ["shoal"]},
            "run": {"url": "", "workflow": "ci.yml", "commit": "abc", "trigger": {"pull": {"number": 12}}},
            "workspace_id": pull.id,
        }))
        .unwrap(),
    ));
    let rows = workspace_tree_rows(&[job, pull], &[], &HashSet::new(), plain, None);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].starts_with("○ 1  pull "), "{rows:?}");
    assert!(rows[1].starts_with("○ 1  └ ci-rust-4 "), "{rows:?}");
    assert!(rows[1].contains("250% CPU, 1.5 GiB"), "{rows:?}");
    assert!(
        rows[1]
            .trim_end()
            .ends_with("CI job ci.yml / rust for PR #12"),
        "{rows:?}"
    );
}

#[test]
fn width_limits_cut_one_column_and_keep_the_rest_aligned() {
    let plain = Palette::stdout(true);
    let cell = |text: &str| (text.to_owned(), None);
    let rows = || {
        vec![
            [
                cell("question"),
                cell("Which database should the cache use?"),
                cell("SQLite"),
            ],
            [cell("ready"), cell("PR #12"), cell("")],
        ]
    };
    let limit = |width| Some(WidthLimit { column: 1, width });
    assert_eq!(
        aligned_to(rows(), plain, limit(60)),
        aligned(rows(), plain),
        "rows that fit stay whole"
    );
    assert_eq!(
        aligned_to(rows(), plain, limit(40)),
        [
            "question  Which database should…  SQLite",
            "ready     PR #12",
        ]
    );
    // However narrow the limit, the column keeps a few characters.
    assert_eq!(
        aligned_to(rows(), plain, limit(10))[0],
        "question  Which datab…  SQLite"
    );
}

#[test]
fn wrapped_items_fill_lines_up_to_the_width() {
    let items: Vec<String> = ["enter: cd", "tab: next view", "ctrl-d: delete"]
        .map(String::from)
        .into();
    assert_eq!(
        wrapped(&items, "   ", None),
        "enter: cd   tab: next view   ctrl-d: delete"
    );
    assert_eq!(
        wrapped(&items, "   ", Some(26)),
        "enter: cd   tab: next view\nctrl-d: delete"
    );
    // An item wider than the limit gets a line of its own.
    assert_eq!(
        wrapped(&items, "   ", Some(5)),
        "enter: cd\ntab: next view\nctrl-d: delete"
    );
}

#[test]
fn mark_labels_name_the_marked_item() {
    let mark = |kind, url: Option<&str>| ReviewMark {
        kind,
        url: url.map(Into::into),
        head: "abc".into(),
        created_at: 0,
        stale: None,
        warnings: Vec::new(),
    };
    let pr = mark(Some(ItemKind::Pr), Some("https://example.com/o/r/pulls/12"));
    let issue = mark(
        Some(ItemKind::Issue),
        Some("https://example.com/o/r/issues/3"),
    );
    assert_eq!(mark_label(&pr), "PR #12");
    assert_eq!(mark_label(&issue), "issue #3");
    assert_eq!(mark_label(&mark(None, None)), "workspace");
}

#[test]
fn workspace_rows_align_status_and_titles() {
    let plain = Palette::stdout(true);
    let mut workspaces = vec![
        workspace("a", "fix-login", "fix-login", WorkspaceState::Ready),
        workspace("a", "x", "feature/x", WorkspaceState::Failed),
        workspace("a", "new", "new", WorkspaceState::Preparing),
        workspace("a", "busy", "busy", WorkspaceState::Ready),
        workspace("a", "review", "review", WorkspaceState::Ready),
        workspace("a", "quiet", "quiet", WorkspaceState::Ready),
    ];
    workspaces[3].running = true;
    workspaces[3].links.issue_title = Some("Refine the list view".into());
    let mark = |stale| crate::model::ReviewMark {
        kind: None,
        url: None,
        head: "head".into(),
        created_at: 0,
        stale: Some(stale),
        warnings: Vec::new(),
    };
    workspaces[4].running = true;
    workspaces[4].review = vec![mark(false)];
    workspaces[5].review = vec![mark(true)];
    let agent = |state| {
        Some(crate::model::AgentStatus {
            state,
            since: 0,
            prompt_cache_expires_at: None,
        })
    };
    // Needing input outranks completion, which outranks a current mark
    // and other turn states.
    workspaces.extend([
        workspace("a", "asking", "asking", WorkspaceState::Ready),
        workspace("a", "marked", "marked", WorkspaceState::Ready),
        workspace("a", "agent", "agent", WorkspaceState::Ready),
        workspace("a", "done", "done", WorkspaceState::Ready),
        workspace("a", "follow-up", "follow-up", WorkspaceState::Ready),
    ]);
    workspaces[6].review = vec![mark(false)];
    workspaces[6].agent_state = agent(AgentState::Waiting);
    workspaces[7].review = vec![mark(false)];
    workspaces[7].agent_state = agent(AgentState::Working);
    workspaces[8].running = true;
    workspaces[8].agent_state = agent(AgentState::Idle);
    workspaces[9].done = true;
    workspaces[9].review = vec![mark(false)];
    workspaces[9].agent_state = agent(AgentState::Idle);
    workspaces[10].done = true;
    workspaces[10].agent_state = agent(AgentState::Waiting);
    for (number, workspace) in (1..).zip(&mut workspaces) {
        workspace.number = number;
    }
    assert_eq!(
        workspace_rows(
            &workspaces,
            &[("a".into(), "shoal".into())],
            &HashSet::from([workspaces[0].id.clone(), workspaces[1].id.clone()]),
            plain,
            None,
        ),
        [
            "● 1   fix-login  stopped",
            "✗ 2   x          failed",
            "◌ 3   new        preparing",
            "◐ 4   busy       process running   Refine the list view",
            "● 5   review     ready",
            "○ 6   quiet      ready (outdated)",
            "● 7   asking     needs input",
            "● 8   marked     ready",
            "○ 9   agent      waiting",
            "○ 10  done       finished",
            "● 11  follow-up  needs input",
        ]
    );
    let repositories = [("a".into(), "shoal".into()), ("b".into(), "app".into())];
    let workspaces = [
        workspace("a", "fix-login", "fix-login", WorkspaceState::Ready),
        workspace("b", "y", "y", WorkspaceState::Ready),
    ];
    assert_eq!(
        workspace_rows(&workspaces, &repositories, &HashSet::new(), plain, None),
        [
            "○ 1  fix-login  shoal  inactive",
            "○ 1  y          app    inactive"
        ]
    );
}

fn repository(id: &str, source: &str) -> Repository {
    Repository {
        id: id.into(),
        path: source.into(),
        source: source.into(),
        last_used: 0,
        name: None,
        workspaces_dir: None,
        contribution: false,
    }
}

#[tokio::test]
async fn repository_choices_name_distinct_repositories_without_reading_remotes() {
    let reads = std::sync::Mutex::new(Vec::new());
    let entries = repository_entries(
        vec![
            repository("a", "/work/shoal"),
            repository("b", "git@example.com:me/app.git"),
        ],
        async |path: &Path| {
            reads.lock().unwrap().push(path.to_owned());
            Ok(None)
        },
    )
    .await;
    assert_eq!(
        entries,
        [("a".into(), "shoal".into()), ("b".into(), "app".into())]
    );
    assert!(reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn repository_choices_tell_shared_names_apart_by_their_origins_host() {
    let entries = repository_entries(
        vec![
            repository("a", "/work/shoal"),
            repository("b", "/forks/shoal"),
        ],
        async |path: &Path| {
            Ok(Some(match path.to_str() {
                Some("/work/shoal") => "git@example.com:me/shoal.git".into(),
                _ => "https://forge.test/me/shoal.git".into(),
            }))
        },
    )
    .await;
    assert_eq!(
        entries,
        [
            ("a".into(), "shoal (example.com)".into()),
            ("b".into(), "shoal (forge.test)".into())
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn repository_choices_label_unreadable_origins_by_their_recorded_source() {
    let started = tokio::time::Instant::now();
    let entries = repository_entries(
        vec![
            repository("a", "/work/shoal"),
            repository("b", "/stalled/shoal"),
            repository("c", "/missing/shoal"),
        ],
        async |path: &Path| match path.to_str() {
            Some("/stalled/shoal") => std::future::pending().await,
            Some("/missing/shoal") => anyhow::bail!("not a Git checkout"),
            _ => Ok(None),
        },
    )
    .await;
    assert_eq!(
        entries,
        [
            ("a".into(), "shoal (local)  /work/shoal".into()),
            ("b".into(), "shoal (local)  /stalled/shoal".into()),
            ("c".into(), "shoal (local)  /missing/shoal".into())
        ]
    );
    // The reads run at once, so the picker waits for one limit, not one per read.
    assert_eq!(started.elapsed(), REMOTE_READ_LIMIT);
}

#[test]
fn a_chat_in_a_workspace_shows_the_workspace() {
    let chat: Chat = serde_json::from_value(serde_json::json!({
        "id": "c", "number": 4, "repository_id": "a", "name": "worker-chat", "agent": "claude",
        "pid": 1, "started_at": 0,
        "workspace": {"id": "w", "name": "worker", "branch": "worker"},
    }))
    .unwrap();
    let plain = Palette::stdout(true);
    let rows = workspace_tree_rows(&[], &[chat], &HashSet::new(), plain, None);
    assert_eq!(
        rows,
        ["◐ 4  worker-chat  in worker  process running  claude"]
    );
}

#[test]
fn a_chat_shows_its_agents_turn_state_with_its_marker() {
    let chat = |agent_state: serde_json::Value, restore: serde_json::Value| -> Chat {
        serde_json::from_value(serde_json::json!({
            "id": "c", "number": 4, "repository_id": "a", "name": "talk", "agent": "claude",
            "pid": 1, "started_at": 0, "agent_state": agent_state, "restore": restore,
        }))
        .unwrap()
    };
    let plain = Palette::stdout(true);
    let row = |chat: Chat| workspace_tree_rows(&[], &[chat], &HashSet::new(), plain, None);
    let state = |state: &str| serde_json::json!({"state": state, "since": 0});
    let running = serde_json::Value::Null;
    assert_eq!(
        row(chat(state("working"), running.clone())),
        ["◐ 4  talk  working  claude"]
    );
    assert_eq!(
        row(chat(state("waiting"), running.clone())),
        ["● 4  talk  needs input  claude"]
    );
    assert_eq!(
        row(chat(state("idle"), running.clone())),
        ["○ 4  talk  waiting  claude"]
    );
    assert_eq!(
        row(chat(serde_json::Value::Null, "resume".into())),
        ["● 4  talk  stopped  claude"]
    );
}

#[test]
fn spaces_show_a_mode_other_than_their_kinds_usual_one() {
    let chat: Chat = serde_json::from_value(serde_json::json!({
        "id": "c", "number": 4, "repository_id": "a", "name": "repo-plan-7", "agent": "claude",
        "mode": "plan", "pid": 1, "started_at": 0,
        "workspace": {"id": "w", "name": "worker", "branch": "worker"},
    }))
    .unwrap();
    let plain = Palette::stdout(true);
    let rows = workspace_tree_rows(&[], &[chat], &HashSet::new(), plain, None);
    assert_eq!(
        rows,
        ["◐ 4  repo-plan-7  in worker  process running, plan mode  claude"]
    );
    assert_eq!(with_mode("running", Some("worker"), "worker"), "running");
    assert_eq!(with_mode("running", None, "worker"), "running");
    assert_eq!(
        with_mode("running", Some("chat"), "worker"),
        "running, chat mode"
    );
}
