//! Whether a PR's description closes an issue when the PR merges, so an
//! agent that fixes its workspace's issue hears when merging would leave it
//! open.
use crate::{
    conventions::{edit, name},
    items::item::Item,
};

/// The keywords every forge reads as closing the issue a reference names.
const KEYWORDS: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// The warning for a PR whose `body` does not close issue `number` at `url`;
/// a PR that leaves part of the issue for later has reason to ignore it.
pub fn warning(item: &Item, body: &str, number: u64, url: &str) -> Option<String> {
    if closes(body, number, url) {
        return None;
    }
    Some(format!(
        "the description of {} does not close issue #{number}, so merging it leaves the issue \
         open; {} adds `Closes #{number}` unless the PR leaves part of the issue for later",
        name(item),
        edit(item, "--body-file")
    ))
}

/// Whether a closing keyword precedes `#number` or `url` in `body`.
fn closes(body: &str, number: u64, url: &str) -> bool {
    let short = format!("#{number}");
    references(body, &short)
        .chain(references(body, url))
        .any(|start| follows_keyword(&body[..start]))
}

/// Where `reference` starts in `body` as a whole reference rather than the
/// start of a longer number or path.
fn references<'a>(body: &'a str, reference: &'a str) -> impl Iterator<Item = usize> + 'a {
    body.match_indices(reference)
        .map(|(start, _)| start)
        .filter(move |start| {
            !body[start + reference.len()..]
                .starts_with(|next: char| next.is_ascii_alphanumeric() || next == '/')
        })
}

/// Whether `before` ends with a closing keyword, then an optional colon.
fn follows_keyword(before: &str) -> bool {
    let before = before.trim_end();
    let before = before.strip_suffix(':').unwrap_or(before).trim_end();
    let word = &before[before
        .trim_end_matches(|letter: char| letter.is_ascii_alphabetic())
        .len()..];
    KEYWORDS
        .iter()
        .any(|keyword| word.eq_ignore_ascii_case(keyword))
}

#[cfg(test)]
mod tests;
