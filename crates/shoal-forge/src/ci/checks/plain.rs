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
mod tests;
