use std::path::PathBuf;

use shoal_protocol::launches::{Carried, CarriedCopy, Launch, Setup, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;

use crate::launches::placement::placeable;
use crate::scope::Caller;

/// A launch whose agent runs in the caller's terminal.
fn here() -> Launch {
    Launch {
        here: true,
        ..Launch::default()
    }
}

#[test]
fn the_users_start_is_placed_whether_its_agent_runs_detached_or_in_its_terminal() {
    assert!(placeable(&Launch::default(), None));
    assert!(placeable(&here(), None));
}

#[test]
fn a_start_whose_caller_cannot_attach_elsewhere_stays_here() {
    let own_way = Launch {
        own_way: true,
        ..here()
    };
    assert!(!placeable(&own_way, None));
    let process = Caller::of_space("s1".into());
    assert!(!placeable(&here(), Some(&process)));
    // A space's process starts detached children anywhere.
    assert!(placeable(&Launch::default(), Some(&process)));
}

#[test]
fn a_start_that_needs_this_machine_stays_here_whoever_runs_its_agent() {
    let space = || SpaceChoice::Space(SpaceSelector::id(&"s1".into()));
    let needs_this_machine = [
        Launch {
            machine: Some("desk".into()),
            ..here()
        },
        Launch {
            forwarded: Some(Carried::default()),
            ..here()
        },
        Launch {
            in_workspace: Some(SpaceChoice::Current),
            ..here()
        },
        Launch {
            copy: Some(space()),
            ..here()
        },
        Launch {
            carried_copy: Some(CarriedCopy {
                files: "0123456789abcdef0123456789abcdef01234567".into(),
                name: "copy".into(),
            }),
            ..here()
        },
        Launch {
            fork: Some("session".into()),
            ..here()
        },
        Launch {
            path: Some(PathBuf::from("/tmp/tree")),
            ..here()
        },
        Launch {
            setup: Some(Setup::Repository),
            ..here()
        },
        Launch {
            blockers: vec!["7".into()],
            ..here()
        },
    ];
    for launch in needs_this_machine {
        assert!(!placeable(&launch, None), "{launch:#?}");
        let detached = Launch {
            here: false,
            ..launch
        };
        assert!(!placeable(&detached, None), "{detached:#?}");
    }
}
