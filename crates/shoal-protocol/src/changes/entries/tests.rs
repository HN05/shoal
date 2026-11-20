use super::*;
use crate::changes::{ChangeSize, tests::change};

fn made(number: u32, turn: i64, seconds: i64, path: &str, added: u32) -> Change {
    Change {
        number,
        turn: Some(turn),
        made_at: crate::time::UnixTime::from_seconds(seconds),
        ..change(path, ChangeKind::Text, added, 1)
    }
}

/// An entry's numbers, and each file it names with its lines added and
/// removed.
type Summary<'a> = (String, Vec<(&'a str, u32, u32)>);

fn summary(entries: &[ChangeEntry]) -> Vec<Summary<'_>> {
    (entries.iter())
        .map(|entry| {
            let files = (entry.files.iter())
                .map(|file| (file.path.as_str(), file.added, file.removed))
                .collect();
            (entry.numbers_text(), files)
        })
        .collect()
}

#[test]
fn a_thread_s_entries_are_its_runs_of_changes_of_one_turn_with_the_size_of_each_file() {
    let changes = [
        made(1, 4, 10, "a.rs", 2),
        made(2, 4, 10, "a.rs", 3),
        made(3, 5, 10, "b.rs", 1),
    ];
    let entries = entries(changes.iter().map(Counted::from), &[]);
    assert_eq!(
        summary(&entries),
        [
            ("1–2".to_owned(), vec![("a.rs", 5, 2)]),
            ("3".to_owned(), vec![("b.rs", 1, 1)]),
        ]
    );
    assert_eq!(ChangeSize::of_entries(&entries), ChangeSize::of(&changes));
}

#[test]
fn a_post_made_during_a_turn_starts_a_new_entry() {
    let changes = [
        made(1, 4, 10, "a.rs", 1),
        made(2, 4, 20, "a.rs", 1),
        made(3, 4, 30, "a.rs", 1),
    ];
    let post = [crate::time::UnixTime::from_seconds(15)];
    let numbers: Vec<String> = (entries(changes.iter().map(Counted::from), &post).iter())
        .map(ChangeEntry::numbers_text)
        .collect();
    assert_eq!(numbers, ["1", "2–3"]);
}

#[test]
fn a_thread_with_much_work_keeps_its_entries_within_their_budget_and_their_size() {
    // A rewrite of 300 files in each of 40 turns.
    let changes: Vec<Change> = (0..40)
        .flat_map(|turn| {
            (0..300).map(move |file| {
                let number = turn * 300 + file + 1;
                made(
                    number,
                    i64::from(turn),
                    0,
                    &format!("src/module_{file}/lib.rs"),
                    2,
                )
            })
        })
        .collect();
    let entries = entries(changes.iter().map(Counted::from), &[]);
    assert!(serde_json::to_vec(&entries).unwrap().len() <= MAX_ENTRIES_BYTES);
    let size = ChangeSize::of_entries(&entries);
    assert_eq!((size.added, size.removed), (2 * 12_000, 12_000));
    assert_eq!(entries[0].numbers.first().map(|range| range[0]), Some(1));
}
