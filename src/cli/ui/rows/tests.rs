use super::*;
use crate::state::WorkspaceState;

fn workspace(repository: &str, name: &str, branch: &str, state: WorkspaceState) -> Workspace {
    Workspace::new_record(
        repository.into(),
        name.into(),
        format!("/work/{name}").into(),
        branch.into(),
        state,
    )
}

#[test]
fn queue_status_names_the_work_a_launch_waits_for() {
    let mut entry = QueueEntry {
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
    };
    assert_eq!(queue_status_cell(&entry).0, "queued, position 2");
    entry.waiting_for = vec!["api".into(), "https://forge.example/o/r/issues/7".into()];
    assert_eq!(
        queue_status_cell(&entry).0,
        "queued, position 2, after api, https://forge.example/o/r/issues/7"
    );
}

#[test]
fn queue_status_names_a_restore_that_waits_for_an_update() {
    let mut entry = QueueEntry {
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
            "○ busy   inactive  50% CPU, 1 MiB  Title",
            "○ quiet  inactive                  Title",
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
    let reference = |w: &Workspace| crate::model::WorkspaceRef {
        id: w.id.clone(),
        name: w.name.clone(),
        branch: w.branch.clone(),
    };
    workspaces[0].parent_workspace = Some(reference(&workspaces[3]));
    workspaces[1].parent_workspace = Some(reference(&workspaces[0]));
    // A parent left out of the list leaves its child at the top level.
    workspaces[2].parent_workspace = Some(crate::model::WorkspaceRef {
        id: "unlisted".into(),
        name: "unlisted".into(),
        branch: "unlisted".into(),
    });
    let rows = workspace_tree_rows(&workspaces, &[], &HashSet::new(), plain, None);
    let expected = ["○ orphan ", "○ parent ", "○ └ child ", "○   └ grandchild "];
    assert_eq!(rows.len(), expected.len());
    for (row, start) in rows.iter().zip(expected) {
        assert!(row.starts_with(start), "{row:?}");
    }
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
    let agent = |state| Some(crate::model::AgentStatus { state, since: 0 });
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
    assert_eq!(
        workspace_rows(
            &workspaces,
            &[("a".into(), "shoal".into())],
            &HashSet::from([workspaces[0].id.clone(), workspaces[1].id.clone()]),
            plain,
            None,
        ),
        [
            "● fix-login  stopped",
            "✗ x          failed",
            "◌ new        preparing",
            "◐ busy       process running   Refine the list view",
            "● review     ready",
            "○ quiet      ready (outdated)",
            "● asking     needs input",
            "● marked     ready",
            "○ agent      waiting",
            "○ done       finished",
            "● follow-up  needs input",
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
            "○ fix-login  shoal  inactive",
            "○ y          app    inactive"
        ]
    );
}
