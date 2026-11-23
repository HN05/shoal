use shoal_protocol::launches::{Launch, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;

use crate::daemon::launches::placement::placeable;

#[test]
fn only_a_detached_start_that_needs_nothing_of_this_machine_is_placed() {
    assert!(placeable(&Launch::default()));
    let unplaced = [
        Launch {
            here: true,
            ..Launch::default()
        },
        Launch {
            machine: Some("desk".into()),
            ..Launch::default()
        },
        Launch {
            in_workspace: Some(SpaceChoice::Current),
            ..Launch::default()
        },
        Launch {
            copy: Some(SpaceChoice::Space(SpaceSelector::id(&"s1".into()))),
            ..Launch::default()
        },
        Launch {
            blockers: vec!["7".into()],
            ..Launch::default()
        },
    ];
    for launch in unplaced {
        assert!(!placeable(&launch), "{launch:#?}");
    }
}
