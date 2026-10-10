//! Interactive input: confirmations, free-text prompts, fzf pickers, and the
//! labels shown in them. Every prompt requires a terminal and can be canceled
//! with Ctrl-C; non-interactive callers must pass explicit flags instead.
use crate::tools::Tool;
use std::{
    collections::HashSet,
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        WorkspaceScope, client,
        context::Context,
        output::{Palette, Style, workspace_state_style},
        workspace_context::{ScopeOrder, WorkspaceContext},
    },
    forge::link::{self, ItemKind},
    model::{Repository, ReviewMark, Workspace},
    paths::Paths,
    removal::{BranchChoice, RemovalCheck},
    state::WorkspaceState,
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

/// Offer to run the fix an error would otherwise only suggest. Non-interactive
/// callers decline, so the error and its hint stand.
pub fn offer(ctx: &Context, question: &str) -> Result<bool> {
    if !ctx.interactive() {
        return Ok(false);
    }
    ask_yes_no(question, &mut io::stdin().lock(), &mut io::stderr().lock())
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
    ask_yes_no("Are you sure?", input, output)
}

fn ask_yes_no(
    question: &str,
    input: &mut impl io::BufRead,
    output: &mut impl Write,
) -> Result<bool> {
    loop {
        write!(output, "{question} [y/N] ")?;
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

/// Choose an open issue or PR, returning its number; `kind` names them when
/// there are none.
pub fn pick_item(
    ctx: &Context,
    prompt: &str,
    kind: &str,
    items: Vec<crate::forge::list::Item>,
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
}

#[derive(Debug)]
pub struct NoCurrentWorkspace;

impl std::fmt::Display for NoCurrentWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no current workspace; pass an explicit workspace")
    }
}

impl std::error::Error for NoCurrentWorkspace {}

pub(super) fn current_directory() -> io::Result<Option<std::path::PathBuf>> {
    match std::env::current_dir().and_then(std::fs::canonicalize) {
        Ok(cwd) => Ok(Some(cwd)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
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
    let scoped = crate::env::is_scoped();
    let cwd = if !scoped { current_directory()? } else { None };
    let context = WorkspaceContext::from_directory(&workspaces, cwd.as_deref());
    if let Some(workspace) = context.resolve(None, scoped, ScopeOrder::BeforeDirectory) {
        return Ok(workspace.id.clone());
    }
    ensure!(!scoped, "scoped workspace is unavailable");
    if matches!(fallback, Fallback::CurrentDirectoryOnly) {
        return Err(NoCurrentWorkspace.into());
    }
    pick_workspace(ctx, workspaces)
}

/// `Some(workspace)` unless `--all` was passed.
pub async fn select_workspace_filter(
    ctx: &Context,
    scope: WorkspaceScope,
) -> Result<Option<String>> {
    if scope.all {
        return Ok(None);
    }
    select_workspace(ctx, scope.workspace, Fallback::CurrentDirectory)
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

pub fn pick_workspace(ctx: &Context, workspaces: Vec<Workspace>) -> Result<String> {
    let stopped = stopped_workspaces(&ctx.paths, &workspaces);
    let rows = workspace_rows(&workspaces, &[], &stopped, Palette::stderr(ctx.json));
    pick(
        ctx,
        "Workspace> ",
        workspaces.into_iter().map(|w| w.id).zip(rows).collect(),
    )
}

/// Workspaces with agents or commands saved by `shoal stop` for `shoal resume`.
pub fn stopped_workspaces(paths: &Paths, workspaces: &[Workspace]) -> HashSet<String> {
    workspaces
        .iter()
        .filter(|w| crate::execution::recovery::pending(paths, &w.id).unwrap_or(false))
        .map(|w| w.id.clone())
        .collect()
}

/// What a workspace is doing: its state unless ready, then stopped work, a
/// current ready-for-review mark, a running agent or command, an outdated mark,
/// or idle.
fn status_cell(workspace: &Workspace, stopped: bool) -> Cell {
    if workspace.state != WorkspaceState::Ready {
        return (
            workspace.state.to_string(),
            Some(workspace_state_style(workspace.state)),
        );
    }
    let outdated = workspace.review.iter().any(|mark| mark.stale == Some(true));
    let (text, style) = if stopped {
        ("stopped", Style::Warning)
    } else if !workspace.review.is_empty() && !outdated {
        ("ready for review", Style::Success)
    } else if workspace.running {
        ("running", Style::Heading)
    } else if outdated {
        ("ready for review (outdated)", Style::Warning)
    } else {
        ("idle", Style::Muted)
    };
    (text.into(), Some(style))
}

/// One aligned row per workspace: a state marker, the name, the repository
/// when the rows span several (`(id, name)` pairs), the status, and the linked
/// issue's title when known.
pub fn workspace_rows(
    workspaces: &[Workspace],
    repositories: &[(String, String)],
    stopped: &HashSet<String>,
    palette: Palette,
) -> Vec<String> {
    let cells: Vec<[Cell; 4]> = workspaces
        .iter()
        .zip(repository_column(workspaces, repositories))
        .map(|(w, repository)| {
            [
                (w.name.clone(), Some(Style::Heading)),
                (repository, None),
                status_cell(w, stopped.contains(&w.id)),
                (w.links.issue_title.clone().unwrap_or_default(), None),
            ]
        })
        .collect();
    workspaces
        .iter()
        .zip(aligned(cells, palette))
        .map(|(workspace, row)| format!("{} {row}", palette.workspace_marker(workspace.state)))
        .collect()
}

/// One aligned row per ready-for-review mark: the workspace, its repository
/// when the rows span several, the marked PR or issue (or the workspace
/// itself), its URL, and whether new commits made the mark outdated.
pub fn review_rows(
    workspaces: &[Workspace],
    repositories: &[(String, String)],
    palette: Palette,
) -> Vec<String> {
    let cells: Vec<[Cell; 5]> = workspaces
        .iter()
        .zip(repository_column(workspaces, repositories))
        .flat_map(|(w, repository)| {
            w.review.iter().map(move |mark| {
                [
                    (w.name.clone(), Some(Style::Heading)),
                    (repository.clone(), None),
                    (mark_label(mark), None),
                    (mark.url.clone().unwrap_or_default(), None),
                    if mark.stale == Some(true) {
                        ("outdated".into(), Some(Style::Warning))
                    } else {
                        (String::new(), None)
                    },
                ]
            })
        })
        .collect();
    aligned(cells, palette)
}

/// `PR #12` or `issue #3` for a linked item, `workspace` for a mark made
/// while nothing was linked.
fn mark_label(mark: &ReviewMark) -> String {
    let (Some(kind), Some(url)) = (mark.kind, &mark.url) else {
        return "workspace".into();
    };
    let name = match kind {
        ItemKind::Pr => "PR",
        ItemKind::Issue => "issue",
    };
    match link::item(kind, url) {
        Ok((_, number)) => format!("{name} #{number}"),
        Err(_) => name.into(),
    }
}

/// Each workspace's repository name, or empty cells when all share one.
/// Repositories are `(id, name)` pairs.
fn repository_column(workspaces: &[Workspace], repositories: &[(String, String)]) -> Vec<String> {
    let names: Vec<&str> = workspaces
        .iter()
        .map(|workspace| {
            repositories
                .iter()
                .find(|(id, _)| id == &workspace.repository_id)
                .map_or("unknown repository", |(_, name)| name.as_str())
        })
        .collect();
    let several = names.iter().collect::<HashSet<_>>().len() > 1;
    names
        .into_iter()
        .map(|name| if several { name.into() } else { String::new() })
        .collect()
}

/// A table cell's text and its style.
pub type Cell = (String, Option<Style>);

/// Rows padded into columns two spaces apart. Columns empty in every row are
/// dropped, and each row ends at its last non-empty cell.
pub fn aligned<const N: usize>(rows: Vec<[Cell; N]>, palette: Palette) -> Vec<String> {
    let widths: Vec<usize> = (0..N)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].0.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.into_iter()
        .map(|row| {
            let last = row.iter().rposition(|(text, _)| !text.is_empty());
            let mut line = String::new();
            let mut first = true;
            for (column, (text, style)) in row.into_iter().enumerate() {
                if widths[column] == 0 || Some(column) > last {
                    continue;
                }
                if !std::mem::take(&mut first) {
                    line.push_str("  ");
                }
                let padding = if Some(column) == last {
                    0
                } else {
                    widths[column] - text.chars().count()
                };
                line.push_str(&match style {
                    Some(style) if !text.is_empty() => palette.paint(style, text),
                    _ => text,
                });
                line.push_str(&" ".repeat(padding));
            }
            line
        })
        .collect()
}

pub fn repository_label(repo: &Repository, palette: Palette) -> String {
    format!(
        "{}  {}",
        palette.paint(Style::Heading, crate::forge::repository::name(repo)),
        palette.paint(Style::Muted, &repo.source)
    )
}

/// Picker entries that stay unambiguous when repositories share a name.
pub async fn repository_choices(mut repos: Vec<Repository>) -> Result<Entries> {
    for repo in &mut repos {
        if let Some(url) = crate::forge::repository::remote_url_from_source(&repo.source).await? {
            repo.source = url;
        }
    }
    let labels: Vec<_> = repos
        .iter()
        .map(|repo| {
            let name = crate::forge::repository::name(repo);
            let shared = repos
                .iter()
                .filter(|r| crate::forge::repository::name(r) == name)
                .count()
                > 1;
            if !shared {
                return name.to_owned();
            }
            match crate::forge::repository::host(&repo.source) {
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

/// Preserve the argument and resolve existing paths in the caller's directory.
pub fn repository_selector(value: String) -> Result<crate::forge::repository::Selector> {
    if Path::new(&value).exists() {
        let path = std::fs::canonicalize(&value)?;
        path.to_str().context("repository path is not UTF-8")?;
        Ok(crate::forge::repository::Selector::Path { value, path })
    } else {
        Ok(value.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(repository: &str, name: &str, branch: &str, state: WorkspaceState) -> Workspace {
        Workspace::new_record(
            repository.into(),
            name.into(),
            format!("/work/{name}").into(),
            branch.into(),
            state,
        )
    }

    #[test]
    fn review_rows_list_each_mark_with_its_item_and_staleness() {
        let plain = Palette::stdout(true);
        let mark = |kind, url: Option<&str>, stale| ReviewMark {
            kind,
            url: url.map(Into::into),
            head: "abc".into(),
            created_at: 0,
            stale,
        };
        let mut linked = workspace("a", "fix-login", "fix-login", WorkspaceState::Ready);
        linked.review = vec![
            mark(
                Some(ItemKind::Pr),
                Some("https://example.com/o/r/pulls/12"),
                Some(true),
            ),
            mark(
                Some(ItemKind::Issue),
                Some("https://example.com/o/r/issues/3"),
                Some(false),
            ),
        ];
        let mut unlinked = workspace("b", "docs", "docs", WorkspaceState::Ready);
        unlinked.review = vec![mark(None, None, None)];
        let repositories = [("a".into(), "shoal".into()), ("b".into(), "app".into())];
        assert_eq!(
            review_rows(&[linked, unlinked], &repositories, plain),
            [
                "fix-login  shoal  PR #12     https://example.com/o/r/pulls/12  outdated",
                "fix-login  shoal  issue #3   https://example.com/o/r/issues/3",
                "docs       app    workspace",
            ]
        );
    }

    #[test]
    fn workspace_rows_align_status_and_titles() {
        let plain = Palette::stdout(true);
        let mut workspaces = [
            workspace("a", "fix-login", "fix-login", WorkspaceState::Ready),
            workspace("a", "x", "feature/x", WorkspaceState::Failed),
            workspace("a", "new", "new", WorkspaceState::Preparing),
            workspace("a", "busy", "busy", WorkspaceState::Ready),
            workspace("a", "review", "review", WorkspaceState::Ready),
            workspace("a", "quiet", "quiet", WorkspaceState::Ready),
        ];
        workspaces[3].running = true;
        workspaces[3].links.issue_title = Some("Refine the list view".into());
        let mark = |stale| crate::model::ReviewMark {
            kind: None,
            url: None,
            head: "head".into(),
            created_at: 0,
            stale: Some(stale),
        };
        workspaces[4].running = true;
        workspaces[4].review = vec![mark(false)];
        workspaces[5].review = vec![mark(true)];
        assert_eq!(
            workspace_rows(
                &workspaces,
                &[("a".into(), "shoal".into())],
                &HashSet::from([workspaces[0].id.clone(), workspaces[1].id.clone()]),
                plain
            ),
            [
                "● fix-login  stopped",
                "✗ x          failed",
                "◌ new        preparing",
                "● busy       running                      Refine the list view",
                "● review     ready for review",
                "● quiet      ready for review (outdated)",
            ]
        );
        let repositories = [("a".into(), "shoal".into()), ("b".into(), "app".into())];
        let workspaces = [
            workspace("a", "fix-login", "fix-login", WorkspaceState::Ready),
            workspace("b", "y", "y", WorkspaceState::Ready),
        ];
        assert_eq!(
            workspace_rows(&workspaces, &repositories, &HashSet::new(), plain),
            ["● fix-login  shoal  idle", "● y          app    idle"]
        );
    }

    #[test]
    fn yes_no_questions_default_to_no_and_repeat_unknown_answers() {
        for (answers, expected) in [("\n", false), ("", false), ("maybe\nYes\n", true)] {
            let mut output = Vec::new();
            let answer = ask_yes_no("Register?", &mut answers.as_bytes(), &mut output).unwrap();
            assert_eq!(answer, expected, "{answers:?}");
        }
        let mut output = Vec::new();
        ask_yes_no("Register?", &mut "maybe\nn\n".as_bytes(), &mut output).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "Register? [y/N] Please enter y or n.\nRegister? [y/N] "
        );
    }
}
