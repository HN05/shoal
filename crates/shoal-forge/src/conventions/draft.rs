//! Draft state a forge keeps in a PR's title, as a prefix it reads and
//! writes there.

/// The title prefixes a forge reads as marking a draft, and the one it
/// writes.
pub(crate) struct DraftMarker {
    /// Prefixes that mark a draft, whatever their ASCII case.
    pub prefixes: &'static [&'static str],
    /// The prefix that marks a new draft.
    pub marker: &'static str,
}

impl DraftMarker {
    /// The length of a draft prefix at the start of `title`.
    pub fn prefix_len(&self, title: &str) -> Option<usize> {
        self.prefixes.iter().find_map(|prefix| {
            title
                .get(..prefix.len())
                .filter(|start| start.eq_ignore_ascii_case(prefix))
                .map(|_| prefix.len())
        })
    }

    /// The title with its draft prefix added or removed.
    pub fn draft_title(&self, title: &str, draft: bool) -> String {
        let bare = self
            .prefix_len(title)
            .map_or(title, |len| title[len..].trim_start());
        if draft {
            format!("{} {bare}", self.marker)
        } else {
            bare.to_owned()
        }
    }
}

#[cfg(test)]
mod tests;
