use super::*;

#[test]
fn hook_reads_are_cancellable() {
    let updates = Operation::from(AgentMethod::AgentUpdates { space: "w".into() });
    assert!(updates.reads_only());
    assert!(Operation::from(DaemonMethod::Status).reads_only());
}

#[test]
fn downloads_stop_when_their_client_hangs_up() {
    let download = ItemMethod::ForgeDownload {
        context: super::super::ItemContext::Workspace("w".into()),
        url: "https://forge.example/attachments/a1".into(),
        repository: None,
    };
    assert!(Operation::from(download).reads_only());
}

#[test]
fn reads_that_finish_work_run_to_the_end() {
    // Planning moves first finishes an interrupted one.
    let plan = RepositoryMethod::PlanRepositoryMoves {
        root: None,
        regular_dir: false,
    };
    assert!(!Operation::from(plan).reads_only());
}
