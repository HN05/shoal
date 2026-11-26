//! The progress section Shoal keeps at the end of a PR's body, between two
//! markers. The section is Shoal's, so an update replaces what anyone wrote
//! between them; the rest of the body stays as it was, byte for byte.
use shoal_protocol::progress::{Progress, RecordedBlocker};

const START: &str = "<!-- shoal:progress -->";
const END: &str = "<!-- /shoal:progress -->";

const SEPARATOR: &str = "\n\n";

pub fn render(progress: &Progress) -> String {
    let mut lines = vec![START.to_owned()];
    if let Some(status) = &progress.status {
        lines.push(format!("**Status:** {}", escaped(status)));
    }
    for blocker in &progress.blockers {
        lines.push(format!("**Blocked by** {}", blocked_by(blocker)));
    }
    let heading = progress.status.is_some() || !progress.blockers.is_empty();
    if heading && !progress.steps.is_empty() {
        lines.push(String::new());
    }
    for step in &progress.steps {
        let check = if step.done { "x" } else { " " };
        lines.push(format!("- [{check}] {}", escaped(&step.name)));
        for blocker in &step.blockers {
            lines.push(format!("  - Blocked by {}", blocked_by(blocker)));
        }
    }
    lines.push(END.to_owned());
    lines.join("\n")
}

fn blocked_by(blocker: &RecordedBlocker) -> String {
    escaped(&format!("{}: {}", blocker.cause(), blocker.reason))
}

/// Keeps `text` from ending the section early or opening another.
fn escaped(text: &str) -> String {
    text.replace("<!--", "&lt;!--")
}

/// Adds the section at the end when `body` has none. `None` removes it.
pub fn with_section(body: &str, section: Option<&str>) -> String {
    let (before, after) = match find(body) {
        Some(range) => (
            body[..range.start]
                .strip_suffix(SEPARATOR)
                .unwrap_or(&body[..range.start]),
            &body[range.end..],
        ),
        None => (body, ""),
    };
    let Some(section) = section else {
        return format!("{before}{after}");
    };
    let separator = if before.is_empty() { "" } else { SEPARATOR };
    format!("{before}{separator}{section}{after}")
}

/// From the start marker to the end of the end marker. Takes the last start
/// marker outside a fenced code block, since Shoal appends its section last
/// and a description may quote the markers.
fn find(body: &str) -> Option<std::ops::Range<usize>> {
    let start = (body.match_indices(START))
        .map(|(start, _)| start)
        .filter(|start| !in_code_block(&body[..*start]))
        .last()?;
    let end = start + body[start..].find(END)? + END.len();
    Some(start..end)
}

/// True after an odd number of lines that open or close a fenced code
/// block.
fn in_code_block(before: &str) -> bool {
    let fences = (before.lines())
        .filter(|line| line.trim_start().starts_with("```"))
        .count();
    fences % 2 == 1
}

#[cfg(test)]
mod tests;
