use super::*;
use crate::{
    model::{Completion, Execution, Inspection, Workspace},
    paths::Paths,
    protocol::{self, Body, Method, Operation, Request, Response, WorkspaceMethod},
    state::{ExecutionState, WorkspaceState},
};
use tokio::{io::BufReader, net::UnixListener};

#[tokio::test]
async fn kept_completion_waits_for_running_and_unresolved_executions() {
    for state in [ExecutionState::Running, ExecutionState::Unknown] {
        let execution = Execution {
            id: "agent".into(),
            workspace_id: "workspace".into(),
            state,
            wrapper: None,
            child: None,
            group_id: None,
        };
        assert_eq!(
            inspect_lifecycle(Some(false), vec![execution]).await,
            Lifecycle::Kept {
                running: true,
                stopped: false
            }
        );
    }
    assert_eq!(
        inspect_lifecycle(Some(false), vec![]).await,
        Lifecycle::Kept {
            running: false,
            stopped: false
        }
    );
    assert_eq!(inspect_lifecycle(None, vec![]).await, Lifecycle::Active);
    // Completion cleanup closes the tab only once it removes the workspace.
    assert_eq!(
        inspect_lifecycle(Some(true), vec![]).await,
        Lifecycle::Active
    );
    assert_eq!(
        inspect_with_records(Some(false), vec![], true).await,
        Lifecycle::Kept {
            running: false,
            stopped: true
        }
    );
}

#[test]
fn kept_completion_closes_the_tab_once_the_agent_finishes_its_turn() {
    use AgentStatus::*;
    let completed = |running, stopped| Lifecycle::Kept { running, stopped };
    let running = completed(true, false);
    for status in [Idle, Done] {
        assert!(running.closes_tab(status));
    }
    for status in [Working, Blocked, Unknown] {
        assert!(!running.closes_tab(status));
    }
    for status in [Idle, Working, Blocked, Done, Unknown] {
        assert!(completed(false, false).closes_tab(status));
        assert!(!completed(false, true).closes_tab(status));
        assert!(!completed(true, true).closes_tab(status));
        assert!(Lifecycle::Removed.closes_tab(status));
        assert!(!Lifecycle::Active.closes_tab(status));
    }
}

#[test]
fn only_a_shell_in_the_foreground_is_at_its_prompt() {
    let processes = |foreground: i64| {
        let output = format!(
            r#"{{"id":"cli:pane:process_info","result":{{"process_info":{{"foreground_process_group_id":{foreground},"foreground_processes":[],"pane_id":"w2:p6","shell_pid":58481}},"type":"pane_process_info"}}}}"#
        );
        serde_json::from_str::<ProcessInfo>(&output)
            .unwrap()
            .result
            .process_info
    };
    assert!(processes(58481).shell_in_foreground());
    assert!(!processes(58578).shell_in_foreground());
    let unknown: ProcessInfo =
        serde_json::from_str(r#"{"result":{"process_info":{"pane_id":"w2:p6"}}}"#).unwrap();
    assert!(!unknown.result.process_info.shell_in_foreground());
}

#[test]
fn tabs_without_an_issue_use_the_template_or_branch() {
    let repo = Repository {
        id: "repo".into(),
        path: "/repo".into(),
        source: "https://example.com/owner/shoal.git".into(),
        last_used: 0,
        name: Some("shoal".into()),
        workspaces_dir: None,
        contribution: false,
    };
    assert_eq!(tab_label(None, &repo, None, "topic"), "topic");
    assert_eq!(
        tab_label(Some("{repo}: {branch}".into()), &repo, None, "topic"),
        "shoal: topic"
    );
}

#[test]
fn tab_info_reads_the_agent_status() {
    let status = |value: &str| {
        let output = format!(
            r#"{{"id":"cli:tab:get","result":{{"tab":{{"agent_status":"{value}","tab_id":"w2:t1"}},"type":"tab_info"}}}}"#
        );
        serde_json::from_str::<TabInfo>(&output)
            .unwrap()
            .result
            .tab
            .agent_status
    };
    assert_eq!(status("done"), AgentStatus::Done);
    assert_eq!(status("idle"), AgentStatus::Idle);
    assert_eq!(status("working"), AgentStatus::Working);
    assert_eq!(status("sleeping"), AgentStatus::Unknown);
    let older: TabInfo = serde_json::from_str(r#"{"result":{"tab":{"tab_id":"w2:t1"}}}"#).unwrap();
    assert_eq!(older.result.tab.agent_status, AgentStatus::Unknown);
}

#[test]
fn only_herdr_reporting_the_tab_missing_proves_it_gone() {
    assert!(tab_not_found(
        br#"{"error":{"code":"tab_not_found","message":"tab w2:t9 not found"},"id":"cli:tab:get"}"#
    ));
    assert!(!tab_not_found(
            br#"{"id":"cli:tab:get","error":{"code":"server_not_running","message":"no herdr server is running"}}"#
        ));
    assert!(!tab_not_found(b"connection reset"));
}

/// `completion` is the recorded cleanup choice.
async fn inspect_lifecycle(completion: Option<bool>, executions: Vec<Execution>) -> Lifecycle {
    inspect_with_records(completion, executions, false).await
}

async fn inspect_with_records(
    completion: Option<bool>,
    executions: Vec<Execution>,
    stopped: bool,
) -> Lifecycle {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(temp.path());
    paths.prepare().unwrap();
    if stopped {
        crate::execution::recovery::save_record(&paths, "workspace", "agent", "codex", None)
            .unwrap();
    }
    let listener = UnixListener::bind(&paths.socket).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let request: Request = protocol::read_buffered(&mut stream).await.unwrap();
        assert!(
            matches!(request.method, Method::Operation(Operation::Workspace(WorkspaceMethod::InspectWorkspace { workspace })) if workspace == "workspace")
        );
        let inspection = Inspection {
            workspace: Workspace::new_record(
                "repo".into(),
                "workspace".into(),
                "/workspace".into(),
                "topic".into(),
                WorkspaceState::Ready,
            ),
            completion: completion.map(|cleanup| Completion {
                head: "head".into(),
                cleanup,
                error: None,
            }),
            executions,
            issue: None,
            pr_cleanup: None,
            ports: vec![],
            resources: vec![],
            simulators: vec![],
        };
        protocol::write(
            stream.get_mut(),
            &Response::new(request.id, Body::Inspection(inspection)),
        )
        .await
        .unwrap();
    });
    let lifecycle = workspace_lifecycle(&Context::new(paths, true), "workspace")
        .await
        .unwrap();
    server.await.unwrap();
    lifecycle
}

#[test]
fn shell_command_quotes_only_words_that_need_it() {
    let argv: Vec<OsString> = ["/opt/bin/shoal", "internal", "herdr", "a b", "it's", ""]
        .map(Into::into)
        .into();
    assert_eq!(
        shell_command(&argv).unwrap(),
        r#"/opt/bin/shoal internal herdr 'a b' 'it'\''s' ''"#
    );
}

#[test]
fn a_closing_tab_is_recorded_until_its_watcher_ends() {
    let root = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    let path = ClosingTab::path(&paths, "workspace", "tab-1").unwrap();
    let closing = ClosingTab::record(&paths, "workspace", "tab-1");
    assert!(path.exists());
    drop(closing);
    assert!(!path.exists());
    assert_eq!(ClosingTab::path(&paths, "workspace", "a/b"), None);
    assert_eq!(ClosingTab::path(&paths, "workspace", ""), None);
}
