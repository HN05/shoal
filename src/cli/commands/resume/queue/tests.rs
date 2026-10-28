use super::*;

#[test]
fn the_summary_counts_queued_agents() {
    assert_eq!(summary(1), "Queued 1 agent; shoal work --queue shows them");
    assert_eq!(summary(4), "Queued 4 agents; shoal work --queue shows them");
}
