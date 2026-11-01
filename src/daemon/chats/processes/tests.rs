use std::sync::Mutex;

use super::stop_in_rounds;
use crate::{
    process::identity::{self, Identity},
    test_support::ProcessGroup,
};

#[tokio::test]
async fn a_stop_rescans_with_what_it_stopped_until_nothing_is_left() {
    // Each scan reveals the next process, as one a stopped process started
    // while it stopped.
    let first = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let second = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let found: Vec<Identity> = [&first, &second]
        .map(|process| identity::capture(process.id()).unwrap().unwrap())
        .into();
    let rounds = Mutex::new(Vec::new());

    stop_in_rounds(|stopped| {
        let round = {
            let mut rounds = rounds.lock().unwrap();
            rounds.push(
                stopped
                    .iter()
                    .map(|process| process.pid)
                    .collect::<Vec<_>>(),
            );
            rounds.len()
        };
        let found = found.get(round - 1).cloned().into_iter().collect();
        async move { Ok(found) }
    })
    .await
    .unwrap();

    assert!(!identity::alive(&found[0]).unwrap());
    assert!(!identity::alive(&found[1]).unwrap());
    assert_eq!(
        *rounds.lock().unwrap(),
        [vec![], vec![found[0].pid], vec![found[0].pid, found[1].pid]]
    );
}
