use super::*;
use crate::daemon::workspace::agent_queue::rank::Carries;
use crate::model::Dependency;

fn edges(waits: &[(&str, &str)]) -> Vec<Edge> {
    (waits.iter())
        .map(|(waiting, on)| Edge {
            waiting: Node::Space((*waiting).into()),
            on: Node::Space((*on).into()),
            dependency: Dependency::Blocked,
            carries: Carries::Rank,
        })
        .collect()
}

fn length(chains: &HashMap<Node, usize>, id: &str) -> usize {
    chains[&Node::Space(id.into())]
}

#[test]
fn a_chain_counts_every_node_waiting_behind_it_along_its_longest_path() {
    let chains = chains(&edges(&[("c", "b"), ("b", "a"), ("x", "a")]));
    let lengths = ["a", "b", "c", "x"].map(|id| length(&chains, id));
    assert_eq!(lengths, [2, 1, 0, 0]);
}

#[test]
fn a_cycles_nodes_count_once_and_share_one_length_whatever_the_order() {
    // A parent waits on its child, which asks it a question, and a third
    // space waits on the parent.
    for waits in [
        [
            ("parent", "child"),
            ("child", "parent"),
            ("other", "parent"),
        ],
        [
            ("other", "parent"),
            ("child", "parent"),
            ("parent", "child"),
        ],
    ] {
        let chains = chains(&edges(&waits));
        let lengths = ["parent", "child", "other"].map(|id| length(&chains, id));
        assert_eq!(lengths, [2, 2, 0]);
    }
}
