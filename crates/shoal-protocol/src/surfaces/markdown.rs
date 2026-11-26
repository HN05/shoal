//! The images a surface's Markdown shows, inline as `![alt](target)` or by
//! reference to a `[label]: target` definition, which the image check and
//! the text renderer read.
use std::collections::BTreeMap;

/// An image of Markdown text: the text that writes it, what it shows and
/// what it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Image<'a> {
    pub(super) source: &'a str,
    pub(super) alt: &'a str,
    pub(super) target: &'a str,
}

/// The images `text` shows, in order. A reference to a label `text` does
/// not define is no image.
pub(super) fn images(text: &str) -> Vec<Image<'_>> {
    let defined = definitions(text);
    let mut images = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("![") {
        let start = from + found;
        let Some(image) = image_at(text, start, &defined) else {
            from = start + 2;
            continue;
        };
        from = start + image.source.len();
        images.push(image);
    }
    images
}

/// The image whose `![` starts at `start` in `text`, inline or by a
/// reference to a label of `defined`.
fn image_at<'a>(
    text: &'a str,
    start: usize,
    defined: &BTreeMap<String, &'a str>,
) -> Option<Image<'a>> {
    let alt_start = start + 2;
    let alt_end = closing(text, alt_start, '[', ']')?;
    let alt = &text[alt_start..alt_end];
    let after = alt_end + 1;
    let (target, end) = match text[after..].chars().next() {
        Some('(') => {
            let close = closing(text, after + 1, '(', ')')?;
            let target = text[after + 1..close].split_whitespace().next();
            (unbracketed(target.unwrap_or_default()), close + 1)
        }
        Some('[') => {
            let close = closing(text, after + 1, '[', ']')?;
            let label = match &text[after + 1..close] {
                "" => alt,
                label => label,
            };
            (*defined.get(&label.to_lowercase())?, close + 1)
        }
        _ => (*defined.get(&alt.to_lowercase())?, after),
    };
    Some(Image {
        source: &text[start..end],
        alt,
        target,
    })
}

/// Where the `close` that ends what opens just before `from` in `text`
/// is: past any nested `open` and `close` it pairs, and any of them a
/// backslash escapes.
fn closing(text: &str, from: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0;
    let mut escaped = false;
    for (at, character) in text[from..].char_indices() {
        match character {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            c if c == open => depth += 1,
            c if c == close && depth == 0 => return Some(from + at),
            c if c == close => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The targets of the reference definitions `[label]: target` in `text`,
/// by their labels, which match without regard to case.
fn definitions(text: &str) -> BTreeMap<String, &str> {
    let mut defined = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix('[') else {
            continue;
        };
        let Some((label, target)) = rest.split_once("]:") else {
            continue;
        };
        if let Some(target) = target.split_whitespace().next() {
            defined
                .entry(label.to_lowercase())
                .or_insert(unbracketed(target));
        }
    }
    defined
}

/// `target` without the angle brackets Markdown allows around it.
fn unbracketed(target: &str) -> &str {
    (target
        .strip_prefix('<')
        .and_then(|target| target.strip_suffix('>')))
    .unwrap_or(target)
}
