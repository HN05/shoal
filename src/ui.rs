//! Interactive input: confirmations, free-text prompts, fzf pickers, and the
//! labels shown in them. Every prompt requires a terminal and can be canceled
//! with Ctrl-C; non-interactive callers must pass explicit flags instead.
use std::{
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result, ensure};

use crate::{
    client,
    context::Context,
    model::{Repository, Workspace},
    output::{Palette, Style},
    removal::{BranchChoice, RemovalCheck},
};

fn require_interactive(ctx: &Context) -> Result<()> {
    ensure!(
        ctx.interactive(),
        "missing argument; pass an explicit target/name in non-interactive mode"
    );
    Ok(())
}

/// Ctrl-C must cancel a prompt even after the execution wrapper registered its
/// own SIGINT handlers (which persist for the process lifetime and would
/// otherwise swallow the keystroke). Restore the default disposition for the
/// duration of the read, then put the previous handler back.
struct InterruptCancels(libc::sigaction);

impl InterruptCancels {
    fn install() -> Self {
        // SAFETY: plain sigaction calls on this process; the previous action is
        // captured in full (handler, mask, and flags) and restored on drop.
        unsafe {
            let mut default: libc::sigaction = std::mem::zeroed();
            default.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut default.sa_mask);
            let mut previous: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGINT, &default, &mut previous);
            Self(previous)
        }
    }
}

impl Drop for InterruptCancels {
    fn drop(&mut self) {
        // SAFETY: restores the action captured in `install`.
        unsafe {
            libc::sigaction(libc::SIGINT, &self.0, std::ptr::null_mut());
        }
    }
}

/// Read one answer from the terminal. `None` at end of input.
fn read_answer(input: &mut impl io::BufRead) -> Result<Option<String>> {
    let _cancel = InterruptCancels::install();
    let mut answer = String::new();
    if input.read_line(&mut answer)? == 0 {
        return Ok(None);
    }
    Ok(Some(answer.trim().to_ascii_lowercase()))
}

/// Approval stays in the CLI. Piped/JSON callers must opt in explicitly.
pub fn confirm(ctx: &Context, action: &str, flag: &str) -> Result<bool> {
    ensure!(
        ctx.interactive(),
        "{action}; confirmation required in non-interactive mode; pass {flag}"
    );
    confirm_with_io(action, &mut io::stdin().lock(), &mut io::stderr().lock())
}

fn confirm_with_io(
    action: &str,
    input: &mut impl io::BufRead,
    output: &mut impl Write,
) -> Result<bool> {
    writeln!(
        output,
        "{}",
        Palette::stderr(false).paint(Style::Warning, action)
    )?;
    loop {
        write!(output, "Are you sure? [y/N] ")?;
        output.flush()?;
        match read_answer(input)?.as_deref() {
            None | Some("" | "n" | "no") => return Ok(false),
            Some("y" | "yes") => return Ok(true),
            Some(_) => writeln!(output, "Please enter y or n.")?,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SetupFailureChoice {
    Delete,
    Ignore,
    Cancel,
}

pub fn setup_failure_choice() -> Result<SetupFailureChoice> {
    setup_failure_with_io(&mut io::stdin().lock(), &mut io::stderr().lock())
}

fn setup_failure_with_io(
    input: &mut impl io::BufRead,
    output: &mut impl Write,
) -> Result<SetupFailureChoice> {
    loop {
        write!(
            output,
            "Setup failed: [d]elete workspace, [i]gnore and continue, or [c]ancel (keep for inspection) [c]: "
        )?;
        output.flush()?;
        match read_answer(input)?.as_deref() {
            None | Some("" | "c" | "cancel") => return Ok(SetupFailureChoice::Cancel),
            Some("d" | "delete") => return Ok(SetupFailureChoice::Delete),
            Some("i" | "ignore") => return Ok(SetupFailureChoice::Ignore),
            Some(_) => writeln!(output, "Please enter d, i, or c.")?,
        }
    }
}

pub fn input(ctx: &Context, prompt: &str) -> Result<String> {
    require_interactive(ctx)?;
    eprint!(
        "{} ",
        Palette::stderr(ctx.json).paint(Style::Heading, format_args!("{prompt}:"))
    );
    io::stderr().flush()?;
    let _cancel = InterruptCancels::install();
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    let value = value.trim_end_matches(['\r', '\n']).to_owned();
    ensure!(!value.is_empty(), "canceled: no value supplied");
    Ok(value)
}

pub fn choose_removal(ctx: &Context, check: &RemovalCheck) -> Result<BranchChoice> {
    let warnings = check.warnings();
    ensure!(
        ctx.interactive(),
        "removal requires a branch choice: {}. Pass --yes with --keep-branch or --delete-branch",
        warnings.join("; ")
    );
    eprintln!("Workspace: {}", check.workspace.name);
    eprintln!("Branch:    {}", check.branch.as_deref().unwrap_or("none"));
    for warning in warnings {
        eprintln!(
            "  - {}",
            Palette::stderr(ctx.json).paint(Style::Warning, warning)
        );
    }
    pick_choice(
        ctx,
        "Branch action> ",
        &[
            (None, "Cancel         Keep everything"),
            (
                Some(BranchChoice::KeepBranch),
                "Keep branch    Delete workspace files only",
            ),
            (
                Some(BranchChoice::DeleteBranch),
                "Delete branch  Delete workspace files and branch",
            ),
        ],
    )?
    .context("removal canceled")
}

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
    Ok(run_picker(ctx, prompt, entries, None)?.id)
}

pub fn pick_with_keys<T: Clone>(
    ctx: &Context,
    prompt: &str,
    entries: Entries,
    bindings: KeyBindings<'_, T>,
) -> Result<Picked<T>> {
    let picked = run_picker(
        ctx,
        prompt,
        entries,
        Some((bindings.keys(), bindings.header())),
    )?;
    Ok(Picked {
        action: bindings.action(&picked.action)?,
        id: picked.id,
    })
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

fn run_picker(
    ctx: &Context,
    prompt: &str,
    entries: Entries,
    bindings: Option<(String, String)>,
) -> Result<Picked<String>> {
    require_interactive(ctx)?;
    ensure!(
        !entries.is_empty(),
        "nothing to select; register a repository with `shoal repo add <path-or-url>` or create a workspace with `shoal add`"
    );
    let mut command = Command::new("fzf");
    command
        .args([
            "--no-sort",
            "--ansi",
            "--delimiter=\t",
            "--with-nth=2..",
            "--prompt",
            prompt,
        ])
        .env_remove("FZF_DEFAULT_OPTS")
        .env_remove("FZF_DEFAULT_OPTS_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some((keys, header)) = &bindings {
        command
            .arg(format!("--expect={keys}"))
            .args(["--header", header]);
    }
    let mut picker = command
        .spawn()
        .context("open picker; install fzf or supply an explicit target")?;
    let mut input = picker.stdin.take().context("picker stdin is unavailable")?;
    for (id, label) in &entries {
        let label = label.replace(['\n', '\r', '\t'], " ");
        // fzf may exit before consuming the entire list when canceled.
        if writeln!(input, "{id}\t{label}").is_err() {
            break;
        }
    }
    drop(input);
    let output = picker.wait_with_output()?;
    ensure!(output.status.success(), "selection canceled");
    let output = String::from_utf8(output.stdout)?;
    let (key, selected) = if bindings.is_some() {
        output.split_once('\n').context("picker omitted action")?
    } else {
        ("", output.as_str())
    };
    let id = selected
        .split('\t')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    ensure!(
        entries.iter().any(|entry| entry.0 == id),
        "picker returned an unknown item"
    );
    Ok(Picked {
        action: key.to_owned(),
        id,
    })
}

/// How to choose a workspace when the command line names none.
#[derive(Clone, Copy)]
pub enum Fallback {
    /// Use the worktree containing the current directory, else open the picker.
    CurrentDirectory,
    /// Use only the current workspace; never open a picker.
    CurrentDirectoryOnly,
    /// Always open the picker.
    Picker,
}

pub async fn select_workspace(
    ctx: &Context,
    explicit: Option<String>,
    fallback: Fallback,
) -> Result<String> {
    if let Some(selector) = explicit {
        return Ok(selector);
    }
    let workspaces = client::workspaces(&ctx.paths).await?;
    if crate::env::is_scoped() {
        // The daemon filters and authorizes the list, so this stays bound to
        // the execution even when the process changes its working directory.
        return workspaces
            .first()
            .map(|w| w.id.clone())
            .context("scoped workspace is unavailable");
    }
    if !matches!(fallback, Fallback::Picker) {
        let cwd = std::fs::canonicalize(std::env::current_dir()?)?;
        if let Some(workspace) = Workspace::innermost(&workspaces, &cwd) {
            return Ok(workspace.id.clone());
        }
    }
    ensure!(
        !matches!(fallback, Fallback::CurrentDirectoryOnly),
        "no current workspace; pass an explicit workspace"
    );
    pick_workspace(ctx, workspaces)
}

/// `Some(workspace)` unless `--all` was passed.
pub async fn select_workspace_filter(
    ctx: &Context,
    explicit: Option<String>,
    all: bool,
) -> Result<Option<String>> {
    if all {
        return Ok(None);
    }
    select_workspace(ctx, explicit, Fallback::CurrentDirectory)
        .await
        .map(Some)
}

/// Always run the picker, including for scoped callers (whose list is filtered).
pub async fn workspace_picker(ctx: &Context) -> Result<String> {
    let workspaces = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .filter(|w| w.path.is_dir())
        .collect();
    pick_workspace(ctx, workspaces)
}

fn pick_workspace(ctx: &Context, workspaces: Vec<Workspace>) -> Result<String> {
    let palette = Palette::stderr(ctx.json);
    pick(
        ctx,
        "Workspace> ",
        workspaces
            .into_iter()
            .map(|w| (w.id.clone(), workspace_label(&w, palette)))
            .collect(),
    )
}

pub fn workspace_label(workspace: &Workspace, palette: Palette) -> String {
    format!(
        "{}  {}  {}  {}",
        palette.paint(Style::Heading, &workspace.name),
        palette.workspace_state(workspace.state),
        workspace.branch,
        palette.paint(Style::Muted, workspace.path.display())
    )
}

pub fn repository_label(repo: &Repository, palette: Palette) -> String {
    format!(
        "{}  {}",
        palette.paint(Style::Heading, crate::repository::name(repo)),
        palette.paint(Style::Muted, &repo.source)
    )
}

/// Picker entries that stay unambiguous when repositories share a name.
pub async fn repository_choices(mut repos: Vec<Repository>) -> Result<Entries> {
    for repo in &mut repos {
        if let Some(url) = crate::repository::remote_url(&repo.source).await? {
            repo.source = url;
        }
    }
    let labels: Vec<_> = repos
        .iter()
        .map(|repo| {
            let name = crate::repository::name(repo);
            let shared = repos
                .iter()
                .filter(|r| crate::repository::name(r) == name)
                .count()
                > 1;
            if !shared {
                return name.to_owned();
            }
            match crate::repository::host(&repo.source) {
                Some(host) => format!("{name} ({host})"),
                None => format!("{name} (local)"),
            }
        })
        .collect();
    Ok(repos
        .into_iter()
        .zip(&labels)
        .map(|(repo, label)| {
            // Same host or multiple local checkouts can still share a name.
            let label = if labels.iter().filter(|other| *other == label).count() > 1 {
                format!("{label}  {}", repo.source)
            } else {
                label.clone()
            };
            (repo.id, label)
        })
        .collect())
}

/// Existing local paths become canonical so the daemon matches them by path.
pub fn repository_selector(value: String) -> Result<String> {
    if Path::new(&value).exists() {
        Ok(std::fs::canonicalize(&value)?
            .to_str()
            .context("repository path is not UTF-8")?
            .to_owned())
    } else {
        Ok(value)
    }
}
