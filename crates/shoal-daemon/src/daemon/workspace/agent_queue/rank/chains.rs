//! How much work waits on each node, for the `speed` schedule: the most
//! nodes on any chain of waits that ends at it. Waits may form cycles, such
//! as a parent waiting on its child while the child asks it a question, so
//! each cycle's nodes count once and share one length, whatever order they
//! are visited in.
use std::collections::{HashMap, HashSet};

use super::{Edge, Node};

/// The length of the longest chain of waits ending at each node of `edges`.
pub(super) fn chains(edges: &[Edge]) -> HashMap<Node, usize> {
    let mut search = Components::new(edges);
    let nodes: Vec<&Node> = (edges.iter())
        .flat_map(|edge| [&edge.waiting, &edge.on])
        .collect();
    for node in nodes {
        if !search.index.contains_key(node) {
            search.visit(node);
        }
    }
    lengths(edges, &search.components)
}

/// Each component's length, from the components that wait on it, which
/// come first in reverse: a component's own other nodes wait on each of
/// its nodes, and the longest chain behind a component that waits on it
/// adds that component's nodes.
fn lengths(edges: &[Edge], components: &[Vec<&Node>]) -> HashMap<Node, usize> {
    let component: HashMap<&Node, usize> = (components.iter().enumerate())
        .flat_map(|(index, nodes)| nodes.iter().map(move |node| (*node, index)))
        .collect();
    let mut length = vec![0; components.len()];
    for index in (0..components.len()).rev() {
        let behind = (edges.iter())
            .filter(|edge| component[&edge.on] == index)
            .map(|edge| component[&edge.waiting])
            .filter(|waiting| *waiting != index)
            .map(|waiting| length[waiting] + components[waiting].len())
            .max()
            .unwrap_or(0);
        length[index] = components[index].len() - 1 + behind;
    }
    (components.iter().enumerate())
        .flat_map(|(index, nodes)| nodes.iter().map(move |node| ((*node).clone(), index)))
        .map(|(node, index)| (node, length[index]))
        .collect()
}

/// Tarjan's search for the strongly connected components of the waits,
/// which it finds each after every component its nodes wait on.
struct Components<'a> {
    waits_on: HashMap<&'a Node, Vec<&'a Node>>,
    index: HashMap<&'a Node, usize>,
    low: HashMap<&'a Node, usize>,
    stack: Vec<&'a Node>,
    stacked: HashSet<&'a Node>,
    components: Vec<Vec<&'a Node>>,
}

impl<'a> Components<'a> {
    fn new(edges: &'a [Edge]) -> Self {
        let mut waits_on: HashMap<&Node, Vec<&Node>> = HashMap::new();
        for edge in edges {
            waits_on.entry(&edge.waiting).or_default().push(&edge.on);
        }
        Self {
            waits_on,
            index: HashMap::new(),
            low: HashMap::new(),
            stack: Vec::new(),
            stacked: HashSet::new(),
            components: Vec::new(),
        }
    }

    fn visit(&mut self, node: &'a Node) {
        let index = self.index.len();
        self.index.insert(node, index);
        self.low.insert(node, index);
        self.stack.push(node);
        self.stacked.insert(node);
        let next = self.waits_on.get(node).cloned().unwrap_or_default();
        for on in next {
            if !self.index.contains_key(on) {
                self.visit(on);
                self.lower(node, self.low[on]);
            } else if self.stacked.contains(on) {
                self.lower(node, self.index[on]);
            }
        }
        if self.low[node] == index {
            let mut component = Vec::new();
            while let Some(member) = self.stack.pop() {
                self.stacked.remove(member);
                component.push(member);
                if member == node {
                    break;
                }
            }
            self.components.push(component);
        }
    }

    fn lower(&mut self, node: &'a Node, to: usize) {
        let low = self
            .low
            .get_mut(node)
            .expect("a visited node has a low link");
        *low = (*low).min(to);
    }
}

#[cfg(test)]
mod tests;
