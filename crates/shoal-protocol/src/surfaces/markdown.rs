//! The images a surface's Markdown shows, found as a CommonMark renderer
//! finds them, which the image check and the text renderer read.
use std::ops::Range;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

/// An image of Markdown text: the text that writes it, what it shows and
/// what it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Image<'a> {
    pub(super) source: &'a str,
    pub(super) alt: String,
    pub(super) target: String,
}

/// The images `text` shows, in order. An image inside another's text
/// counts toward that text alone, as a renderer shows it.
pub(super) fn images(text: &str) -> Vec<Image<'_>> {
    let mut images = Vec::new();
    let mut open: Option<(Range<usize>, String, String)> = None;
    let mut depth = 0;
    for (event, range) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                depth += 1;
                open.get_or_insert_with(|| (range, dest_url.into_string(), String::new()));
            }
            Event::End(TagEnd::Image) => {
                depth -= 1;
                if depth == 0
                    && let Some((range, target, alt)) = open.take()
                {
                    images.push(Image {
                        source: &text[range],
                        alt,
                        target,
                    });
                }
            }
            Event::Text(shown) | Event::Code(shown) => {
                if let Some((_, _, alt)) = open.as_mut() {
                    alt.push_str(&shown);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, _, alt)) = open.as_mut() {
                    alt.push(' ');
                }
            }
            _ => {}
        }
    }
    images
}
