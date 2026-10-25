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

use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    cli::{
        WorkspaceScope,
        availability::{Availability, Fit},
        client,
        context::Context,
        list_status::ListStatus,
        output::{Palette, Style},
        recent,
        workspace_context::{ScopeOrder, WorkspaceContext},
    },
    execution::recovery,
    forge::items::link::{self, ItemKind},
    model::{Chat, QueueEntry, Repository, ReviewMark, Workspace},
    paths::Paths,
    state::{AgentState, QueueKind},
};

mod several;

pub use several::pick_many;

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

#[derive(Debug, PartialEq, Eq)]
pub enum ModifiedSkillChoice {
    Keep,
    Replace,
    Diff,
}

/// Ask what to do with an installed skill the user changed, as package
/// managers do for modified configuration files.
pub fn modified_skill_choice(name: &str, path: &Path) -> Result<ModifiedSkillChoice> {
    modified_skill_with_io(
        name,
        path,
        &mut io::stdin().lock(),
        &mut io::stderr().lock(),
    )
}

fn modified_skill_with_io(
    name: &str,
    path: &Path,
    input: &mut impl io::BufRead,
    output: &mut impl Write,
) -> Result<ModifiedSkillChoice> {
    loop {
        write!(
            output,
            "Modified skill {name} at {}: [k]eep, [r]eplace, or show [d]iff [k]: ",
            path.display()
        )?;
        output.flush()?;
        match read_answer(input)?.as_deref() {
            None | Some("" | "k" | "keep") => return Ok(ModifiedSkillChoice::Keep),
            Some("r" | "replace") => return Ok(ModifiedSkillChoice::Replace),
            Some("d" | "diff") => return Ok(ModifiedSkillChoice::Diff),
            Some(_) => writeln!(output, "Please enter k, r, or d.")?,
        }
    }
}

pub fn input(ctx: &Context, prompt: &str) -> Result<String> {
    input_or_cancel(ctx, prompt)?.context("canceled: no value supplied")
}

/// Read a prompt where empty input or EOF means cancellation.
pub fn input_or_cancel(ctx: &Context, prompt: &str) -> Result<Option<String>> {
    Ok(read_input(ctx, prompt)?.filter(|value| !value.is_empty()))
}

/// Read a line, preserving blank input; EOF means cancellation.
pub fn read_input(ctx: &Context, prompt: &str) -> Result<Option<String>> {
    require_interactive(ctx)?;
    eprint!(
        "{} ",
        Palette::stderr(ctx.json).paint(Style::Heading, format_args!("{prompt}:"))
    );
    io::stderr().flush()?;
    let _cancel = InterruptCancels::install();
    let mut value = String::new();
    if io::stdin().read_line(&mut value)? == 0 {
        return Ok(None);
    }
    Ok(Some(value.trim_end_matches(['\r', '\n']).to_owned()))
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

/// A picker whose keys are bound per view, and which Tab can switch to
/// another view by running `next`.
pub struct ViewPicker<'a> {
    pub prompt: &'a str,
    pub header: &'a str,
    /// Every key a view binds, by fzf name, `enter` included.
    pub keys: &'a [&'a str],
    /// The keys the first view leaves unbound.
    pub unbound: &'a [&'a str],
    /// The command whose output, fzf actions, shows the next view.
    pub next: Option<String>,
    /// The search the picker starts with.
    pub query: Option<String>,
}

/// Choose an entry with one of `picker.keys`, returning the key pressed.
/// The first view may be empty, and the entry may come from a view Tab
/// loaded, so the caller checks the id.
pub fn pick_in_views(
    ctx: &Context,
    entries: Entries,
    picker: ViewPicker<'_>,
) -> Result<Picked<String>> {
    let mut args = vec!["--header".into(), picker.header.into()];
    for key in picker.keys {
        args.extend(["--bind".into(), format!("{key}:print({key})+accept")]);
    }
    if !picker.unbound.is_empty() {
        let keys = picker.unbound.join(",");
        args.extend(["--bind".into(), format!("start:unbind({keys})")]);
    }
    if let Some(next) = picker.next {
        args.extend(["--bind".into(), format!("tab:transform({next})")]);
    }
    if let Some(query) = picker.query {
        args.extend(["--query".into(), query]);
    }
    let output = fzf_output(ctx, picker.prompt, &entries, &args)?;
    let (key, selected) = output.split_once('\n').context("picker omitted action")?;
    Ok(Picked {
        action: key.to_owned(),
        id: entry_id(selected),
    })
}

/// The width picker rows should fit: the terminal's, less fzf's pointer
/// and scrollbar columns.
pub fn picker_width() -> Option<usize> {
    let terminal = std::fs::File::open("/dev/tty").ok()?;
    Some(crate::cli::output::terminal_width(terminal)?.saturating_sub(3))
}

/// `items` joined by `separator` into lines no wider than `width`, where
/// one item fits; no limit keeps one line.
pub fn wrapped(items: &[String], separator: &str, width: Option<usize>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for item in items {
        match lines.last_mut() {
            Some(line)
                if width.is_none_or(|width| {
                    line.chars().count() + separator.len() + item.chars().count() <= width
                }) =>
            {
                line.push_str(separator);
                line.push_str(item);
            }
            _ => lines.push(item.clone()),
        }
    }
    lines.join("\n")
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
fn picker_output(
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
fn fzf_output(ctx: &Context, prompt: &str, entries: &Entries, extra: &[String]) -> Result<String> {
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
fn entry_id(line: &str) -> String {
    line.split('\t')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// How to choose a workspace when the command line names none.
#[derive(Clone, Copy)]
pub enum Fallback {
    /// Use the worktree containing the current directory, else open the picker.
    CurrentDirectory,
    /// Use only the current workspace; never open a picker.
    CurrentDirectoryOnly,
    /// Use the workspace the caller's scope belongs to, else the current
    /// workspace; never open a picker. Agent hooks report for the agent's own
    /// workspace wherever its shell has moved, such as into a child workspace.
    OwnWorkspace,
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
    let workspace = match resolve_workspace(ctx, explicit, fallback).await? {
        Resolution::Workspace(id) => id,
        Resolution::Picker(workspaces) => pick_workspace(ctx, workspaces).await?,
    };
    recent::record(ctx, &workspace).await;
    Ok(workspace)
}

/// The workspace an explicit argument names, where `-` is the previous one.
pub async fn explicit_workspace(ctx: &Context, selector: String) -> Result<String> {
    if selector == recent::PREVIOUS {
        recent::previous(ctx).await
    } else {
        Ok(selector)
    }
}

/// `Some(workspace)` unless `--all` was passed or picked; the picker offers
/// every workspace after the individual ones.
pub async fn select_workspace_filter(
    ctx: &Context,
    scope: WorkspaceScope,
) -> Result<Option<String>> {
    if scope.all {
        return Ok(None);
    }
    match resolve_workspace(ctx, scope.workspace, Fallback::CurrentDirectory).await? {
        Resolution::Workspace(id) => Ok(Some(id)),
        Resolution::Picker(workspaces) => pick_workspace_or_all(ctx, workspaces).await,
    }
}

/// Like `select_workspace_filter`, but the current workspace counts only with
/// something to resume, and the picker lists what each workspace has to
/// resume. Without anything to resume anywhere it selects all.
pub async fn select_stopped_workspace(
    ctx: &Context,
    scope: WorkspaceScope,
) -> Result<Option<String>> {
    if scope.all {
        return Ok(None);
    }
    let explicit = scope.workspace.is_some();
    let workspaces =
        match resolve_workspace(ctx, scope.workspace, Fallback::CurrentDirectory).await? {
            Resolution::Workspace(id) if explicit || resumable(&ctx.paths, &id).is_some() => {
                return Ok(Some(id));
            }
            Resolution::Workspace(_) => client::workspaces(&ctx.paths).await?,
            Resolution::Picker(workspaces) => workspaces,
        };
    pick_resume_workspace(ctx, workspaces)
}

/// Workspaces with stopped work come first, then those with a last session;
/// `None` picks all, or nothing is resumable.
fn pick_resume_workspace(ctx: &Context, workspaces: Vec<Workspace>) -> Result<Option<String>> {
    let mut rows = workspaces
        .into_iter()
        .map(|w| (resumable(&ctx.paths, &w.id), w))
        .collect::<Vec<_>>();
    if rows.iter().all(|(resumable, _)| resumable.is_none()) {
        return Ok(None);
    }
    rows.sort_by_key(|(resumable, _)| match resumable {
        Some((_, true)) => 0,
        Some((_, false)) => 1,
        None => 2,
    });
    let (statuses, workspaces): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .map(|(resumable, w)| {
            let stopped = matches!(resumable, Some((_, true)));
            let status = ListStatus::of_workspace(&w, stopped);
            ((status, resume_status_cell(resumable)), w)
        })
        .unzip();
    let labels = rows_with_status(
        &workspaces,
        &[],
        statuses,
        Palette::stderr(ctx.json),
        picker_width(),
    );
    let mut entries: Entries = workspaces.into_iter().map(|w| w.id).zip(labels).collect();
    entries.push((ALL_ID.to_owned(), ALL_WORKSPACES.to_owned()));
    let id = pick(ctx, "Workspace> ", entries)?;
    Ok((id != ALL_ID).then_some(id))
}

/// What `shoal resume` restores in a workspace, such as `stopped: claude,
/// 1 command` or `exited: claude`, and whether it is stopped work; `None`
/// when nothing is saved.
pub fn resumable(paths: &Paths, workspace_id: &str) -> Option<(String, bool)> {
    let saved = recovery::Saved::load(paths, workspace_id).unwrap_or_default();
    if saved.is_empty() {
        let last = recovery::LastSession::load(paths, workspace_id).ok()??;
        return Some((format!("exited: {}", last.agent), false));
    }
    let mut parts = saved
        .agents
        .iter()
        .map(|(_, path)| match recovery::Record::load(path) {
            Ok(Some(record)) => record.agent,
            _ => "agent".to_owned(),
        })
        .collect::<Vec<_>>();
    match saved.commands.len() {
        0 => {}
        1 => parts.push("1 command".into()),
        n => parts.push(format!("{n} commands")),
    }
    Some((format!("stopped: {}", parts.join(", ")), true))
}

/// How the resume picker and completion name a workspace without stopped work.
pub const NOTHING_TO_RESUME: &str = "nothing to resume";

fn resume_status_cell(resumable: Option<(String, bool)>) -> Cell {
    match resumable {
        Some((text, true)) => (text, Some(ListStatus::Stopped.style())),
        Some((text, false)) => (text, None),
        None => (NOTHING_TO_RESUME.into(), Some(Style::Muted)),
    }
}

async fn pick_workspace_or_all(
    ctx: &Context,
    workspaces: Vec<Workspace>,
) -> Result<Option<String>> {
    let mut entries = workspace_entries(ctx, workspaces).await;
    entries.push((ALL_ID.to_owned(), ALL_WORKSPACES.to_owned()));
    let id = pick(ctx, "Workspace> ", entries)?;
    Ok((id != ALL_ID).then_some(id))
}

/// How pickers and completion name `--all`.
pub const ALL_WORKSPACES: &str = "All workspaces";
/// The picker ID of the entry that stands for `--all`; no workspace has it.
const ALL_ID: &str = "--all";

enum Resolution {
    Workspace(String),
    /// No workspace is explicit or current; the user picks among these.
    Picker(Vec<Workspace>),
}

async fn resolve_workspace(
    ctx: &Context,
    explicit: Option<String>,
    fallback: Fallback,
) -> Result<Resolution> {
    if let Some(selector) = explicit {
        return Ok(Resolution::Workspace(
            explicit_workspace(ctx, selector).await?,
        ));
    }
    // A scoped caller lists its own workspace first, then the workspaces
    // split from it, which the current directory selects.
    let workspaces = client::workspaces(&ctx.paths).await?;
    let scoped = crate::env::is_scoped();
    let cwd = current_directory()?;
    let context = WorkspaceContext::from_directory(&workspaces, cwd.as_deref());
    let order = match fallback {
        Fallback::OwnWorkspace => ScopeOrder::BeforeDirectory,
        Fallback::CurrentDirectory | Fallback::CurrentDirectoryOnly => ScopeOrder::AfterDirectory,
    };
    if let Some(workspace) = context.resolve(None, scoped, order) {
        return Ok(Resolution::Workspace(workspace.id.clone()));
    }
    ensure!(!scoped, "scoped workspace is unavailable");
    if matches!(
        fallback,
        Fallback::CurrentDirectoryOnly | Fallback::OwnWorkspace
    ) {
        return Err(NoCurrentWorkspace.into());
    }
    Ok(Resolution::Picker(workspaces))
}

/// The workspace containing the current directory, for an unscoped caller.
pub fn current_workspace(workspaces: &[Workspace]) -> Result<Option<&Workspace>> {
    if crate::env::is_scoped() {
        return Ok(None);
    }
    let cwd = current_directory()?;
    let context = WorkspaceContext::from_directory(workspaces, cwd.as_deref());
    Ok(context.resolve(None, false, ScopeOrder::BeforeDirectory))
}

/// Always run the picker, including for scoped callers (whose list is filtered).
pub async fn workspace_picker(ctx: &Context) -> Result<String> {
    pick_workspace(ctx, client::workspaces(&ctx.paths).await?).await
}

pub async fn pick_workspace(ctx: &Context, workspaces: Vec<Workspace>) -> Result<String> {
    let entries = workspace_entries(ctx, workspaces).await;
    ensure!(!entries.is_empty(), "{}", ctx.need.none_offered());
    pick(ctx, "Workspace> ", entries)
}

/// The picker entries for `workspaces`, judged against what the command needs.
async fn workspace_entries(ctx: &Context, workspaces: Vec<Workspace>) -> Entries {
    let availability = Availability::load(ctx.need, &ctx.paths, &workspaces).await;
    let stopped = stopped_workspaces(&ctx.paths, &workspaces);
    availability_entries(
        workspaces,
        &availability,
        &stopped,
        Palette::stderr(ctx.json),
    )
}

/// Workspaces the command would act on come first; those it would refuse
/// follow with the reason in place of their status, and those without what
/// it acts on are left out.
fn availability_entries(
    workspaces: Vec<Workspace>,
    availability: &Availability,
    stopped: &HashSet<String>,
    palette: Palette,
) -> Entries {
    let mut rows = workspaces
        .into_iter()
        .filter_map(|w| match availability.fit(&w) {
            Fit::Accepted => Some((None, w)),
            Fit::Refused(reason) => Some((Some(reason), w)),
            Fit::Hidden => None,
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|(refusal, _)| refusal.is_some());
    let (statuses, workspaces): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .map(|(refusal, w)| {
            let status = ListStatus::of_workspace(&w, stopped.contains(&w.id));
            let cell = match refusal {
                Some(refusal) => (refusal, Some(Style::Muted)),
                None => status_cell(status),
            };
            ((status, cell), w)
        })
        .unzip();
    let labels = rows_with_status(&workspaces, &[], statuses, palette, picker_width());
    workspaces.into_iter().map(|w| w.id).zip(labels).collect()
}

/// Workspaces with agents or commands saved by `shoal stop` for `shoal resume`.
pub fn stopped_workspaces(paths: &Paths, workspaces: &[Workspace]) -> HashSet<String> {
    workspaces
        .iter()
        .filter(|w| recovery::pending(paths, &w.id).unwrap_or(false))
        .map(|w| w.id.clone())
        .collect()
}

/// How lists and status name an agent's reported turn state.
pub fn agent_state_label(state: AgentState) -> &'static str {
    ListStatus::agent(state).label()
}

fn status_cell(status: ListStatus) -> Cell {
    (status.label().into(), Some(status.style()))
}

/// One aligned row per workspace: a status marker, the name, the repository
/// when the rows span several (`(id, name)` pairs), the status, and the linked
/// issue's title when known.
pub fn workspace_rows(
    workspaces: &[Workspace],
    repositories: &[(String, String)],
    stopped: &HashSet<String>,
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let statuses = workspaces
        .iter()
        .map(|w| {
            let status = ListStatus::of_workspace(w, stopped.contains(&w.id));
            (status, status_cell(status))
        })
        .collect();
    rows_with_status(workspaces, repositories, statuses, palette, width)
}

/// `workspace_rows` for workspaces and the running chats that opened some
/// of them: the user's own workspaces, then each chat followed by its
/// workspaces, indented. Child workspaces follow their parent, indented
/// below it. A chat row names the chat, its status and its agent.
pub fn workspace_tree_rows(
    workspaces: &[Workspace],
    chats: &[Chat],
    repositories: &[Repository],
    stopped: &HashSet<String>,
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let tree = WorkspaceTree {
        workspaces,
        stopped,
        palette,
    };
    let opened_by = |w: &Workspace, chat: &Chat| w.chat.as_ref() == Some(&chat.id);
    let roots = tree.roots();
    let mut rows = Vec::new();
    for root in roots
        .iter()
        .filter(|w| !chats.iter().any(|chat| opened_by(w, chat)))
    {
        tree.push(&mut rows, root, 0);
    }
    for chat in chats {
        rows.push(chat_row(chat, repositories, palette));
        for root in roots.iter().filter(|w| opened_by(w, chat)) {
            tree.push(&mut rows, root, 1);
        }
    }
    let (markers, cells): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
    markers
        .into_iter()
        .zip(aligned_to(cells, palette, title_limit(width)))
        .map(|(marker, row)| format!("{marker} {row}"))
        .collect()
}

/// Listed workspaces arranged under their listed parents.
struct WorkspaceTree<'a> {
    workspaces: &'a [Workspace],
    stopped: &'a HashSet<String>,
    palette: Palette,
}

impl<'a> WorkspaceTree<'a> {
    /// The workspaces whose parent is not listed.
    fn roots(&self) -> Vec<&'a Workspace> {
        self.workspaces
            .iter()
            .filter(|w| {
                w.parent_workspace
                    .as_ref()
                    .is_none_or(|parent| !self.workspaces.iter().any(|p| p.id == parent.id))
            })
            .collect()
    }

    /// The row of `workspace` at `depth`, then those of its children below it.
    fn push(&self, rows: &mut Vec<(String, [Cell; 4])>, workspace: &Workspace, depth: usize) {
        rows.push(tree_workspace_row(
            workspace,
            depth,
            self.stopped,
            self.palette,
        ));
        for child in self.workspaces.iter().filter(|w| {
            w.parent_workspace
                .as_ref()
                .is_some_and(|parent| parent.id == workspace.id)
        }) {
            self.push(rows, child, depth + 1);
        }
    }
}

fn tree_workspace_row(
    workspace: &Workspace,
    depth: usize,
    stopped: &HashSet<String>,
    palette: Palette,
) -> (String, [Cell; 4]) {
    let name = match depth {
        0 => workspace.name.clone(),
        depth => format!("{}└ {}", "  ".repeat(depth - 1), workspace.name),
    };
    let status = ListStatus::of_workspace(workspace, stopped.contains(&workspace.id));
    (
        status.marker(palette),
        [
            (name, Some(Style::Heading)),
            (String::new(), None),
            status_cell(status),
            (
                workspace.links.issue_title.clone().unwrap_or_default(),
                None,
            ),
        ],
    )
}

fn chat_row(chat: &Chat, repositories: &[Repository], palette: Palette) -> (String, [Cell; 4]) {
    let status = ListStatus::ProcessRunning;
    (
        palette.paint(Style::Heading, "◆"),
        [
            (chat_label(chat, repositories), Some(Style::Heading)),
            (String::new(), None),
            (status.label().into(), Some(status.style())),
            (chat.agent.clone(), None),
        ],
    )
}

/// How lists name a chat: after its repository, numbered after the first.
pub fn chat_label(chat: &Chat, repositories: &[Repository]) -> String {
    let repository = repositories
        .iter()
        .find(|repo| repo.id == chat.repository_id)
        .map_or("unknown repository", crate::repository::name);
    match chat.number {
        1 => format!("{repository} chat"),
        number => format!("{repository} chat {number}"),
    }
}

/// `workspace_rows` with a status, which picks the marker, and a status cell
/// per workspace.
fn rows_with_status(
    workspaces: &[Workspace],
    repositories: &[(String, String)],
    statuses: Vec<(ListStatus, Cell)>,
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let (markers, statuses): (Vec<_>, Vec<_>) = statuses
        .into_iter()
        .map(|(status, cell)| (status.marker(palette), cell))
        .unzip();
    let cells: Vec<[Cell; 4]> = workspaces
        .iter()
        .zip(repository_column(workspaces, repositories))
        .zip(statuses)
        .map(|((w, repository), status)| {
            [
                (w.name.clone(), Some(Style::Heading)),
                (repository, None),
                status,
                (w.links.issue_title.clone().unwrap_or_default(), None),
            ]
        })
        .collect();
    markers
        .into_iter()
        .zip(aligned_to(cells, palette, title_limit(width)))
        .map(|(marker, row)| format!("{marker} {row}"))
        .collect()
}

/// Rows of `width` with a state marker before their cells cut the title.
fn title_limit(width: Option<usize>) -> Option<WidthLimit> {
    width.map(|width| WidthLimit {
        column: 3,
        width: width.saturating_sub(2),
    })
}

fn queue_status_cell(entry: &QueueEntry) -> Cell {
    let status = ListStatus::of_entry(entry);
    let mut text = match entry.position {
        Some(position) => format!("{}, position {position}", status.label()),
        None => status.label().into(),
    };
    if !entry.waiting_for.is_empty() {
        text += &format!(", after {}", entry.waiting_for.join(", "));
    }
    (text, Some(status.style()))
}

/// What a queue entry starts: a launch's issue title, or a restore.
fn queue_subject(entry: &QueueEntry) -> String {
    match entry.kind {
        QueueKind::Launch => entry.issue_title.clone().unwrap_or_default(),
        QueueKind::Restore => format!("Automatic restore of {}", entry.agent),
    }
}

/// One aligned row per agent queue entry: its workspace name, its repository
/// when the entries span several, its place, and what it starts.
pub fn queue_rows(
    entries: &[QueueEntry],
    repositories: &[Repository],
    palette: Palette,
) -> Vec<String> {
    let names: Vec<&str> = entries
        .iter()
        .map(|entry| {
            repositories
                .iter()
                .find(|repo| repo.id == entry.repository_id)
                .map_or("unknown repository", crate::repository::name)
        })
        .collect();
    let several = names.iter().collect::<HashSet<_>>().len() > 1;
    let cells: Vec<[Cell; 4]> = entries
        .iter()
        .zip(names)
        .map(|(entry, repository)| {
            [
                (entry.name.clone(), Some(Style::Heading)),
                (
                    if several {
                        repository.into()
                    } else {
                        String::new()
                    },
                    None,
                ),
                queue_status_cell(entry),
                (queue_subject(entry), None),
            ]
        })
        .collect();
    entries
        .iter()
        .zip(aligned(cells, palette))
        .map(|(entry, row)| format!("{} {row}", ListStatus::of_entry(entry).marker(palette)))
        .collect()
}

/// `PR #12` or `issue #3` for a linked item, `workspace` for a mark made
/// while nothing was linked.
pub fn mark_label(mark: &ReviewMark) -> String {
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
    repository_cells(
        workspaces.iter().map(|w| w.repository_id.as_str()),
        repositories,
    )
}

/// The name of each repository ID, or empty cells when all name one.
pub fn repository_cells<'a>(
    ids: impl Iterator<Item = &'a str>,
    repositories: &[(String, String)],
) -> Vec<String> {
    let names: Vec<&str> = ids
        .map(|repository| {
            repositories
                .iter()
                .find(|(id, _)| id == repository)
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
    aligned_to(rows, palette, None)
}

/// The rows' width limit, met by cutting the cells of one column, usually
/// free text, so the columns after it stay aligned and visible.
#[derive(Clone, Copy)]
pub struct WidthLimit {
    pub column: usize,
    pub width: usize,
}

/// A fitted column keeps this many characters however narrow the limit.
const MIN_FITTED: usize = 12;

/// `aligned`, cutting `limit`'s column with an ellipsis where rows would
/// exceed its width.
pub fn aligned_to<const N: usize>(
    rows: Vec<[Cell; N]>,
    palette: Palette,
    limit: Option<WidthLimit>,
) -> Vec<String> {
    let mut widths: Vec<usize> = (0..N)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].0.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    if let Some(limit) = limit {
        narrow(&mut widths, limit);
    }
    let rows = rows.into_iter().map(|row| {
        let mut column = 0;
        row.map(|(text, style)| {
            let cell = (truncated(text, widths[column]), style);
            column += 1;
            cell
        })
    });
    rows.map(|row| {
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

/// Narrow `limit.column` by what the row width exceeds `limit.width` by.
fn narrow(widths: &mut [usize], limit: WidthLimit) {
    let shown = widths.iter().filter(|width| **width > 0).count();
    let total = widths.iter().sum::<usize>() + 2 * shown.saturating_sub(1);
    let Some(excess) = total.checked_sub(limit.width) else {
        return;
    };
    let column = &mut widths[limit.column];
    *column = column.saturating_sub(excess).max(MIN_FITTED.min(*column));
}

/// `text` cut to `width` characters, ending in an ellipsis when cut.
fn truncated(text: String, width: usize) -> String {
    if text.chars().count() <= width {
        return text;
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
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
        if let Some(url) = crate::repository::remote_url_from_source(&repo.source).await? {
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

/// Preserve the argument and resolve existing paths in the caller's directory.
pub fn repository_selector(value: String) -> Result<crate::repository::Selector> {
    if Path::new(&value).exists() {
        let path = std::fs::canonicalize(&value)?;
        path.to_str().context("repository path is not UTF-8")?;
        Ok(crate::repository::Selector::Path { value, path })
    } else {
        Ok(value.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::WorkspaceState;

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
    fn queue_status_names_the_work_a_launch_waits_for() {
        let mut entry = QueueEntry {
            id: "entry".into(),
            kind: QueueKind::Launch,
            repository_id: "repo".into(),
            name: "ui".into(),
            agent: "claude".into(),
            issue_title: None,
            state: crate::state::QueueState::Queued,
            position: Some(2),
            created_at: 0,
            workspace_id: None,
            opens: None,
            waiting_for: Vec::new(),
            base: None,
        };
        assert_eq!(queue_status_cell(&entry).0, "queued, position 2");
        entry.waiting_for = vec!["api".into(), "https://forge.example/o/r/issues/7".into()];
        assert_eq!(
            queue_status_cell(&entry).0,
            "queued, position 2, after api, https://forge.example/o/r/issues/7"
        );
    }

    #[test]
    fn tree_rows_list_children_below_their_parent() {
        let plain = Palette::stdout(true);
        let mut workspaces = vec![
            workspace("a", "child", "child", WorkspaceState::Ready),
            workspace("a", "grandchild", "grandchild", WorkspaceState::Ready),
            workspace("a", "orphan", "orphan", WorkspaceState::Ready),
            workspace("a", "parent", "parent", WorkspaceState::Ready),
        ];
        let reference = |w: &Workspace| crate::model::WorkspaceRef {
            id: w.id.clone(),
            name: w.name.clone(),
            branch: w.branch.clone(),
        };
        workspaces[0].parent_workspace = Some(reference(&workspaces[3]));
        workspaces[1].parent_workspace = Some(reference(&workspaces[0]));
        // A parent left out of the list leaves its child at the top level.
        workspaces[2].parent_workspace = Some(crate::model::WorkspaceRef {
            id: "unlisted".into(),
            name: "unlisted".into(),
            branch: "unlisted".into(),
        });
        let rows = workspace_tree_rows(&workspaces, &[], &[], &HashSet::new(), plain, None);
        let expected = ["○ orphan ", "○ parent ", "○ └ child ", "○   └ grandchild "];
        assert_eq!(rows.len(), expected.len());
        for (row, start) in rows.iter().zip(expected) {
            assert!(row.starts_with(start), "{row:?}");
        }
    }

    #[test]
    fn width_limits_cut_one_column_and_keep_the_rest_aligned() {
        let plain = Palette::stdout(true);
        let cell = |text: &str| (text.to_owned(), None);
        let rows = || {
            vec![
                [
                    cell("question"),
                    cell("Which database should the cache use?"),
                    cell("SQLite"),
                ],
                [cell("ready"), cell("PR #12"), cell("")],
            ]
        };
        let limit = |width| Some(WidthLimit { column: 1, width });
        assert_eq!(
            aligned_to(rows(), plain, limit(60)),
            aligned(rows(), plain),
            "rows that fit stay whole"
        );
        assert_eq!(
            aligned_to(rows(), plain, limit(40)),
            [
                "question  Which database should…  SQLite",
                "ready     PR #12",
            ]
        );
        // However narrow the limit, the column keeps a few characters.
        assert_eq!(
            aligned_to(rows(), plain, limit(10))[0],
            "question  Which datab…  SQLite"
        );
    }

    #[test]
    fn wrapped_items_fill_lines_up_to_the_width() {
        let items: Vec<String> = ["enter: cd", "tab: next view", "ctrl-d: delete"]
            .map(String::from)
            .into();
        assert_eq!(
            wrapped(&items, "   ", None),
            "enter: cd   tab: next view   ctrl-d: delete"
        );
        assert_eq!(
            wrapped(&items, "   ", Some(26)),
            "enter: cd   tab: next view\nctrl-d: delete"
        );
        // An item wider than the limit gets a line of its own.
        assert_eq!(
            wrapped(&items, "   ", Some(5)),
            "enter: cd\ntab: next view\nctrl-d: delete"
        );
    }

    #[test]
    fn mark_labels_name_the_marked_item() {
        let mark = |kind, url: Option<&str>| ReviewMark {
            kind,
            url: url.map(Into::into),
            head: "abc".into(),
            created_at: 0,
            stale: None,
            warnings: Vec::new(),
        };
        let pr = mark(Some(ItemKind::Pr), Some("https://example.com/o/r/pulls/12"));
        let issue = mark(
            Some(ItemKind::Issue),
            Some("https://example.com/o/r/issues/3"),
        );
        assert_eq!(mark_label(&pr), "PR #12");
        assert_eq!(mark_label(&issue), "issue #3");
        assert_eq!(mark_label(&mark(None, None)), "workspace");
    }

    #[test]
    fn workspace_rows_align_status_and_titles() {
        let plain = Palette::stdout(true);
        let mut workspaces = vec![
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
            warnings: Vec::new(),
        };
        workspaces[4].running = true;
        workspaces[4].review = vec![mark(false)];
        workspaces[5].review = vec![mark(true)];
        let agent = |state| Some(crate::model::AgentStatus { state, since: 0 });
        // Needing input outranks completion, which outranks a current mark
        // and other turn states.
        workspaces.extend([
            workspace("a", "asking", "asking", WorkspaceState::Ready),
            workspace("a", "marked", "marked", WorkspaceState::Ready),
            workspace("a", "agent", "agent", WorkspaceState::Ready),
            workspace("a", "done", "done", WorkspaceState::Ready),
            workspace("a", "follow-up", "follow-up", WorkspaceState::Ready),
        ]);
        workspaces[6].review = vec![mark(false)];
        workspaces[6].agent_state = agent(AgentState::Waiting);
        workspaces[7].review = vec![mark(false)];
        workspaces[7].agent_state = agent(AgentState::Working);
        workspaces[8].running = true;
        workspaces[8].agent_state = agent(AgentState::Idle);
        workspaces[9].done = true;
        workspaces[9].review = vec![mark(false)];
        workspaces[9].agent_state = agent(AgentState::Idle);
        workspaces[10].done = true;
        workspaces[10].agent_state = agent(AgentState::Waiting);
        assert_eq!(
            workspace_rows(
                &workspaces,
                &[("a".into(), "shoal".into())],
                &HashSet::from([workspaces[0].id.clone(), workspaces[1].id.clone()]),
                plain,
                None,
            ),
            [
                "● fix-login  stopped",
                "✗ x          failed",
                "◌ new        preparing",
                "◐ busy       process running   Refine the list view",
                "● review     ready",
                "○ quiet      ready (outdated)",
                "● asking     needs input",
                "● marked     ready",
                "○ agent      waiting",
                "○ done       finished",
                "● follow-up  needs input",
            ]
        );
        let repositories = [("a".into(), "shoal".into()), ("b".into(), "app".into())];
        let workspaces = [
            workspace("a", "fix-login", "fix-login", WorkspaceState::Ready),
            workspace("b", "y", "y", WorkspaceState::Ready),
        ];
        assert_eq!(
            workspace_rows(&workspaces, &repositories, &HashSet::new(), plain, None),
            [
                "○ fix-login  shoal  inactive",
                "○ y          app    inactive"
            ]
        );
    }

    #[test]
    fn modified_skill_choice_defaults_to_keep_and_repeats_unknown_answers() {
        let path = Path::new("/skills/shoal-worker/SKILL.md");
        for (answers, expected) in [
            ("\n", ModifiedSkillChoice::Keep),
            ("", ModifiedSkillChoice::Keep),
            ("R\n", ModifiedSkillChoice::Replace),
            ("maybe\ndiff\n", ModifiedSkillChoice::Diff),
        ] {
            let mut output = Vec::new();
            let choice =
                modified_skill_with_io("shoal-worker", path, &mut answers.as_bytes(), &mut output)
                    .unwrap();
            assert_eq!(choice, expected, "{answers:?}");
        }
        let mut output = Vec::new();
        modified_skill_with_io("shoal-worker", path, &mut "x\nk\n".as_bytes(), &mut output)
            .unwrap();
        let prompt = "Modified skill shoal-worker at /skills/shoal-worker/SKILL.md: [k]eep, [r]eplace, or show [d]iff [k]: ";
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("{prompt}Please enter k, r, or d.\n{prompt}")
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
