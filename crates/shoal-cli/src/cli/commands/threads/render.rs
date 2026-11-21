//! How a thread reads on the terminal: one line to choose it by, a row of
//! a list, and the whole thread with its posts.
use shoal_protocol::{
    changes::{ChangeDiff, ChangeEntry, ChangeKind, ChangeSize},
    code::CodeRef,
    threads::{Finding, Post, Side, Thread},
};

use crate::cli::{
    commands::alerts::hanging_indent,
    output::{Palette, Style},
    ui::{self, Cell},
};

/// Where `thread` stands: whose turn it is, and whether its holder's agent
/// works for it, or how it was settled.
pub(super) fn status(thread: &Thread) -> String {
    let turn = match (&thread.resolution, thread.awaits()) {
        (Some(resolution), _) => return resolution.outcome.describe(),
        (None, None) => "empty".into(),
        (None, Some(side)) => format!("waiting for {}", thread.party(side)),
    };
    match thread.working {
        true => format!("working; {turn}"),
        false => turn,
    }
}

/// One line with what the user needs to choose a thread: its number, its
/// space, its name and its latest post.
pub(in crate::cli) fn summary(thread: &Thread) -> String {
    let mut line = format!("{}  {}: {}", thread.number, thread.space, thread.one_line());
    if let Some(post) = thread.last_post() {
        let text = post.post.text.replace(['\n', '\t'], " ");
        line.push_str(&format!("  {}: {text}", post.author()));
    }
    line
}

/// A row of a printed list: the number, where it stands, its space and
/// name, then its latest post.
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

/// The thread's kind, with a review's finding's severity and review.
fn kind(thread: &Thread) -> String {
    match thread.finding {
        None => thread.kind.to_string(),
        Some(Finding {
            severity,
            review: None,
        }) => format!("{} ({severity})", thread.kind),
        Some(Finding {
            severity,
            review: Some(review),
        }) => format!("{} ({severity}, review {review})", thread.kind),
    }
}

/// The thread's number and name, labeled rows of who opened and holds it
/// and where it stands, then each post.
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
        lines.extend(post_lines(number, post, palette));
    }
    for entry in entries {
        lines.push(String::new());
        lines.extend(entry_lines(entry, palette));
    }
    lines.join("\n")
}

/// What one turn of an agent changed for the thread: its size and change
/// numbers, then each file, with its size or kind.
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

/// A post, `number` among those that speak: who sent it, its text, and
/// what it offers or picks.
fn post_lines(number: u32, post: &Post, palette: Palette) -> Vec<String> {
    let author = post.author();
    let heading = match &post.forwarded_to {
        Some(holder) => format!("{author} forwarded it to {}", holder.label()),
        None if post.in_chat => format!("#{number} {author}, in the chat"),
        None => format!("#{number} {author}"),
    };
    let mut lines = vec![format!("  {}", palette.paint(Style::Muted, heading))];
    lines.extend((post.post.text.lines()).map(|line| format!("    {line}").trim_end().to_owned()));
    for (number, choice) in (1..).zip(&post.post.choices) {
        lines.push(format!("    {number}. {choice}"));
    }
    if let Some(recommendation) = &post.post.recommendation {
        lines.push(format!("    Recommended: {recommendation}"));
    }
    if let Some(reason) = &post.post.reason {
        lines.push(format!("    Reason: {reason}"));
    }
    if let Some(choice) = post.post.choice {
        lines.push(format!("    Chose {choice}"));
    }
    for file in &post.files {
        lines.push(format!("    Attached file: {}", file.path.display()));
    }
    lines.extend(post.code.iter().flat_map(code_lines));
    lines.extend(post.changes.iter().flat_map(change_lines));
    lines
}

/// A change a post points at, where it is now in the worktree of its
/// space, with its diff.
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

/// A place in code a post points at, where it is now in the worktree of
/// its space, with its state and snippet.
pub(super) fn code_lines(code: &CodeRef) -> Vec<String> {
    let place = format!("    Code: {} in {}", code.place(), code.space.label());
    let snippet = code.snippet_lines("      ");
    let snippet = snippet.split('\n').skip(1).map(str::to_owned);
    std::iter::once(place).chain(snippet).collect()
}
