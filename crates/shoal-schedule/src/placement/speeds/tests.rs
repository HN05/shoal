use std::collections::BTreeMap;

use shoal_ids::member_ids::MemberId;

use super::{Medians, speeds};

const ORIGIN: &str = "example.com/owner/repo";

/// Medians of `ORIGIN`'s runs, by key.
fn medians(runs: &[(&str, u64)]) -> Medians {
    let runs = (runs.iter())
        .map(|(key, seconds)| ((*key).to_owned(), *seconds))
        .collect();
    Medians(BTreeMap::from([(ORIGIN.to_owned(), runs)]))
}

/// Each machine's speed at `ORIGIN`, by name, none where it has none.
fn speeds_of(machines: &[(&str, Medians)]) -> Vec<(String, Option<u32>)> {
    let ids: Vec<(MemberId, &Medians)> = (machines.iter())
        .map(|(name, medians)| ((*name).into(), medians))
        .collect();
    let speeds = speeds(ids.iter().map(|(id, medians)| (id, *medians)));
    (ids.iter())
        .map(|(id, _)| {
            let speed = speeds.get(id).and_then(|speeds| speeds.get(ORIGIN));
            (id.to_string(), speed.copied())
        })
        .collect()
}

#[test]
fn machines_compare_only_on_the_keys_both_ran() {
    let machines = [
        // Its build takes half the time; its lint, which the other never
        // ran, says nothing of how it compares.
        ("fast", medians(&[("build", 300), ("lint", 2000)])),
        ("slow", medians(&[("build", 600), ("test", 100)])),
    ];
    assert_eq!(
        speeds_of(&machines),
        [("fast".into(), Some(200)), ("slow".into(), Some(50))]
    );
}

#[test]
fn a_machine_sharing_no_key_has_no_speed() {
    let machines = [
        ("fast", medians(&[("build", 300)])),
        ("slow", medians(&[("build", 600)])),
        ("other", medians(&[("test", 60)])),
        // A run shorter than a comparison reads tells nothing either.
        ("quick", medians(&[("check", 2)])),
        ("short", medians(&[("check", 4)])),
    ];
    let speeds = speeds_of(&machines);
    assert_eq!(speeds[2], ("other".into(), None));
    assert_eq!(speeds[3], ("quick".into(), None));
}

#[test]
fn a_machine_compares_with_each_machine_it_shares_keys_with() {
    let machines = [
        ("fast", medians(&[("build", 100), ("test", 100)])),
        ("middle", medians(&[("build", 200)])),
        ("slow", medians(&[("test", 400)])),
    ];
    // Twice as fast as one machine and four times as fast as the other
    // averages, on a log scale, to the square root of 2 * 4.
    let speeds = speeds_of(&machines);
    assert_eq!(speeds[0], ("fast".into(), Some(283)));
    assert_eq!(speeds[1], ("middle".into(), Some(50)));
    assert_eq!(speeds[2], ("slow".into(), Some(25)));
}
