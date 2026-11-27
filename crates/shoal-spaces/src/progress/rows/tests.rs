use super::*;
use crate::state::BlockerKind;
use shoal_protocol::progress::{PlanStep, StepState};

fn blocker(kind: BlockerKind, target: &str, step: Option<StepRef>) -> Stored {
    Stored {
        blocker: RecordedBlocker {
            kind,
            target: target.into(),
            name: target.into(),
            reason: "waits".into(),
            lookup_failed: (kind == BlockerKind::Pr).then(|| "503".into()),
        },
        step,
    }
}

#[test]
fn a_plan_round_trips_through_its_record_and_blocker_rows() -> Result<()> {
    let db = shoal_store::test_support::migrated()?;
    let id = SpaceId::from("space");
    let plan = Plan {
        status: Some("waiting".into()),
        steps: vec![PlanStep {
            name: "build".into(),
            state: StepState::Active,
            key: Some("b".into()),
        }],
        blockers: vec![
            blocker(BlockerKind::Issue, "u1", None),
            blocker(BlockerKind::Pr, "u2", Some(StepRef::Key("b".into()))),
            blocker(
                BlockerKind::Space,
                "id",
                Some(StepRef::Name("build".into())),
            ),
        ],
    };
    write(&db, &id, &plan, UnixTime::from_seconds(5))?;
    let record: String = db.query_row("SELECT record FROM space_progress", [], |row| row.get(0))?;
    assert!(!record.contains("blockers"), "{record}");
    assert_eq!(
        stored(&db, &id)?,
        Some((plan.clone(), UnixTime::from_seconds(5)))
    );
    let fewer = Plan {
        blockers: plan.blockers[1..].to_vec(),
        ..plan
    };
    write(&db, &id, &fewer, UnixTime::from_seconds(6))?;
    assert_eq!(stored(&db, &id)?.map(|(plan, _)| plan), Some(fewer));
    Ok(())
}
