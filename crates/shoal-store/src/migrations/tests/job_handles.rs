//! Migrations 340 and 341: a CI job's handle moves into a column of the
//! queue entry or claimed job that records it.
use super::*;
use serde_json::{Value, json};

/// A job as an earlier Shoal recorded it, with its `handle` inside.
fn job(handle: &str) -> Value {
    json!({
        "job": {"handle": handle, "id": 7, "run": 3, "name": "rust", "labels": ["shoal"]},
        "run": {"url": "https://forge.example/team/repo/actions/runs/3", "commit": "abc",
            "trigger": "other"},
    })
}

/// A database at the floor with the repository `repo`, its workspace
/// `work`, a job's queue entry and a claimed job, each of `job`.
fn with_jobs(queued: &Value, claimed: &Value) -> Result<Connection> {
    let db = at_floor()?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         BEGIN;
         INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
             VALUES ('work','/work','work','ready','abc','/admin');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('work','workspace','repo','work');
         COMMIT;",
    )?;
    db.execute(
        "INSERT INTO agent_queue(seq,id,kind,repository_id,name,agent,state,created_at,job)
         VALUES (5,'entry','job','repo','ci-rust-7','','queued',1,?1)",
        [queued.to_string()],
    )?;
    db.execute(
        "INSERT INTO ci_jobs(id,repository_id,workspace_id,job,started_at)
         VALUES ('claim','repo','work',?1,1)",
        [claimed.to_string()],
    )?;
    Ok(db)
}

#[test]
fn a_job_keeps_its_handle_in_its_column() -> Result<()> {
    let mut db = with_jobs(&job("queued-1"), &job("claimed-1"))?;
    // A later entry took and left a higher number, which no entry reuses.
    db.execute(
        "UPDATE sqlite_sequence SET seq=9 WHERE name='agent_queue'",
        [],
    )?;

    migrate(&mut db)?;

    let mut without_handle = job("");
    without_handle["job"]
        .as_object_mut()
        .unwrap()
        .remove("handle");
    for (table, id, handle) in [
        ("agent_queue", "entry", "queued-1"),
        ("ci_jobs", "claim", "claimed-1"),
    ] {
        let (kept, column): (String, String) = db.query_row(
            &format!("SELECT job,handle FROM {table} WHERE id=?1"),
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(column, handle);
        assert_eq!(serde_json::from_str::<Value>(&kept)?, without_handle);
    }
    let next: i64 = db.query_row(
        "INSERT INTO agent_queue(id,kind,name,agent,state,created_at,job,handle)
         VALUES ('next','job','ci-rust-8','','queued',1,'{}','other') RETURNING seq",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(next, 10);
    // A repository queues each handle once.
    let again = db.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,job,handle)
         VALUES ('again','job','repo','ci-rust-7','','queued',1,'{}','queued-1')",
        [],
    );
    assert!(again.is_err());
    Ok(())
}

#[test]
fn a_job_without_its_handle_stops_the_migration_naming_its_row() -> Result<()> {
    let mut handleless = job("");
    handleless["job"].as_object_mut().unwrap().remove("handle");
    let mut db = with_jobs(&job("queued-1"), &handleless)?;

    let error = format!("{:#}", migrate(&mut db).unwrap_err());

    assert!(error.contains("ci_jobs row claim"), "{error}");
    assert!(error.contains("has no handle"), "{error}");
    Ok(())
}
