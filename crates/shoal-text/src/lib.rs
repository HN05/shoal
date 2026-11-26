//! Cutting text to a length: in bytes for protocol and storage limits, or in
//! terminal columns for what a terminal shows. Every marked cut ends what it
//! keeps with [`MARK`], after trimming the whitespace before it.
use std::borrow::Cow;

use unicode_width::UnicodeWidthChar;

/// What stands for the text a cut leaves out.
pub const MARK: char = '…';

/// `text` cut to at most `max` bytes, ending with [`MARK`] when cut. A
/// limit too small for the mark gets the unmarked [`prefix_bytes`].
pub fn cut_bytes(text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    match max.checked_sub(MARK.len_utf8()) {
        Some(room) => marked(prefix_bytes(&text, room)),
        None => prefix_bytes(&text, max).to_owned(),
    }
}

/// `text`'s start within `max` bytes, cut at a character boundary without a
/// mark, for text whose reader must find no character it did not write.
pub fn prefix_bytes(text: &str, max: usize) -> &str {
    &text[..text.floor_char_boundary(max)]
}

/// `text` within `max` bytes, its middle replaced by [`MARK`] when it is
/// longer, so that both its start and its end stay.
pub fn cut_middle(text: &str, max: usize) -> Cow<'_, str> {
    if text.len() <= max {
        return text.into();
    }
    let kept = max.saturating_sub(MARK.len_utf8()) / 2;
    let start = &text[..text.floor_char_boundary(kept)];
    let end = &text[text.ceil_char_boundary(text.len() - kept)..];
    format!("{}{}", marked(start), end.trim_start()).into()
}

/// How many terminal columns `text` takes: two for a wide character, such
/// as an emoji or a CJK ideograph, none for a control character or a
/// combining mark.
pub fn width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// `text` cut to at most `columns` terminal columns, ending with [`MARK`]
/// when cut.
pub fn cut_width(text: String, columns: usize) -> String {
    if width(&text) <= columns {
        return text;
    }
    let Some(room) = columns.checked_sub(1) else {
        return String::new();
    };
    let mut used = 0;
    let end = (text.char_indices())
        .find(|(_, c)| {
            used += char_width(*c);
            used > room
        })
        .map_or(text.len(), |(end, _)| end);
    marked(&text[..end])
}

fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

fn marked(kept: &str) -> String {
    format!("{}{MARK}", kept.trim_end())
}

#[cfg(test)]
mod tests;
