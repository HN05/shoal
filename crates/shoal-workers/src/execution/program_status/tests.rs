use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use shoal_protocol::program_status::Waits;
use std::{
    io::{Read as _, Write as _},
    os::fd::FromRawFd as _,
};

fn status(state: ProgramState, message: Option<&str>) -> ProgramStatus {
    ProgramStatus {
        state,
        title: "Fix login".into(),
        message: message.map(Into::into),
        turn: false,
    }
}

/// The pairs of `sequence`, each base64 text decoded.
fn decoded(sequence: &[u8]) -> Vec<(String, String)> {
    let text = std::str::from_utf8(sequence).unwrap();
    let body = (text.strip_prefix("\x1b]7501;"))
        .and_then(|body| body.strip_suffix("\x1b\\"))
        .unwrap_or_else(|| panic!("not a report: {text:?}"));
    (body.split(':'))
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap();
            let value = match key {
                "title" | "msg" => String::from_utf8(STANDARD.decode(value).unwrap()).unwrap(),
                _ => value.to_owned(),
            };
            (key.to_owned(), value)
        })
        .collect()
}

fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    (pairs.iter())
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[test]
fn a_report_names_shoals_record_with_its_title_and_kind() {
    let blocked = status(
        ProgramState::Blocked {
            kind: Waits::Question,
        },
        Some("3.1: Which\ndatabase?"),
    );
    assert_eq!(
        decoded(&sequence::sequence(&blocked)),
        pairs(&[
            ("state", "blocked"),
            ("id", "shoal"),
            ("app", "shoal"),
            ("title", "Fix login"),
            ("kind", "question"),
            ("msg", "3.1: Which database?"),
        ])
    );
    let working = status(ProgramState::Working { progress: Some(40) }, None);
    assert_eq!(
        decoded(&sequence::sequence(&working)),
        pairs(&[
            ("state", "working"),
            ("id", "shoal"),
            ("app", "shoal"),
            ("title", "Fix login"),
            ("progress", "40"),
        ])
    );
    assert_eq!(
        sequence::sequence(&status(ProgramState::Clear, None)),
        b"\x1b]7501;state=clear:id=shoal\x1b\\"
    );
}

#[test]
fn a_long_message_is_cut_on_a_character_boundary() {
    let long = "é".repeat(1500);
    let report = decoded(&sequence::sequence(&status(
        ProgramState::Done,
        Some(&long),
    )));
    let (_, message) = report.iter().find(|(key, _)| key == "msg").unwrap();
    assert_eq!(message.len(), 2048);
    assert!(long.starts_with(message.as_str()));
}

#[test]
fn a_failed_exit_reports_its_code_or_signal() {
    let done = status(ProgramState::Done, Some("Ready for review"));
    let failed = exit_report(&done, 2).unwrap();
    assert_eq!(failed.state, ProgramState::Error);
    assert!(failed.message.unwrap().contains("code 2"));
    let killed = exit_report(&done, 128 + libc::SIGKILL).unwrap();
    assert!(killed.message.unwrap().contains("signal 9"));
}

#[test]
fn a_clean_exit_clears_the_turn_and_keeps_shoals_own_report() {
    let turn = ProgramStatus {
        turn: true,
        ..status(ProgramState::Working { progress: None }, None)
    };
    assert_eq!(exit_report(&turn, 0).unwrap().state, ProgramState::Clear);
    let done = status(ProgramState::Done, Some("Ready for review"));
    assert_eq!(exit_report(&done, 0), None);
}

/// A terminal for `reads_protocol` to query, and the side that answers.
fn terminal() -> (File, File) {
    let (mut controller, mut device) = (0, 0);
    // SAFETY: openpty fills both descriptors on success.
    let opened = unsafe {
        libc::openpty(
            &mut controller,
            &mut device,
            std::ptr::null_mut(),
            // macOS takes these as mutable, Linux as constant.
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0, "{}", std::io::Error::last_os_error());
    // SAFETY: both descriptors were just opened and are owned here alone.
    unsafe { (File::from_raw_fd(device), File::from_raw_fd(controller)) }
}

/// Query a terminal that answers the query with `replies`, written once
/// the query arrived.
fn queried(replies: &[&[u8]]) -> bool {
    let (device, mut controller) = terminal();
    let querying = std::thread::spawn(move || {
        // A hang guard: every reply ends with the attributes reply.
        query::reads_protocol(&device, Duration::from_secs(60))
    });
    let mut query = [0; 64];
    let mut read = 0;
    while !query[..read].ends_with(b"\x1b[c") {
        read += controller.read(&mut query[read..]).unwrap();
    }
    for reply in replies {
        controller.write_all(reply).unwrap();
    }
    querying.join().unwrap()
}

#[test]
fn a_terminal_that_answers_the_query_reads_the_protocol() {
    assert!(queried(&[
        b"\x1b]7501;?:states=idle,working\x1b\\",
        b"\x1b[?62;22c"
    ]));
}

#[test]
fn a_terminal_that_answers_only_the_attributes_request_does_not() {
    assert!(!queried(&[b"typed", b"\x1b[?1;2c"]));
}

#[test]
fn a_terminal_that_answers_neither_request_does_not() {
    let (device, _controller) = terminal();
    assert!(!query::reads_protocol(&device, Duration::ZERO));
}

#[tokio::test]
async fn the_writer_writes_the_latest_report_and_says_when() {
    let (device, mut controller) = terminal();
    let writer = writer::Writer::start(device);
    writer.queue(b"first".to_vec());
    let latest = writer.queue(b"latest".to_vec());
    tokio::time::timeout(Duration::from_secs(60), latest.wait())
        .await
        .expect("the wait has its own limit");
    let mut read = Vec::new();
    while !read.ends_with(b"latest") {
        let mut chunk = [0; 64];
        let count = controller.read(&mut chunk).unwrap();
        read.extend_from_slice(&chunk[..count]);
    }
}
