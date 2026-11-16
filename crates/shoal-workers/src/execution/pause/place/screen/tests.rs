use super::*;

fn screen() -> Screen {
    Screen::new(&Protection {
        reason: "idle until an update arrives".into(),
        resumes_when: "an update arrives for it".into(),
    })
}

fn rows(screen: &Screen) -> Vec<String> {
    let text = screen.text();
    assert!(text.ends_with("\r\n"), "{text}");
    text.split_terminator("\r\n").map(str::to_owned).collect()
}

#[test]
fn the_screen_shows_the_stop_and_the_keys() {
    let rows = rows(&screen());
    assert!(
        rows.iter()
            .any(|row| row.starts_with("Reason:") && row.ends_with(" idle until an update arrives")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.starts_with("Resumes when:")
                && row.ends_with(" an update arrives for it")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row.starts_with("Enter ")), "{rows:?}");
    assert!(
        rows.iter().any(|row| row.starts_with("Ctrl-C ")),
        "{rows:?}"
    );
}

#[test]
fn notes_and_the_wake_show_above_the_keys() {
    let mut screen = screen();
    screen
        .notes
        .push("the restore also waits for pid 42".into());
    screen.wake = Some(Wake::Refused("approval pending".into()));
    let refused = rows(&screen);
    let row = |text: &str| refused.iter().position(|row| row.contains(text)).unwrap();
    let keys = row("Enter ");
    assert!(row("pid 42") < keys, "{refused:?}");
    assert!(row("approval pending") < keys, "{refused:?}");
    screen.wake = Some(Wake::Waking);
    let waking = rows(&screen);
    assert!(
        !waking.iter().any(|row| row.contains("approval pending")),
        "{waking:?}"
    );
}
