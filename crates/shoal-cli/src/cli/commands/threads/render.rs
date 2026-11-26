//! How a thread reads on the terminal.
use shoal_protocol::{
    changes::{ChangeDiff, ChangeEntry, ChangeKind, ChangeSize},
    code::CodeRef,
    threads::{Finding, Post, Side, Thread},
};

use crate::{
    cli::{
        output::{Palette, Style},
        ui::{self, Cell},
    },
    client::output::alerts::hanging_indent,
};

pub(super) fn status(thread: &Thread) -> String {
    let turn = match (&thread.resolution, thread.awaits()) {
        (Some(resolution), _) => return resolution.outcome.describe(),
        (None, None) if thread.held => "pending until its review is submitted".into(),
        (None, None) => "empty".into(),
        (None, Some(side)) => format!("waiting for {}", thread.party(side)),
    };
    match thread.working {
        true => format!("working; {turn}"),
        false => turn,
    }
}

pub(in crate::cli) fn summary(thread: &Thread) -> String {
    let mut line = format!("{}  {}: {}", thread.number, thread.space, thread.one_line());
    if let Some(post) = thread.last_post() {
        let text = post.post.text.replace(['\n', '\t'], " ");
        line.push_str(&format!("  {}: {text}", post.author()));
    }
    line
}

pub(super) fn row(thread: &Thread, palette: Palette) -> String {
    let mut lines = vec![format!(
        "{} {} {}: {}",
        palette.paint(Style::Heading, thread.number),
        palette.paint(Style::Muted, format_args!("[{}]", status(thread))),
        thread.space,
        thread.one_line()
    )];
    if let Some(post) = thread.last_post() {
        lines.push(format!(
            "  {}: {}",
            post.author(),
            hanging_indent(&post.post.text, "    ")
        ));
    }
    lines.join("\n")
}

fn kind(thread: &Thread) -> String {
    let Some(Finding { severity, review }) = thread.finding else {
        return thread.kind.to_string();
    };
    let review = review.map(|review| match thread.held {
        true => format!("pending review {review}"),
        false => format!("review {review}"),
    });
    let about: Vec<String> = (severity.map(|severity| severity.to_string()).into_iter())
        .chain(review)
        .collect();
    match about.is_empty() {
        true => thread.kind.to_string(),
        false => format!("{} ({})", thread.kind, about.join(", ")),
    }
}

pub(super) fn details(thread: &Thread, palette: Palette) -> String {
    let mut rows: Vec<[Cell; 2]> = Vec::new();
    let mut row = |label: &str, value: String| {
        rows.push([(label.to_owned(), Some(Style::Muted)), (value, None)]);
    };
    row("Status", status(thread));
    row("Kind", kind(thread));
    row("Opened by", thread.party(Side::Opener));
    row("Held by", thread.party(Side::Holder));
    let heading = format!("Thread {}: {}", thread.number, thread.one_line());
    let mut lines = vec![palette.paint(Style::Heading, heading)];
    lines.extend(
        ui::aligned(rows, palette)
            .into_iter()
            .map(|line| format!("  {line}")),
    );
    let mut entries = thread.changes.iter().peekable();
    let mut number = 0;
    for post in &thread.posts {
        while let Some(entry) = entries.next_if(|entry| entry.at < post.created_at) {
            lines.push(String::new());
            lines.extend(entry_lines(entry, palette));
        }
        number += u32::from(post.speaks());
        lines.push(String::new());
        lines.extend(post_lines(thread, number, post, palette));
    }
    for entry in entries {
        lines.push(String::new());
        lines.extend(entry_lines(entry, palette));
    }
    lines.join("\n")
}

fn entry_lines(entry: &ChangeEntry, palette: Palette) -> Vec<String> {
    let size = ChangeSize::of_entries([entry]);
    let heading = format!(
        "Changes {} by {}: {size}",
        entry.numbers_text(),
        entry.space.label()
    );
    let files = (entry.files.iter()).map(|file| match file.kind {
        ChangeKind::Text => format!("    {}  +{} −{}", file.path, file.added, file.removed),
        kind => format!("    {} ({kind})", file.path),
    });
    let more = (entry.more.iter()).map(|more| {
        let files = ChangeSize {
            files: more.files as usize,
            added: more.added,
            removed: more.removed,
        };
        format!("    and {files} in other files")
    });
    std::iter::once(format!("  {}", palette.paint(Style::Muted, heading)))
        .chain(files)
        .chain(more)
        .collect()
}

fn post_lines(thread: &Thread, number: u32, post: &Post, palette: Palette) -> Vec<String> {
    let author = post.author();
    let heading = match (&post.forwarded_to, post.side) {
        (Some(to), Some(Side::Opener)) => {
            format!("{author} forwarded its opener's side to {}", to.label())
        }
        (Some(to), _) => format!("{author} forwarded it to {}", to.label()),
        (None, _) if post.in_chat => format!("#{number} {author}, in the chat"),
        (None, None) => format!("#{number} {author}, participating"),
        (None, Some(_)) => format!("#{number} {author}"),
    };
    let mut lines = vec![format!("  {}", palette.paint(Style::Muted, heading))];
    lines.extend((post.post.text.lines()).map(|line| format!("    {line}").trim_end().to_owned()));
    let interaction = thread.interaction_lines(post).into_iter();
    lines.extend(interaction.map(|line| format!("    {line}").trim_end().to_owned()));
    for file in &post.files {
        lines.push(format!("    Attached file: {}", file.shown()));
    }
    lines.extend(post.code.iter().flat_map(code_lines));
    lines.extend(post.changes.iter().flat_map(change_lines));
    lines
}

fn change_lines(diff: &ChangeDiff) -> Vec<String> {
    let change = &diff.change;
    let place = format!(
        "    Change {}: {} in {}",
        change.number,
        change.place(),
        change.space.label()
    );
    let patch = diff.patch().unwrap_or_default();
    let patch = patch.lines().map(|line| format!("      {line}"));
    std::iter::once(place).chain(patch).collect()
}

pub(super) fn code_lines(code: &CodeRef) -> Vec<String> {
    let place = format!("    Code: {} in {}", code.place(), code.space.label());
    let snippet = code.snippet_lines("      ");
    let snippet = snippet.split('\n').skip(1).map(str::to_owned);
    std::iter::once(place).chain(snippet).collect()
}
