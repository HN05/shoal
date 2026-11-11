use super::*;

#[test]
fn a_job_end_lists_what_fits_in_a_frame_and_counts_the_rest() {
    let stopped: Vec<_> = (0..300)
        .map(|pid| StoppedProcess {
            pid,
            command: "x".repeat(crate::daemon::leftovers::COMMAND_LIMIT),
        })
        .collect();
    let ended = ended_job(
        "rust".into(),
        "https://forge.example/runs/6".into(),
        stopped,
    );
    assert_eq!(ended.stopped.len() + ended.omitted, 300);
    assert_eq!(ended.stopped[0].pid, 0);
    assert!(serde_json::to_vec(&ended).unwrap().len() < crate::protocol::MAX_FRAME / 2);
}
