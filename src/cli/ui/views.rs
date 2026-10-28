//! View descriptions, initial picker bindings and in-place transitions.
use anyhow::{Context as _, Result};

use super::{Entries, Picked, picker::entry_id, picker::fzf_output};
use crate::cli::context::Context;

/// The presentation and available actions of one picker view.
pub struct PickerView<'a> {
    pub prompt: &'a str,
    pub header: String,
    /// The keys this view binds, in header order, `enter` included.
    pub keys: Vec<&'a str>,
}

impl PickerView<'_> {
    fn unbound<'a>(&self, keys: &'a [&'a str]) -> Vec<&'a str> {
        keys.iter()
            .copied()
            .filter(|key| !self.keys.contains(key))
            .collect()
    }

    /// Switch to this view and reload its rows. The command goes last so
    /// its shell text can contain any character.
    pub fn transition(&self, keys: &[&str], reload: &str) -> String {
        let mut actions = Vec::new();
        let unbound = self.unbound(keys);
        if !unbound.is_empty() {
            actions.push(format!("unbind({})", unbound.join(",")));
        }
        actions.extend([
            format!("rebind({})", self.keys.join(",")),
            format!("change-prompt({})", self.prompt),
            format!("change-header({})", self.header),
            format!("reload:{reload}"),
        ]);
        actions.join("+")
    }
}

/// The next available view after the one the picker currently shows.
/// An absent or unknown prompt starts at the first view.
pub fn next_picker_view<T>(views: &[T], prompt: impl Fn(&T) -> &str) -> Result<&T> {
    let shown = std::env::var("FZF_PROMPT").unwrap_or_default();
    view_after(views, &shown, prompt)
}

fn view_after<'a, T>(views: &'a [T], shown: &str, prompt: impl Fn(&T) -> &str) -> Result<&'a T> {
    let next = views
        .iter()
        .position(|view| prompt(view) == shown)
        .map_or(0, |at| (at + 1) % views.len());
    views.get(next).context("no picker views available")
}

/// A picker whose keys are bound per view, and which Tab can switch to
/// another view by running `next`.
pub struct ViewPicker<'a> {
    pub view: PickerView<'a>,
    /// Every key a view binds, by fzf name, `enter` included.
    pub keys: &'a [&'a str],
    /// The command whose output, fzf actions, shows the next view.
    pub next: Option<String>,
    /// The search the picker starts with.
    pub query: Option<String>,
}

impl ViewPicker<'_> {
    fn arguments(&self) -> Vec<String> {
        let mut args = vec!["--header".into(), self.view.header.clone()];
        for key in self.keys {
            args.extend(["--bind".into(), format!("{key}:print({key})+accept")]);
        }
        let unbound = self.view.unbound(self.keys);
        if !unbound.is_empty() {
            args.extend([
                "--bind".into(),
                format!("start:unbind({})", unbound.join(",")),
            ]);
        }
        if let Some(next) = &self.next {
            args.extend(["--bind".into(), format!("tab:transform({next})")]);
        }
        if let Some(query) = &self.query {
            args.extend(["--query".into(), query.clone()]);
        }
        args
    }
}

/// Choose an entry with one of `picker.keys`, returning the key pressed.
/// The first view may be empty, and the entry may come from a view Tab
/// loaded, so the caller checks the id.
pub fn pick_in_views(
    ctx: &Context,
    entries: Entries,
    picker: ViewPicker<'_>,
) -> Result<Picked<String>> {
    let output = fzf_output(ctx, picker.view.prompt, &entries, &picker.arguments())?;
    let (key, selected) = output.split_once('\n').context("picker omitted action")?;
    Ok(Picked {
        action: key.to_owned(),
        id: entry_id(selected),
    })
}

#[cfg(test)]
mod tests;
