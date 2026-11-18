use serde_json::json;

use super::*;

#[test]
fn a_request_with_files_attached_here_is_one_that_attaches_files() {
    let method = |attachments: serde_json::Value| json!({"send_agent_message": {"space": "7", "message": "hi", "attachments": attachments}});
    let attached = method(json!([{"path": "/tmp/report.md"}]));
    assert!(attaches_files(&attached));
    assert!(!attaches_files(&method(json!([]))));
    let rename = json!({"rename_space": {"space": "7", "name": "x"}});
    assert!(!attaches_files(&rename));
    // An issue's files sit within its content.
    let issue = json!({"open_issue": {"space": "7", "issue": {"title": "t",
        "attachments": [{"path": "/tmp/chart.png"}]}, "link": false}});
    assert!(attaches_files(&issue));
}
