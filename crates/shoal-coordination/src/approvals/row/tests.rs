use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::config::repo::Lifetime;
use shoal_store::test_support::seeds;

use super::super::{
    ApprovalRequest, ApprovalRequestExt, GateStep, HoldSpecification, Specification, Target, store,
};
use super::*;
use crate::stand_in::Fixture;

fn hold(owner: &SpaceId) -> ApprovalRequest {
    let mut request = ApprovalRequest::new(
        owner,
        Target::Hold("review".into()),
        "review",
        Specification::Hold(HoldSpecification {
            hold: "review".into(),
        }),
        Lifetime::Workspace,
        Some("keep it for review"),
    );
    request.key = "hold".into();
    request.space = Some("worker".parse().unwrap());
    request.number = ItemNumber {
        space: 1.into(),
        item: 4,
    };
    request
}

/// `request` as its row gives it back.
fn stored(db: &Connection, request: &ApprovalRequest) -> Result<serde_json::Value> {
    let read = store::by_key(db, &request.key)?.expect("the request is stored");
    Ok(serde_json::to_value(read)?)
}

#[tokio::test]
async fn a_request_comes_back_from_its_columns_and_record_as_it_went_in() -> Result<()> {
    let fixture = &Fixture::new().await;
    fixture
        .store()
        .run(|db| {
            seeds::ready_workspace(db, "repo", "owner", "worker")?;
            let mut request = hold(&SpaceId::from("owner"));
            insert(db, &request)?;
            assert_eq!(stored(db, &request)?, serde_json::to_value(&request)?);
            request.status = DecisionStatus::Approved;
            request.decided_at = Some(UnixTime::now());
            request.decider = Some(SpaceId::from("owner"));
            request.routed = true;
            request.operation = Some("push-1".into());
            request.warnings = vec!["the comment lost its files".into()];
            request.active = false;
            request.gate = Some(GateStep::Escalated {
                reason: "unsure".into(),
                rule: "ask the user".into(),
            });
            write(db, &request)?;
            assert_eq!(stored(db, &request)?, serde_json::to_value(&request)?);
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_request_its_repository_owns_names_its_space_as_the_proposer() -> Result<()> {
    let fixture = &Fixture::new().await;
    fixture
        .store()
        .run(|db| {
            seeds::ready_workspace(db, "repo", "owner", "worker")?;
            let mut request = hold(&SpaceId::from("owner"));
            request.repository_id = Some("repo".into());
            request.gate = Some(GateStep::Unapplied {
                error: "the push failed".into(),
            });
            insert(db, &request)?;
            let owner: (Option<String>, Option<String>) = db.query_row(
                "SELECT space_id,proposer_id FROM access_requests WHERE id='hold'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(owner, (None, Some("owner".into())));
            assert_eq!(stored(db, &request)?, serde_json::to_value(&request)?);
            Ok(())
        })
        .await
}

#[tokio::test]
async fn details_the_gate_s_state_does_not_take_are_refused() -> Result<()> {
    let fixture = &Fixture::new().await;
    fixture
        .store()
        .run(|db| {
            seeds::ready_workspace(db, "repo", "owner", "worker")?;
            let mut request = hold(&SpaceId::from("owner"));
            request.gate = Some(GateStep::Unjudged {
                reason: "the space is gone".into(),
            });
            insert(db, &request)?;
            db.execute("UPDATE access_requests SET gate='allowed'", [])?;
            assert!(store::by_key(db, "hold").is_err());
            db.execute("UPDATE access_requests SET gate=NULL", [])?;
            assert!(store::by_key(db, "hold").is_err());
            Ok(())
        })
        .await
}
