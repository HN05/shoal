use super::*;

#[test]
fn the_summary_counts_queued_agents() {
    assert_eq!(summary(1, false), "Queued 1 agent; shoal queue shows them");
    assert_eq!(summary(4, false), "Queued 4 agents; shoal queue shows them");
}
