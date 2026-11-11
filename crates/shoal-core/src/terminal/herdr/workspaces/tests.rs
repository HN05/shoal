use super::*;

#[test]
fn the_first_workspace_with_the_label_is_used() {
    let listed: Response<Listed> = serde_json::from_str(
        r#"{"id":"cli:workspace:list","result":{"type":"workspace_list","workspaces":[
            {"workspace_id":"w1","label":"notes","number":1},
            {"workspace_id":"w2","label":"shoal","number":2},
            {"workspace_id":"w3","label":"shoal","number":3},
            {"workspace_id":"w4","number":4}]}}"#,
    )
    .unwrap();
    let workspaces = listed.result.workspaces;
    assert_eq!(first_labelled(&workspaces, "shoal"), Some("w2"));
    assert_eq!(first_labelled(&workspaces, "other"), None);
}

#[test]
fn a_created_workspace_is_read_from_the_response() {
    let created: Response<Created> = serde_json::from_str(
        r#"{"id":"cli:workspace:create","result":{"root_pane":{"pane_id":"w5:p1"},"tab":{"tab_id":"w5:t1"},"type":"workspace_created","workspace":{"label":"shoal","workspace_id":"w5"}}}"#,
    )
    .unwrap();
    assert_eq!(created.result.workspace.workspace_id, "w5");
}
