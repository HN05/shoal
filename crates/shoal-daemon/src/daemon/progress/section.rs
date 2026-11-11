//! The progress section Shoal keeps at the end of a PR's body: the status
//! line and the steps between two markers. The section is Shoal's, so a
//! later update replaces what anyone wrote between them; the rest of the
//! body stays as it was, byte for byte.
use crate::client::progress::Progress;

const START: &str = "<!-- shoal:progress -->";
const END: &str = "<!-- /shoal:progress -->";

/// What separates the section from the body before it.
const SEPARATOR: &str = "\n\n";

/// The section that shows `progress`.
pub(super) fn render(progress: &Progress) -> String {
    let mut lines = vec![START.to_owned()];
    if let Some(status) = &progress.status {
        lines.push(format!("**Status:** {}", escaped(status)));
    }
    if progress.status.is_some() && !progress.steps.is_empty() {
        lines.push(String::new());
    }
    for step in &progress.steps {
        let check = if step.done { "x" } else { " " };
        lines.push(format!("- [{check}] {}", escaped(&step.name)));
    }
    lines.push(END.to_owned());
    lines.join("\n")
}

/// `text` unable to end the section early or open another.
fn escaped(text: &str) -> String {
    text.replace("<!--", "&lt;!--")
}

/// `body` with its section replaced by `section`, or `section` added at its
/// end; `None` removes the section.
pub(crate) fn with_section(body: &str, section: Option<&str>) -> String {
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

/// Where the section is in `body`, from its start marker to the end of its
/// end marker: the last start marker outside a fenced code block, since
/// Shoal appends its section last and a description may quote the markers.
fn find(body: &str) -> Option<std::ops::Range<usize>> {
    let start = (body.match_indices(START))
        .map(|(start, _)| start)
        .filter(|start| !in_code_block(&body[..*start]))
        .last()?;
    let end = start + body[start..].find(END)? + END.len();
    Some(start..end)
}

/// Whether text that follows `before` sits in a fenced code block: after
/// an odd number of lines that open or close one.
fn in_code_block(before: &str) -> bool {
    let fences = (before.lines())
        .filter(|line| line.trim_start().starts_with("```"))
        .count();
    fences % 2 == 1
}

#[cfg(test)]
mod tests;
