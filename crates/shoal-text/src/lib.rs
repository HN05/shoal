//! Cutting text to a length: in bytes for protocol and storage limits, or in
//! terminal columns for what a terminal shows. Cuts keep whole grapheme
//! clusters, so an emoji sequence or an accented letter stays whole or goes.
//! Every marked cut ends what it keeps with [`MARK`], after trimming the
//! whitespace before it.
use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// What stands for the text a cut leaves out.
pub const MARK: char = '…';

/// `text` cut to at most `max` bytes, ending with [`MARK`] when cut.
pub fn cut_bytes(text: String, max: usize) -> String {
    cut(text, usize::MAX, max)
}

/// `text` cut to at most `columns` terminal columns, ending with [`MARK`]
/// when cut.
pub fn cut_width(text: String, columns: usize) -> String {
    cut(text, columns, usize::MAX)
}

/// `text` cut to fit both `columns` terminal columns and `bytes` bytes,
/// ending with [`MARK`] when cut. A limit too small for the mark gets the
/// unmarked start that fits.
pub fn cut(text: String, columns: usize, bytes: usize) -> String {
    if width(&text) <= columns && text.len() <= bytes {
        return text;
    }
    match (columns.checked_sub(1), bytes.checked_sub(MARK.len_utf8())) {
        (Some(columns), Some(bytes)) => marked(prefix(&text, columns, bytes)),
        _ => prefix(&text, columns, bytes).to_owned(),
    }
}

/// `text`'s start within `max` bytes, without a mark, for text whose reader
/// must find no character it did not write.
pub fn prefix_bytes(text: &str, max: usize) -> &str {
    prefix(text, usize::MAX, max)
}

/// `text` within `max` bytes, its middle replaced by [`MARK`] when it is
/// longer, so that both its start and its end stay. The end keeps its line
/// breaks, so cut lines still join as lines.
pub fn cut_middle(text: &str, max: usize) -> Cow<'_, str> {
    if text.len() <= max {
        return text.into();
    }
    let kept = max.saturating_sub(MARK.len_utf8()) / 2;
    let start = prefix_bytes(text, kept);
    let end = suffix_bytes(text, kept).trim_start_matches([' ', '\t']);
    format!("{}{end}", marked(start)).into()
}

/// How many terminal columns `text` takes: two for a wide grapheme, such as
/// an emoji or a CJK ideograph, none for a combining mark on its own.
pub fn width(text: &str) -> usize {
    text.graphemes(true).map(UnicodeWidthStr::width).sum()
}

/// `text` followed by the spaces that make it `columns` wide, or as it is
/// when it is wider, so that the columns after it line up.
pub fn pad(text: &str, columns: usize) -> String {
    let padding = columns.saturating_sub(width(text));
    format!("{text}{}", " ".repeat(padding))
}

/// The longest start of `text` that fits `columns` and `bytes`.
fn prefix(text: &str, columns: usize, bytes: usize) -> &str {
    let mut used = 0;
    let end = (text.grapheme_indices(true))
        .find(|(start, grapheme)| {
            used += grapheme.width();
            used > columns || start + grapheme.len() > bytes
        })
        .map_or(text.len(), |(start, _)| start);
    &text[..end]
}

/// The longest end of `text` within `max` bytes.
fn suffix_bytes(text: &str, max: usize) -> &str {
    let start = (text.grapheme_indices(true).rev())
        .find(|(start, _)| text.len() - start > max)
        .map_or(0, |(start, grapheme)| start + grapheme.len());
    &text[start..]
}

fn marked(kept: &str) -> String {
    format!("{}{MARK}", kept.trim_end())
}

#[cfg(test)]
mod tests;
