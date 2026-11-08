use super::*;

#[test]
fn task_lists_give_steps_and_refuse_other_lines() {
    let steps = task_list("- [x] Reproduce\n\n  * [ ] Fix the timeout  \n- [X] Test\n").unwrap();
    let parsed: Vec<_> = (steps.iter())
        .map(|step| (step.name.as_str(), step.state))
        .collect();
    assert_eq!(
        parsed,
        [
            ("Reproduce", StepState::Done),
            ("Fix the timeout", StepState::Pending),
            ("Test", StepState::Done),
        ]
    );
    assert!(task_list("").unwrap().is_empty());
    let refused = task_list("- [x] Reproduce\nFix it\n").unwrap_err();
    assert!(format!("{refused:#}").starts_with("line 2"));
    assert!(task_list("- [~] Maybe\n").is_err());
    assert!(task_list("- [ ]   \n").is_err());
}

#[test]
fn steps_number_from_one_and_mark_the_current_step() {
    let progress = Progress {
        status: None,
        steps: (1..=10)
            .map(|at| crate::client::progress::Step {
                name: format!("Step {at}"),
                done: at < 3,
            })
            .collect(),
        current: Some(2),
        updated_at: crate::time::UnixTime::EPOCH,
    };
    let lines = step_lines(&progress, Palette::stdout(true));
    assert_eq!(lines[0], " 1. [x] Step 1");
    assert_eq!(lines[2], " 3. [ ] Step 3  (current)");
    assert_eq!(lines[9], "10. [ ] Step 10");
}

#[test]
fn an_invalid_status_line_is_refused_before_anything_changes() {
    assert!(changes(Some("two\nlines".into()), false).is_err());
    let cleared = changes(Some(String::new()), false).unwrap();
    assert_eq!(cleared, [ProgressChange::Status(None)]);
}
