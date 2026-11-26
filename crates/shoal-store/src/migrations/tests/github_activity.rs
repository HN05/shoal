//! Migration 302: GitHub activity records take the REST shape.
use super::*;
use serde_json::{Value, json};

/// The fingerprint GitHub's GraphQL shape gave a comment or review.
fn graphql(id: Value, body: &str, state: Value) -> String {
    json!({"id": id, "body": body, "state": state, "lastEditedAt": null}).to_string()
}

/// The record `table` holds after migrating `record` from the floor.
fn migrated_record(table: &str, record: &Value) -> Result<Value> {
    let mut db = at_floor()?;
    db.execute(
        &format!("INSERT INTO {table}(space_id,url,record) VALUES ('space','url',?1)"),
        [record.to_string()],
    )?;
    migrate(&mut db)?;
    let text: String =
        db.query_row(&format!("SELECT record FROM {table}"), [], |row| row.get(0))?;
    Ok(serde_json::from_str(&text)?)
}

#[test]
fn github_comments_and_reviews_take_their_database_ids_and_rest_fingerprints() -> Result<()> {
    let comment = "IC_kwDODKw3uc8AAAABrIBVhQ";
    let review = "MDE3OlB1bGxSZXF1ZXN0UmV2aWV3NDI0Mg==";
    let record = json!({
        "kind": "pr",
        "comments": {
            format!("discussion:{comment}"):
                {"fingerprint": graphql(comment.into(), "hello", Value::Null), "author": "a"},
            format!("reviews:{review}"):
                {"fingerprint": graphql(review.into(), "", "APPROVED".into()), "author": "b"},
            "inline:3": {"fingerprint": graphql(3.into(), "finding", Value::Null)},
            "discussion:IC_unreadable": {"fingerprint": graphql("x".into(), "", Value::Null)},
            "verdict:b": {"fingerprint": "APPROVED", "author": "b"},
        },
        "checks": {},
        "authored": [format!("discussion:{comment}"), "inline:3"],
    });
    let record = migrated_record("pr_activity", &record)?;
    assert_eq!(
        record["comments"],
        json!({
            "discussion:7189058949": {"fingerprint": r#"{"body":"hello"}"#, "author": "a"},
            "reviews:4242":
                {"fingerprint": r#"{"body":"","state":"APPROVED"}"#, "author": "b"},
            "inline:3": {"fingerprint": r#"{"body":"finding"}"#},
            "verdict:b": {"fingerprint": "APPROVED", "author": "b"},
        })
    );
    assert_eq!(
        record["authored"],
        json!(["discussion:7189058949", "inline:3"])
    );
    Ok(())
}

#[test]
fn other_forges_records_stay_as_they_were() -> Result<()> {
    let record = json!({
        "kind": "issue",
        "comments": {"discussion:9": {"fingerprint": r#"{"body":"hi","updated_at":"t"}"#}},
        "checks": {},
    });
    assert_eq!(migrated_record("space_item_activity", &record)?, record);
    Ok(())
}
