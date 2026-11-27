//! Migrations 309 and 310: a review's PR URL moves into `target_url`.
use super::*;
use serde_json::{Value, json};

const PR: &str = "https://forge.example/team/repo/pulls/8";

/// A database at the floor with one workspace and a review of each target
/// of `targets`, numbered from 1.
fn with_reviews(targets: &[Value]) -> Result<Connection> {
    let db = at_floor()?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        BEGIN;
        INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
            VALUES ('workspace','/work','worker','ready','abc','/admin');
        INSERT INTO spaces(id,kind,repository_id,name)
            VALUES ('workspace','workspace','repo','worker');
        COMMIT;",
    )?;
    for target in targets {
        db.execute(
            "INSERT INTO reviews(workspace_id,target,opener,agent,state,created_at)
                VALUES ('workspace',?1,'shoal','codex','finished',1)",
            [target.to_string()],
        )?;
    }
    Ok(db)
}

#[test]
fn a_pr_review_s_url_moves_into_its_column_and_a_workspace_review_has_none() -> Result<()> {
    let pr = json!({"kind": "pr", "url": PR, "head": "topic", "base": "main", "post": true});
    let mut db = with_reviews(&[json!({"kind": "workspace"}), pr])?;
    migrate(&mut db)?;
    let mut reading = db.prepare("SELECT target_url,target FROM reviews ORDER BY id")?;
    let rows = reading
        .query_map([], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let rows: Vec<(Option<String>, Value)> = (rows.into_iter())
        .map(|(url, target)| Ok((url, serde_json::from_str(&target)?)))
        .collect::<Result<_>>()?;
    assert_eq!(
        rows,
        [
            (None, json!({"kind": "workspace"})),
            (
                Some(PR.into()),
                json!({"kind": "pr", "head": "topic", "base": "main", "post": true})
            ),
        ]
    );
    Ok(())
}

/// The error migrating a review of `target` fails with.
fn refusal(target: Value) -> Result<String> {
    let mut db = with_reviews(&[json!({"kind": "workspace"}), target])?;
    let error = migrate(&mut db).expect_err("the migration takes the review");
    Ok(format!("{error:#}"))
}

#[test]
fn a_pr_review_without_a_url_fails_the_migration_naming_its_row() -> Result<()> {
    let target = json!({"kind": "pr", "head": "topic", "base": "main", "post": false});
    assert!(refusal(target)?.contains("reviews row 2"));
    Ok(())
}

#[test]
fn a_review_whose_target_has_no_kind_fails_the_migration() -> Result<()> {
    assert!(refusal(json!({}))?.contains("reviews row 2"));
    assert!(refusal(json!([PR]))?.contains("reviews row 2"));
    Ok(())
}
