//! Choosing one of the issues and PRs a space links, to view it or open it
//! in the browser.
use anyhow::Result;

use super::{Key, KeyBindings, WidthLimit, aligned_to, pick_with_keys, picker_width};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
    },
    forge::items::link,
    model::LinkedItem,
};

/// The URL of one of `items` and whether to open it in the browser, which
/// only a caller that may `browse` is offered.
pub fn pick_linked(ctx: &Context, items: &[LinkedItem], browse: bool) -> Result<(String, bool)> {
    let entries = items
        .iter()
        .map(|item| item.url.clone())
        .zip(linked_labels(items))
        .collect();
    let bindings: &[_] = &[
        (Key::Enter, "view", false),
        (Key::Ctrl('o'), "open in browser", true),
    ];
    let bindings = if browse { bindings } else { &bindings[..1] };
    let picked = pick_with_keys(ctx, "View> ", entries, KeyBindings(bindings))?;
    Ok((picked.id, picked.action))
}

/// One aligned row per item: its kind, number, title, which Shoal does not
/// record for every item, and URL.
fn linked_labels(items: &[LinkedItem]) -> Vec<String> {
    let cells = items
        .iter()
        .map(|item| {
            let kind = item.kind.title();
            let number = link::item(item.kind, &item.url)
                .map(|(_, number)| format!("#{number}"))
                .unwrap_or_default();
            [
                (kind.to_owned(), Some(Style::Heading)),
                (number, None),
                (item.title.clone().unwrap_or_default(), None),
                (item.url.clone(), Some(Style::Muted)),
            ]
        })
        .collect();
    let limit = picker_width().map(|width| WidthLimit { column: 2, width });
    aligned_to(cells, Palette::picker(), limit)
}
