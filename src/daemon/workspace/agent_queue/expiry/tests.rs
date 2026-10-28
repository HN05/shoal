use super::*;
use crate::daemon::store::Store;

async fn database() -> Result<(tempfile::TempDir, Store)> {
    let root = tempfile::tempdir()?;
    let store = Store::open(root.path().join("state.sqlite")).await?;
    store
        .run(|db| {
            db.execute_batch(
                "INSERT INTO repositories(id,path,source,last_used)
                 VALUES ('repo','/unused/repo','local',0);
                 INSERT INTO workspaces(id,repository_id,name,path,branch,state)
                 VALUES ('workspace','repo','work','/unused/work','work','ready');
                 INSERT INTO agent_queue(id,kind,repository_id,name,agent,plan,env,
                     execution_id,state,created_at,seen_at,workspace_id) VALUES
                 ('launch','launch','repo','launch','codex','{}','{}',NULL,'starting',0,99,NULL),
                 ('opened','launch','repo','opened','codex','{}','{}',NULL,'starting',0,99,'workspace'),
                 ('restore','restore','repo','restore','codex',NULL,'{}','execution','starting',0,99,'workspace');",
            )?;
            Ok(())
        })
        .await?;
    Ok((root, store))
}

fn remaining(db: &Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT id FROM agent_queue ORDER BY id")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

#[tokio::test]
async fn expiry_commits_deletions_and_returns_only_reportable_failures() -> Result<()> {
    let (_root, store) = database().await?;
    store
        .run(|db| {
            db.execute_batch(
                "INSERT INTO agent_queue(id,kind,repository_id,name,agent,plan,env,
                     execution_id,state,created_at,seen_at,workspace_id) VALUES
                 ('held','launch','repo','held','codex','{}','{}',NULL,'starting',0,99,NULL),
                 ('boundary','launch','repo','boundary','codex','{}','{}',NULL,'starting',0,100,NULL),
                 ('recent','launch','repo','recent','codex','{}','{}',NULL,'starting',0,101,NULL),
                 ('unseen','launch','repo','unseen','codex','{}','{}',NULL,'starting',0,NULL,NULL),
                 ('queued','launch','repo','queued','codex','{}','{}',NULL,'queued',0,99,NULL),
                 ('wrapper','restore','repo','wrapper','codex',NULL,NULL,'wrapper','starting',0,99,'workspace');",
            )?;
            let held = HashSet::from(["held".to_owned()]);
            let failures = expire_starts(db, &held, 100)?;
            assert_eq!(failures.len(), 2);
            assert!(failures.contains(&FailedStart::UnopenedLaunch("launch".into())));
            assert!(failures.contains(&FailedStart::UnstartedRestore("restore".into())));
            assert_eq!(
                remaining(db)?,
                ["boundary", "held", "queued", "recent", "unseen", "wrapper"]
            );
            assert!(expire_starts(db, &held, 100)?.is_empty());
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_failed_deletion_rolls_back_the_whole_expiry() -> Result<()> {
    let (_root, store) = database().await?;
    store
        .run(|db| {
            // Fail the second deletion regardless of the query's row order.
            db.execute_batch(
                "CREATE TRIGGER fail_expiry AFTER DELETE ON agent_queue
                 WHEN (SELECT count(*) FROM agent_queue) = 1
                 BEGIN SELECT RAISE(ABORT, 'expiry rejected'); END;",
            )?;
            let error = expire_starts(db, &HashSet::new(), 100)
                .expect_err("expiry must fail on the second deletion");
            assert!(error.to_string().contains("expiry rejected"));
            assert_eq!(remaining(db)?, ["launch", "opened", "restore"]);
            db.execute_batch("DROP TRIGGER fail_expiry;")?;
            assert_eq!(expire_starts(db, &HashSet::new(), 100)?.len(), 2);
            assert!(remaining(db)?.is_empty());
            Ok(())
        })
        .await
}
