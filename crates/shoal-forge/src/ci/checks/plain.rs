//! Job logs as plain text, without what forges and runners write for a
//! terminal to draw.
use std::{iter::Peekable, str::Chars};

/// `log` without terminal escape sequences or other control characters, each
/// line keeping only its last text a carriage return redrew it with.
pub(crate) fn readable(log: &str) -> String {
    without_escapes(log)
        .split_inclusive('\n')
        .map(redrawn)
        .collect()
}

fn without_escapes(log: &str) -> String {
    let mut plain = String::with_capacity(log.len());
    let mut chars = log.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => skip_escape(&mut chars),
            '\u{9b}' => skip_control_sequence(&mut chars),
            '\n' | '\r' | '\t' => plain.push(c),
            c if c.is_control() => {}
            c => plain.push(c),
        }
    }
    plain
}

/// Skip an escape sequence after its ESC, ending a string sequence at the end
/// of its line when nothing terminates it.
fn skip_escape(chars: &mut Peekable<Chars>) {
    match chars.next_if(|c| matches!(c, ' '..='~')) {
        Some('[') => skip_control_sequence(chars),
        Some(']' | 'P' | 'X' | '^' | '_') => {
            while let Some(c) = chars.next_if(|c| *c != '\n') {
                if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                    break;
                }
            }
        }
        Some(' '..='/') => {
            while chars.next_if(|c| matches!(c, ' '..='/')).is_some() {}
            chars.next_if(|c| matches!(c, '0'..='~'));
        }
        _ => {}
    }
}

/// Skip a control sequence's parameters, intermediates and final byte.
fn skip_control_sequence(chars: &mut Peekable<Chars>) {
    while chars.next_if(|c| matches!(c, ' '..='?')).is_some() {}
    chars.next_if(|c| matches!(c, '@'..='~'));
}

/// The last nonempty text of `line` after the carriage returns that redrew it,
/// keeping its line feed.
fn redrawn(line: &str) -> String {
    let (text, end) = match line.strip_suffix('\n') {
        Some(text) => (text, "\n"),
        None => (line, ""),
    };
    let last = text
        .rsplit('\r')
        .find(|part| !part.is_empty())
        .unwrap_or("");
    format!("{last}{end}")
}

/// `log` without the timestamp GitHub and Forgejo start each line with,
/// `2024-05-01T12:00:00.1234567Z `.
pub(crate) fn without_timestamps(log: &str) -> String {
    log.split_inclusive('\n')
        .map(|line| {
            let end = line.find([' ', '\n']).unwrap_or(line.len());
            if is_timestamp(&line[..end]) {
                line[end..].strip_prefix(' ').unwrap_or(&line[end..])
            } else {
                line
            }
        })
        .collect()
}

/// Whether `word` is a UTC time to at least the second.
fn is_timestamp(word: &str) -> bool {
    let Some(seconds) = word.strip_suffix('Z').and_then(|time| time.get(..19)) else {
        return false;
    };
    let fraction = &word[19..word.len() - 1];
    let fields_match = seconds.bytes().enumerate().all(|(i, b)| match i {
        4 | 7 => b == b'-',
        10 => b == b'T',
        13 | 16 => b == b':',
        _ => b.is_ascii_digit(),
    });
    let fraction_matches = fraction.is_empty()
        || fraction
            .strip_prefix('.')
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()));
    fields_match && fraction_matches
}

/// What starts the marker GitLab writes at each end of a collapsible section,
/// `\e[0Ksection_start:1700000000:name[collapsed=true]\r\e[0K`.
const SECTION_MARKER: &str = "\x1b[0Ksection_";

/// `log` without GitLab's section markers, or the lines that held nothing else.
pub(crate) fn without_sections(log: &str) -> String {
    log.split_inclusive('\n')
        .filter_map(|line| {
            let plain = without_markers(line);
            let held_text = !plain.trim_end_matches(['\r', '\n']).is_empty();
            (held_text || plain.len() == line.len()).then_some(plain)
        })
        .collect()
}

fn without_markers(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find(SECTION_MARKER) {
        plain.push_str(&rest[..start]);
        let marker = &rest[start + SECTION_MARKER.len()..];
        match marker_len(marker) {
            Some(len) => rest = &marker[len..],
            None => {
                plain.push_str(SECTION_MARKER);
                rest = marker;
            }
        }
    }
    plain.push_str(rest);
    plain
}

/// The length of a section marker after its `section_`, through the escape
/// that follows its carriage return.
fn marker_len(marker: &str) -> Option<usize> {
    let fields = marker
        .strip_prefix("start:")
        .or_else(|| marker.strip_prefix("end:"))?;
    let (time, name) = fields.split_once(':')?;
    if time.is_empty() || !time.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (_, after) = name.split_once('\r')?;
    let after = after.strip_prefix("\x1b[0K").unwrap_or(after);
    Some(marker.len() - after.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::{Backend, ForgeKind};

    #[test]
    fn escapes_controls_and_redrawn_text_are_removed() {
        let log = concat!(
            "\x1b[1m\x1b[92m   Compiling\x1b[0m shoal v0.1.0\r\n",
            "\x1b]8;;https://example.com\x07link\x1b]8;;\x1b\\ text\x1b(B\x1b=\n",
            "Receiving objects:  50%\rReceiving objects: 100%\r\n",
            "bell\x07 and\u{9b}31m C1\n",
            "\tindented\x1b]unterminated\nnext\x1b",
        );
        assert_eq!(
            readable(log),
            concat!(
                "   Compiling shoal v0.1.0\n",
                "link text\n",
                "Receiving objects: 100%\n",
                "bell and C1\n",
                "\tindented\nnext",
            )
        );
    }

    #[test]
    fn github_and_forgejo_logs_lose_their_timestamps() {
        let log = concat!(
            "2024-05-01T12:00:00.1234567Z ##[group]Run cargo test\n",
            "2024-05-01T12:00:01.0000000Z \x1b[36;1mcargo test\x1b[0m\n",
            "2024-05-01T12:00:02.0000000Z \n",
            "2024-05-01T12:00:03Z test result: FAILED\r\n",
            "2024-05-01T12:00:04.5Z\n",
            "2024-05-01 12:00:05 not a timestamp\n",
            "2024-05-01T12:00:06.Z neither\n",
            "⭐ Run Main Test",
        );
        let plain = concat!(
            "##[group]Run cargo test\n",
            "cargo test\n",
            "\n",
            "test result: FAILED\n",
            "\n",
            "2024-05-01 12:00:05 not a timestamp\n",
            "2024-05-01T12:00:06.Z neither\n",
            "⭐ Run Main Test",
        );
        assert_eq!(ForgeKind::GitHub.plain_log(log), plain);
        assert_eq!(ForgeKind::Forgejo.plain_log(log), plain);
    }

    #[test]
    fn gitlab_traces_lose_their_section_markers() {
        let trace = concat!(
            "\x1b[0KRunning with gitlab-runner 17.0.0\x1b[0;m\n",
            "\x1b[0Ksection_start:1700000000:prepare_script[collapsed=true]\r",
            "\x1b[0K\x1b[0K\x1b[36;1mPreparing environment\x1b[0;m\x1b[0;m\n",
            "Running on runner-1\n",
            "\x1b[0Ksection_end:1700000001:prepare_script\r\x1b[0K\n",
            "\x1b[0Ksection_end:1700000002:step_script\r\x1b[0K",
            "\x1b[0Ksection_start:1700000002:after_script\r\x1b[0K",
            "\x1b[32;1m$ cargo test\x1b[0;m\n",
            "output\x1b[0Ksection_end:1700000003:after_script\r\x1b[0K\n",
            "\n",
            "\x1b[0Ksection_end:x:not_a_marker\r\x1b[0K\n",
            "\x1b[31;1mERROR: Job failed: exit code 101\n\x1b[0;m\n",
        );
        assert_eq!(
            ForgeKind::GitLab.plain_log(trace),
            concat!(
                "Running with gitlab-runner 17.0.0\n",
                "Preparing environment\n",
                "Running on runner-1\n",
                "$ cargo test\n",
                "output\n",
                "\n",
                "section_end:x:not_a_marker\n",
                "ERROR: Job failed: exit code 101\n",
                "\n",
            )
        );
    }
}
