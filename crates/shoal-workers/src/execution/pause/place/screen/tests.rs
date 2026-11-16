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
    assert!(
        rows.iter().any(|row| row.starts_with("Ctrl-C ")),
        "{rows:?}"
    );
}

#[test]
fn notes_show_above_the_keys() {
    let mut screen = screen();
    screen
        .notes
        .push("the restore also waits for pid 42".into());
    let rows = rows(&screen);
    let row = |text: &str| rows.iter().position(|row| row.contains(text)).unwrap();
    assert!(row("pid 42") < row("Ctrl-C "), "{rows:?}");
}
