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

fn shows(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

#[test]
fn the_screen_shows_why_the_agent_sleeps_and_when_it_resumes() {
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
}

#[test]
fn the_screen_shows_its_notes_and_only_the_latest_wake() {
    let mut screen = screen();
    screen
        .notes
        .push("the restore also waits for pid 42".into());
    screen.wake = Some(Wake::Unsent("the daemon is away".into()));
    let unsent = rows(&screen);
    assert!(shows(&unsent, "pid 42"), "{unsent:?}");
    assert!(shows(&unsent, "the daemon is away"), "{unsent:?}");
    screen.wake = Some(Wake::Waking);
    let waking = rows(&screen);
    assert!(shows(&waking, "pid 42"), "{waking:?}");
    assert!(!shows(&waking, "the daemon is away"), "{waking:?}");
}
