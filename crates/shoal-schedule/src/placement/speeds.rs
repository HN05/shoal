//! How fast each machine does a repository's work, from the medians of its
//! runs, against the machines that ran the same work.
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use shoal_ids::member_ids::MemberId;

/// The shortest median a comparison reads: whole seconds say little of a
/// shorter run, whose startup outweighs its work.
const SHORTEST: u64 = 10;

/// What a machine reports of its runs: by repository origin, the median
/// duration in seconds of each run key's whole runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Medians(pub BTreeMap<String, Runs>);

/// The median duration in seconds of each run key's whole runs.
pub type Runs = BTreeMap<String, u64>;

/// A machine's speed at a repository's work, in percent of the machines it
/// shares run keys with there: 200 takes half their time.
pub type Speed = u32;

/// Each machine's speed per repository origin, from the `medians` each
/// reported. A machine compares with another only on the keys both ran,
/// and has no speed at a repository where it shares none.
pub fn speeds<'a>(
    medians: impl IntoIterator<Item = (&'a MemberId, &'a Medians)>,
) -> HashMap<MemberId, BTreeMap<String, Speed>> {
    let mut speeds: HashMap<MemberId, BTreeMap<String, Speed>> = HashMap::new();
    for (origin, machines) in by_origin(medians) {
        for (machine, speed) in speeds_among(&machines) {
            speeds
                .entry(machine.clone())
                .or_default()
                .insert(origin.into(), speed);
        }
    }
    speeds
}

/// The machines that ran each repository's work, with their medians there.
type ByOrigin<'a> = BTreeMap<&'a str, Vec<(&'a MemberId, &'a Runs)>>;

fn by_origin<'a>(medians: impl IntoIterator<Item = (&'a MemberId, &'a Medians)>) -> ByOrigin<'a> {
    let mut by_origin = ByOrigin::new();
    for (machine, medians) in medians {
        for (origin, runs) in &medians.0 {
            by_origin.entry(origin).or_default().push((machine, runs));
        }
    }
    by_origin
}

/// The speed of each of `machines` that shares a key with another, against
/// those it shares keys with.
fn speeds_among<'a>(machines: &[(&'a MemberId, &Runs)]) -> Vec<(&'a MemberId, Speed)> {
    (machines.iter())
        .filter_map(|(machine, runs)| {
            let others = machines.iter().filter(|(other, _)| other != machine);
            let ratios: Vec<f64> = others
                .filter_map(|(_, theirs)| ratio(runs, theirs))
                .collect();
            Some((*machine, percent(mean(&ratios)?)))
        })
        .collect()
}

/// How much faster `ours` ran the keys it shares with `theirs`, as the
/// mean log of their medians' ratios; none without a shared key.
fn ratio(ours: &Runs, theirs: &Runs) -> Option<f64> {
    let logs: Vec<f64> = (ours.iter())
        .filter_map(|(key, ours)| Some((*ours, *theirs.get(key)?)))
        .filter(|(ours, theirs)| *ours >= SHORTEST && *theirs >= SHORTEST)
        .map(|(ours, theirs)| (theirs as f64 / ours as f64).ln())
        .collect();
    mean(&logs)
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// The speed a mean log ratio stands for.
fn percent(log: f64) -> Speed {
    (100.0 * log.exp()).round().clamp(1.0, f64::from(u32::MAX)) as Speed
}

#[cfg(test)]
mod tests;
