use super::*;

#[test]
fn view_cycle_wraps_and_unknown_prompts_start_at_the_first_view() -> Result<()> {
    let views = ["inbox> ", "spaces> ", "notifications> "];
    for (shown, expected) in [
        ("inbox> ", "spaces> "),
        ("spaces> ", "notifications> "),
        ("notifications> ", "inbox> "),
        ("", "inbox> "),
        ("unknown> ", "inbox> "),
    ] {
        assert_eq!(*view_after(&views, shown, |view| *view)?, expected);
    }
    assert_eq!(
        *view_after(&["spaces> "], "spaces> ", |view| *view)?,
        "spaces> "
    );
    assert!(view_after::<&str>(&[], "", |view| *view).is_err());
    Ok(())
}

#[test]
fn initial_and_reloaded_views_disable_the_same_unavailable_actions() {
    let picker = ViewPicker {
        view: PickerView {
            prompt: "notifications> ",
            header: "enter: cd\nctrl-o: status".into(),
            keys: vec![Key::Enter, Key::Ctrl('o')],
        },
        keys: &[Key::Enter, Key::Ctrl('d'), Key::Ctrl('o')],
        next: Some("shoal internal menu-next".into()),
        query: Some("'current".into()),
    };
    assert_eq!(
        picker.arguments(),
        [
            "--header",
            "enter: cd\nctrl-o: status",
            "--bind",
            "enter:print(enter)+accept",
            "--bind",
            "ctrl-d:print(ctrl-d)+accept",
            "--bind",
            "ctrl-o:print(ctrl-o)+accept",
            "--bind",
            "start:unbind(ctrl-d)",
            "--bind",
            "tab:transform(shoal internal menu-next)",
            "--query",
            "'current",
        ]
    );
    let reload = "'shoal path' internal menu-rows notifications + (literal)";
    assert_eq!(
        picker.view.transition(picker.keys, reload),
        format!(
            "unbind(ctrl-d)+rebind(enter,ctrl-o)+change-prompt(notifications> )+change-header(enter: cd\nctrl-o: status)+reload:{reload}"
        )
    );
}

#[test]
fn a_single_view_with_every_action_needs_no_unbinding_or_tab() {
    let picker = ViewPicker {
        view: PickerView {
            prompt: "spaces> ",
            header: "enter: cd".into(),
            keys: vec![Key::Enter],
        },
        keys: &[Key::Enter],
        next: None,
        query: None,
    };
    assert_eq!(
        picker.arguments(),
        [
            "--header",
            "enter: cd",
            "--bind",
            "enter:print(enter)+accept"
        ]
    );
    assert_eq!(
        picker.view.transition(picker.keys, "reload rows"),
        "rebind(enter)+change-prompt(spaces> )+change-header(enter: cd)+reload:reload rows"
    );
}

#[test]
fn headers_name_tab_after_enter_only_when_the_view_switches() {
    let keys = [(Key::Enter, "cd"), (Key::Ctrl('o'), "status")];
    assert_eq!(
        hints(&keys, true),
        ["enter: cd", "tab: next view", "ctrl-o: status"]
    );
    assert_eq!(hints(&keys, false), ["enter: cd", "ctrl-o: status"]);
}
