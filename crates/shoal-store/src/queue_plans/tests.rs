use anyhow::Result;
use serde_json::json;

use crate::start_plan::QueuedStart;
use crate::{migrations::migrate, tests::historical_database};
use shoal_protocol::launches::LaunchSource;

/// The plan of the queue entry `id` once `db` migrated.
fn plan(db: &rusqlite::Connection, id: &str) -> Result<QueuedStart> {
    let plan: String = db.query_row("SELECT plan FROM agent_queue WHERE id=?1", [id], |row| {
        row.get(0)
    })?;
    QueuedStart::of(&plan)
}

/// A workspace's queued plan and a chat's both read as the launch they
/// queued, with their agent, task and issue, titled as the row or the
/// chat's links had it, and the source the row recorded.
#[test]
fn queued_plans_of_both_kinds_read_as_the_launch_they_queued() -> Result<()> {
    let mut db = historical_database(234)?;
    let workspace = json!({
        "repository": "repo", "workspace": "workspace", "place_name": null,
        "issue": "https://forge.example/team/repo/issues/7",
        "agent": {"agent": "claude", "model": {"model": "opus"}},
        "args": [{"Unix": [84, 97, 115, 107]}], "prompt": null, "mode": "plan",
        "fork": null, "queue_entry": null,
        "pr": "https://forge.example/team/repo/pulls/8"
    });
    let chat = json!({"parent": "space", "chat_start": {"resolved": {
        "repository": "repo", "name": "talk", "agent": "codex", "mode": "chat",
        "issue": "https://forge.example/team/repo/issues/9",
        "links": {"issues": [{"url": "https://forge.example/team/repo/issues/9", "title": "Talk"}]},
        "prompt": "Discuss it", "args": ["more"]
    }}});
    db.execute(
        r#"INSERT INTO agent_queue(id,kind,repository_id,name,agent,plan,env,state,created_at,opens_workspace_id,issue_title,source)
            VALUES ('workspace','launch','repo','worker','claude',?1,'{}','queued',1,'workspace','Split it','{"kind":"manual"}'),
                ('chat','launch','repo','talk','codex',?2,'{}','queued',1,NULL,NULL,NULL)"#,
        [workspace.to_string(), chat.to_string()],
    )?;
    migrate(&mut db)?;

    let workspace = plan(&db, "workspace")?;
    assert_eq!(workspace.launch.args, ["Task"]);
    assert_eq!(workspace.launch.model.model.as_deref(), Some("opus"));
    let issue = workspace.launch.issue.as_ref().unwrap();
    assert_eq!(issue.title, "Split it");
    assert_eq!(workspace.launch.source, Some(LaunchSource::Manual));
    assert!(workspace.pr.is_some());
    let chat = plan(&db, "chat")?;
    assert_eq!(chat.launch.agent, "codex");
    assert_eq!(chat.launch.prompt.as_deref(), Some("Discuss it"));
    assert_eq!(chat.launch.issue.unwrap().title, "Talk");
    assert!(chat.pr.is_none());
    Ok(())
}
