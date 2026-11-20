use shoal_forge_types::ci::runner::{JobRun, Trigger};

use super::*;

#[test]
fn only_a_run_on_the_known_default_branch_is_on_it() {
    let run = |trigger| JobRun {
        url: "https://forge.example/team/repo/actions/runs/1"
            .parse()
            .unwrap(),
        workflow: None,
        commit: "abc".into(),
        trigger,
        reference: None,
    };
    let main = run(Trigger::Branch("main".into()));
    assert!(on_branch(&main, Some("main")));
    assert!(!on_branch(&main, None));
    assert!(!on_branch(
        &run(Trigger::Branch("topic".into())),
        Some("main")
    ));
    assert!(!on_branch(&run(Trigger::Other), Some("main")));
}
