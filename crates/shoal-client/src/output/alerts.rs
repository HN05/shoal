//! How an alert reads where agents and the user see it: in a hook's
//! context, a list or a picker's preview.
use shoal_protocol::alerts::Alert;

/// The alert's text, after the space that sent it or that it is about,
/// followed by a line for each file attached to it.
pub fn shown(alert: &Alert) -> String {
    let attached =
        (alert.attachments().into_iter()).map(|path| format!("Attached file: {}", path.display()));
    std::iter::once(alert.text())
        .filter(|text| !text.is_empty())
        .chain(attached)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `text` with its later lines indented by `indent`, so a multi-line alert
/// reads as one entry of a list.
pub fn hanging_indent(text: &str, indent: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(n, line)| {
            if n == 0 || line.is_empty() {
                line.to_owned()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests;
