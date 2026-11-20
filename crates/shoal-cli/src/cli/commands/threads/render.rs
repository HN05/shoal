//! How a thread reads on the terminal: one line to choose it by, a row of
//! a list, and the whole thread with its posts.
use shoal_protocol::threads::{Post, Side, Thread};

use crate::cli::{
    commands::alerts::hanging_indent,
    output::{Palette, Style},
    ui::{self, Cell},
};

/// Where `thread` stands: whose turn it is, or how it was settled.
pub(super) fn status(thread: &Thread) -> String {
    match (&thread.resolution, thread.awaits()) {
        (Some(resolution), _) => resolution.outcome.describe(),
        (None, None) => "empty".into(),
        (None, Some(side)) => format!("waiting for {}", thread.party(side)),
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

/// The thread's number and name, labeled rows of who opened and holds it
/// and where it stands, then each post.
pub(super) fn details(thread: &Thread, palette: Palette) -> String {
    let mut rows: Vec<[Cell; 2]> = Vec::new();
    let mut row = |label: &str, value: String| {
        rows.push([(label.to_owned(), Some(Style::Muted)), (value, None)]);
    };
    row("Status", status(thread));
    row("Kind", thread.kind.to_string());
    row("Opened by", thread.party(Side::Opener));
    row("Held by", thread.party(Side::Holder));
    let heading = format!("Thread {}: {}", thread.number, thread.one_line());
    let mut lines = vec![palette.paint(Style::Heading, heading)];
    lines.extend(
        ui::aligned(rows, palette)
            .into_iter()
            .map(|line| format!("  {line}")),
    );
    for post in &thread.posts {
        lines.push(String::new());
        lines.extend(post_lines(post, palette));
    }
    lines.join("\n")
}

/// A post: who sent it, its text, and what it offers or picks.
fn post_lines(post: &Post, palette: Palette) -> Vec<String> {
    let author = post.author();
    let heading = match &post.forwarded_to {
        Some(holder) => format!("{author} forwarded it to {}", holder.label()),
        None if post.in_chat => format!("{author}, in the chat"),
        None => author,
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
    lines
}
