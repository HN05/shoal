//! The OSC 7501 sequence of a report on Shoal's own record, beside the root
//! record an agent that reports itself writes.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use shoal_protocol::program_status::{ProgramState, ProgramStatus, Waits};

/// The record's id, and the app it names.
const RECORD: &str = "shoal";
/// The most decoded bytes the protocol takes of a title.
const TITLE_LIMIT: usize = 192;
/// The most decoded bytes the protocol takes of a message.
const MESSAGE_LIMIT: usize = 2048;

/// The sequence that reports `status`. A report replaces its record whole,
/// so each one carries the app and title.
pub(super) fn sequence(status: &ProgramStatus) -> Vec<u8> {
    format!("\x1b]7501;{}\x1b\\", pairs(status).join(":")).into_bytes()
}

fn pairs(status: &ProgramStatus) -> Vec<String> {
    let mut pairs = vec![
        format!("state={}", state_name(status.state)),
        format!("id={RECORD}"),
    ];
    if status.state == ProgramState::Clear {
        return pairs;
    }
    pairs.push(format!("app={RECORD}"));
    pairs.push(format!("title={}", text(&status.title, TITLE_LIMIT)));
    match status.state {
        ProgramState::Blocked { kind } => pairs.push(format!("kind={}", kind_name(kind))),
        ProgramState::Working {
            progress: Some(progress),
        } => pairs.push(format!("progress={}", progress.min(100))),
        _ => {}
    }
    if let Some(message) = &status.message {
        pairs.push(format!("msg={}", text(message, MESSAGE_LIMIT)));
    }
    pairs
}

fn state_name(state: ProgramState) -> &'static str {
    match state {
        ProgramState::Idle => "idle",
        ProgramState::Working { .. } => "working",
        ProgramState::Blocked { .. } => "blocked",
        ProgramState::Done => "done",
        ProgramState::Error => "error",
        ProgramState::Clear => "clear",
    }
}

fn kind_name(kind: Waits) -> &'static str {
    match kind {
        Waits::Permission => "permission",
        Waits::Question => "question",
    }
}

/// `text` as base64, on one line, since a terminal refuses a report whose
/// text holds a control character, and cut to `limit` bytes.
fn text(text: &str, limit: usize) -> String {
    let line: String = (text.chars())
        .map(|char| if char.is_control() { ' ' } else { char })
        .collect();
    let mut end = line.len().min(limit);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    STANDARD.encode(&line[..end])
}
