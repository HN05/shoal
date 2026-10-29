use super::*;

#[test]
fn hook_reads_are_cancellable() {
    let updates = Operation::from(AgentMethod::AgentUpdates {
        workspace: "w".into(),
    });
    assert!(updates.reads_only());
    assert!(Operation::from(DaemonMethod::Status).reads_only());
}

#[test]
fn reads_that_finish_or_record_work_run_to_the_end() {
    // Planning moves first finishes an interrupted one; a forge type is
    // recorded once learned.
    let plan = RepositoryMethod::PlanRepositoryMoves {
        root: None,
        regular_dir: false,
    };
    let forge = ItemMethod::ForgeType {
        host: "example.com".into(),
        scheme: "https".into(),
    };
    for operation in [plan.into(), forge.into()] {
        let operation: Operation = operation;
        assert!(!operation.reads_only(), "{operation:?}");
    }
}
