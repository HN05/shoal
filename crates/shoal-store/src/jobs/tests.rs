use rusqlite::params;
use serde_json::{Value, json};
use shoal_protocol::model::QueuedJob;

use super::{JobColumns, queued_job};

/// A job of a PR's run on the default branch, linked to one space.
fn job() -> QueuedJob {
    serde_json::from_value(json!({
        "job": {"handle": "attempt-2", "id": 7, "run": 3, "name": "rust", "labels": ["shoal"]},
        "run": {"url": "https://forge.example/team/repo/actions/runs/3", "workflow": "ci.yml",
            "commit": "abc", "trigger": {"pull": {"number": 12}}},
        "space_ids": ["work"],
        "default_branch": true,
    }))
    .unwrap()
}

#[test]
fn a_jobs_entry_gives_back_the_job_it_keeps_in_its_handle_and_json() -> anyhow::Result<()> {
    let db = crate::test_support::migrated()?;
    let columns = JobColumns::of(&job())?;
    db.execute(
        "INSERT INTO agent_queue(id,kind,name,agent,state,created_at,job,handle)
         VALUES ('entry','job','ci-rust-7','','queued',1,?1,?2)",
        params![columns.job, columns.handle],
    )?;

    let (read, kept): (Option<QueuedJob>, String) =
        db.query_row("SELECT job,handle FROM agent_queue", [], |row| {
            Ok((queued_job(row)?, row.get("job")?))
        })?;

    assert_eq!(read, Some(job()));
    let kept: Value = serde_json::from_str(&kept)?;
    assert_eq!(kept["job"].get("handle"), None, "{kept}");
    Ok(())
}
