//! fzf pickers: one entry, optionally with a key that names the action.
use std::{
    io::Write,
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result, bail, ensure};

use super::{current_directory, require_interactive};
use crate::{cli::context::Context, tools::Tool};

/// `(id, label)` pairs; the id is returned, only the label is shown.
pub type Entries = Vec<(String, String)>;

/// A typed action and the selected entry id.
pub struct Picked<T> {
    pub action: T,
    pub id: String,
}

/// `(key, label, action)` bindings, including `enter` for the default action.
pub struct KeyBindings<'a, T>(pub &'a [(&'a str, &'a str, T)]);

impl<T: Clone> KeyBindings<'_, T> {
    fn keys(&self) -> String {
        self.0
            .iter()
            .map(|(key, _, _)| *key)
            .filter(|key| *key != "enter")
            .collect::<Vec<_>>()
            .join(",")
    }

    fn header(&self) -> String {
        self.0
            .iter()
            .map(|(key, label, _)| format!("{key}: {label}"))
            .collect::<Vec<_>>()
            .join("   ")
    }

    fn action(&self, key: &str) -> Result<T> {
        let key = if key.is_empty() { "enter" } else { key };
        self.0
            .iter()
            .find(|(bound, _, _)| *bound == key)
            .map(|(_, _, action)| action.clone())
            .context("unknown picker action")
    }
}

pub fn pick(ctx: &Context, prompt: &str, entries: Entries) -> Result<String> {
    Ok(run_picker(ctx, prompt, entries, Vec::new(), false)?.id)
}

pub fn pick_with_keys<T: Clone>(
    ctx: &Context,
    prompt: &str,
    entries: Entries,
    bindings: KeyBindings<'_, T>,
) -> Result<Picked<T>> {
    let args = vec![
        format!("--expect={}", bindings.keys()),
        "--header".into(),
        bindings.header(),
    ];
    let picked = run_picker(ctx, prompt, entries, args, true)?;
    Ok(Picked {
        action: bindings.action(&picked.action)?,
        id: picked.id,
    })
}

/// A key that acts on the highlighted entry without leaving the picker.
pub struct InPlaceKey<'a> {
    pub key: &'a str,
    pub label: &'a str,
    /// Shell text that, given the entry's id as its last argument, acts on
    /// it and prints the outcome.
    pub command: String,
}

impl InPlaceKey<'_> {
    /// Run the command on the highlighted entry, show its output or error
    /// above `hints`, then reload the rows. The reload goes last so its
    /// shell text can contain any character.
    fn binding(&self, hints: &str, reload: &str) -> Result<String> {
        let header = format!(
            "{} {{1}} 2>&1; printf '%s\\n' {}",
            self.command,
            crate::shell::quote(&[hints])
        );
        let header = enclosed("transform-header", &header)?;
        Ok(format!("{}:{header}+reload:{reload}", self.key))
    }
}

/// Choose an entry with Enter, labeled `enter`, while `keys` act on the
/// highlighted one in place and the rows reload from `reload`. The entry may
/// come from a reload, so the caller checks the id.
pub fn pick_in_place(
    ctx: &Context,
    prompt: &str,
    entries: Entries,
    enter: &str,
    keys: &[InPlaceKey<'_>],
    reload: &str,
) -> Result<String> {
    let hints = std::iter::once(format!("enter: {enter}"))
        .chain(keys.iter().map(|key| format!("{}: {}", key.key, key.label)))
        .collect::<Vec<_>>()
        .join("   ");
    let mut args = vec!["--header".into(), hints.clone()];
    for key in keys {
        args.extend(["--bind".into(), key.binding(&hints, reload)?]);
    }
    Ok(entry_id(&picker_output(ctx, prompt, &entries, &args)?))
}

/// The fzf `action` with `argument` between delimiters it accepts, chosen
/// so that the argument does not close them.
fn enclosed(action: &str, argument: &str) -> Result<String> {
    const DELIMITERS: [(char, char); 8] = [
        ('(', ')'),
        ('[', ']'),
        ('<', '>'),
        ('~', '~'),
        ('!', '!'),
        ('@', '@'),
        ('#', '#'),
        ('^', '^'),
    ];
    DELIMITERS
        .into_iter()
        .find(|(_, close)| !argument.contains(*close))
        .map(|(open, close)| format!("{action}{open}{argument}{close}"))
        .context("the picker command contains every fzf delimiter")
}

/// Choose an open issue or PR, returning its number; `kind` names them when
/// there are none.
pub fn pick_item(
    ctx: &Context,
    prompt: &str,
    kind: &str,
    items: Vec<crate::forge::items::item::Item>,
) -> Result<String> {
    ensure!(!items.is_empty(), "no open {kind}");
    let entries = items
        .into_iter()
        .map(|item| {
            (
                item.number.to_string(),
                format!("#{}  {}", item.number, item.title),
            )
        })
        .collect();
    pick(ctx, prompt, entries)
}

/// Choose a typed value; labels are display-only, even when they repeat.
pub fn pick_choice<T: Clone>(ctx: &Context, prompt: &str, choices: &[(T, &str)]) -> Result<T> {
    let entries = choices
        .iter()
        .enumerate()
        .map(|(index, (_, label))| (index.to_string(), (*label).to_owned()))
        .collect();
    let id = pick(ctx, prompt, entries)?;
    let index: usize = id.parse().context("picker returned an invalid choice")?;
    choices
        .get(index)
        .map(|(choice, _)| choice.clone())
        .context("picker returned an unknown choice")
}

/// The width picker rows should fit: the terminal's, less fzf's pointer
/// and scrollbar columns.
pub fn picker_width() -> Option<usize> {
    let terminal = std::fs::File::open("/dev/tty").ok()?;
    Some(crate::cli::output::terminal_width(terminal)?.saturating_sub(3))
}

/// One picker input line: the id, a tab, and the label on one line.
pub fn picker_line(id: &str, label: &str) -> String {
    format!("{id}\t{}", label.replace(['\n', '\r', '\t'], " "))
}

/// `keyed` pickers print the key that chose the entry on the line before it.
fn run_picker(
    ctx: &Context,
    prompt: &str,
    entries: Entries,
    extra: Vec<String>,
    keyed: bool,
) -> Result<Picked<String>> {
    let output = picker_output(ctx, prompt, &entries, &extra)?;
    let (key, selected) = if keyed {
        output.split_once('\n').context("picker omitted action")?
    } else {
        ("", output.as_str())
    };
    let id = entry_id(selected);
    ensure!(
        entries.iter().any(|entry| entry.0 == id),
        "picker returned an unknown item"
    );
    Ok(Picked {
        action: key.to_owned(),
        id,
    })
}

/// Run fzf over the entries with `extra` arguments and return what it printed.
pub(super) fn picker_output(
    ctx: &Context,
    prompt: &str,
    entries: &Entries,
    extra: &[String],
) -> Result<String> {
    ensure!(
        !entries.is_empty(),
        "nothing to select; register a repository with `shoal repo add <path-or-url>` or create a workspace with `shoal add`"
    );
    fzf_output(ctx, prompt, entries, extra)
}

/// `picker_output` for any number of entries, none included.
pub(super) fn fzf_output(
    ctx: &Context,
    prompt: &str,
    entries: &Entries,
    extra: &[String],
) -> Result<String> {
    require_interactive(ctx)?;
    let mut command = Command::new(Tool::Fzf.program());
    if current_directory()?.is_none() {
        command.current_dir("/");
    }
    command
        .args([
            "--no-sort",
            "--ansi",
            "--delimiter=\t",
            "--with-nth=2..",
            // The count goes at the end of the prompt line, so the key
            // hints sit directly above the prompt.
            "--info=inline-right",
            "--no-separator",
            "--prompt",
            prompt,
        ])
        .args(extra)
        .env_remove("FZF_DEFAULT_OPTS")
        .env_remove("FZF_DEFAULT_OPTS_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut picker = command
        .spawn()
        .context("open picker; install fzf or supply an explicit target")?;
    let mut input = picker.stdin.take().context("picker stdin is unavailable")?;
    for (id, label) in entries {
        // fzf may exit before consuming the entire list when canceled.
        if writeln!(input, "{}", picker_line(id, label)).is_err() {
            break;
        }
    }
    drop(input);
    let output = picker.wait_with_output()?;
    // fzf exits 1 without a match and 130 when the user leaves; 2 is its own error.
    match output.status.code() {
        Some(0) => Ok(String::from_utf8(output.stdout)?),
        Some(1 | 130) => Err(SelectionCanceled.into()),
        _ => bail!("picker failed: {}", output.status),
    }
}

/// The user left the picker without choosing. The CLI exits without an error
/// message, as it does when Ctrl-C cancels a prompt.
#[derive(Debug)]
pub struct SelectionCanceled;

impl std::fmt::Display for SelectionCanceled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("selection canceled")
    }
}

impl std::error::Error for SelectionCanceled {}

/// The id field of a line fzf printed.
pub(super) fn entry_id(line: &str) -> String {
    line.split('\t')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests;
