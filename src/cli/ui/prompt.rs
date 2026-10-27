//! Prompts on the terminal: confirmations, questions with a few answers,
//! and free text.
use std::{
    io::{self, Write},
    path::Path,
};

use anyhow::{Context as _, Result, ensure};

use crate::cli::{
    context::Context,
    output::{Palette, Style},
};

pub(super) fn require_interactive(ctx: &Context) -> Result<()> {
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

/// Read a value offered as `prompt [default]`, where empty input takes
/// `default` and EOF means cancellation.
pub fn input_with_default(ctx: &Context, prompt: &str, default: &str) -> Result<String> {
    let value = read_input(ctx, &format!("{prompt} [{default}]"))?
        .context("canceled: no value supplied")?;
    Ok(or_default(value, default))
}

/// The typed value, or `default` for blank input.
fn or_default(value: String, default: &str) -> String {
    match value.trim() {
        "" => default.to_owned(),
        value => value.to_owned(),
    }
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

#[cfg(test)]
mod tests;
