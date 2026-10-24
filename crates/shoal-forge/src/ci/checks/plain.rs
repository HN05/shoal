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

#[cfg(test)]
mod tests {
    use super::*;

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
}
