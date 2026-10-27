use super::*;

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
