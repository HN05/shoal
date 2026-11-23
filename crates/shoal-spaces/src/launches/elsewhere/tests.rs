use shoal_protocol::launches::{Launch, SpaceChoice};

use crate::launches::elsewhere::ensure_carried;

#[test]
fn a_start_on_another_machine_refuses_what_only_this_machine_has() {
    assert!(ensure_carried(&Launch::default(), "laptop").is_ok());
    let refused = |launch: Launch| format!("{:#}", ensure_carried(&launch, "laptop").unwrap_err());
    let here = refused(Launch {
        here: true,
        ..Default::default()
    });
    assert!(here.contains("--here"), "{here}");
    let existing = refused(Launch {
        in_workspace: Some(SpaceChoice::Current),
        ..Default::default()
    });
    assert!(existing.contains("--in"), "{existing}");
}
