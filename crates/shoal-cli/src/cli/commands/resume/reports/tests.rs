use super::*;

#[test]
fn the_summary_counts_queued_agents() {
    assert_eq!(summary(1, false), "Queued 1 agent; shoal queue shows them");
    assert_eq!(summary(4, false), "Queued 4 agents; shoal queue shows them");
}

#[test]
fn a_stopped_command_reruns_in_its_workspace_by_number() {
    let argv = ["npm".to_owned(), "test".to_owned()];
    assert_eq!(
        rerun(SpaceNumber::new(7), &argv),
        "shoal run --workspace 7 -- npm test"
    );
}
