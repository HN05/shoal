use shoal_ids::space_ids::SpaceId;

use super::*;
use crate::code::Lines;

fn change(path: &str, kind: ChangeKind, added: u32, removed: u32) -> Change {
    let at = CodeAt {
        path: path.into(),
        lines: Some(Lines {
            start: 0,
            end: added,
        }),
        blob: None,
    };
    Change {
        number: 1,
        space: ThreadSpace {
            id: SpaceId::from("space"),
            name: None,
            number: Some(SpaceNumber::from(3)),
        },
        author: ChangeAuthor::Agent,
        kind,
        made_at: crate::time::UnixTime::from_seconds(0),
        turn: None,
        execution: None,
        thread: None,
        commit: None,
        original: at.clone(),
        latest: at,
        state: CodeState::Unchanged,
        added,
        removed,
        size: None,
    }
}

#[test]
fn a_size_counts_the_files_and_lines_of_hunks_only() {
    let changes = [
        change("a.rs", ChangeKind::Text, 30, 2),
        change("a.rs", ChangeKind::Text, 10, 10),
        change("b.rs", ChangeKind::Text, 0, 0),
        change("Cargo.lock", ChangeKind::Generated, 0, 0),
    ];
    let size = ChangeSize::of(&changes);
    assert_eq!(size.to_string(), "2 files changed, +40 −12");
}

#[test]
fn a_change_is_named_in_its_own_space_by_its_number_alone() {
    let change = change("a.rs", ChangeKind::Text, 1, 0);
    assert_eq!(change.label(Some(SpaceNumber::from(3))), "1");
    assert_eq!(change.label(Some(SpaceNumber::from(4))), "3.1");
    let own: ChangeNumber = "12".parse().unwrap();
    let resolved = own.resolve(Some(SpaceNumber::from(5))).unwrap();
    assert_eq!(resolved, (SpaceNumber::from(5), 12));
    assert!(own.resolve(None).is_err());
    let full: ChangeNumber = "3.12".parse().unwrap();
    assert_eq!(full.resolve(None).unwrap(), (SpaceNumber::from(3), 12));
}
