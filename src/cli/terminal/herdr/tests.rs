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
fn pane_info_names_the_tab_and_the_agent_status() {
    let info: PaneInfo = serde_json::from_str(
        r#"{"id":"cli:pane:get","result":{"pane":{"agent_status":"working","pane_id":"w2:pAS","tab_id":"w2:tAS"},"type":"pane_info"}}"#,
    )
    .unwrap();
    assert_eq!(info.result.pane.tab_id, "w2:tAS");
    assert_eq!(info.result.pane.agent_status, AgentStatus::Working);
    let older: PaneInfo =
        serde_json::from_str(r#"{"result":{"pane":{"tab_id":"w2:t1"}}}"#).unwrap();
    assert_eq!(older.result.pane.agent_status, AgentStatus::Unknown);
}

#[test]
fn a_finished_or_unseen_turn_reads_as_idle() {
    use AgentStatus::*;
    assert_eq!(Idle.turn(), Some(AgentState::Idle));
    assert_eq!(Done.turn(), Some(AgentState::Idle));
    assert_eq!(Working.turn(), Some(AgentState::Working));
    assert_eq!(Blocked.turn(), Some(AgentState::Waiting));
    assert_eq!(Unknown.turn(), None);
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
fn a_herdr_the_variables_name_needs_none_on_path() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("herdr");
    std::fs::write(&program, "").unwrap();
    let data = BTreeMap::from([("HERDR_BIN_PATH".to_owned(), program.display().to_string())]);
    assert!(ensure_reachable(&data).is_ok());
    assert_eq!(herdr::named_program(&data), program.to_str());
    let gone = BTreeMap::from([(
        "HERDR_BIN_PATH".to_owned(),
        dir.path().join("gone").display().to_string(),
    )]);
    assert_eq!(herdr::named_program(&gone), None);
}
