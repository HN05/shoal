//! Configured command definitions, their validation and placeholder substitution.
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
};

use anyhow::{Context as _, Result, ensure};
use shoal_refs::{BranchName, CommitId};

use crate::{model::Workspace, placeholders::render_os as render};

pub type Commands = BTreeMap<String, Vec<String>>;

pub fn defaults() -> Commands {
    crate::ai::default_commands()
}

pub fn validate(commands: &Commands) -> Result<()> {
    for (name, argv) in commands {
        crate::validate::lowercase_name("command", name)?;
        ensure!(
            argv.first()
                .is_some_and(|program| !program.trim().is_empty())
                && argv.iter().all(|arg| !arg.contains('\0')),
            "command {name:?} needs a nonempty executable and arguments without NUL bytes"
        );
        ensure!(
            argv[0] != "{args}" && argv.iter().filter(|arg| *arg == "{args}").count() <= 1,
            "command {name:?} may contain {{args}} once, after its executable"
        );
    }
    Ok(())
}

/// Whether `argv` needs the workspace's diff base, which only the daemon knows.
pub fn uses_diff_base(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg.contains("{diff_base}"))
}

/// The placeholders only a workspace has values for; `{path}` names a
/// directory anywhere.
const WORKSPACE_ONLY: &[&str] = &["{workspace}", "{branch}", "{diff_base}"];

/// The first placeholder in `argv` that has no value outside a workspace.
pub fn workspace_only_placeholder(argv: &[String]) -> Option<&'static str> {
    WORKSPACE_ONLY
        .iter()
        .copied()
        .find(|key| argv.iter().any(|arg| arg.contains(key)))
}

/// The placeholders every workspace command may use; `{diff_base}` only when
/// `diff_base` is known.
pub fn workspace_fields<'a>(
    workspace: &'a Workspace,
    diff_base: Option<&'a CommitId>,
) -> Vec<(&'static str, &'a OsStr)> {
    let mut fields = vec![
        ("{workspace}", OsStr::new(workspace.directory_name())),
        (
            "{branch}",
            OsStr::new(
                workspace
                    .branch()
                    .map(BranchName::as_str)
                    .unwrap_or_default(),
            ),
        ),
        ("{path}", workspace.path.as_os_str()),
    ];
    if let Some(base) = diff_base {
        fields.push(("{diff_base}", base.as_ref()));
    }
    fields
}

/// `name`'s command for `workspace`, with `extra_fields` beside the
/// workspace's own placeholders, `{diff_base}` among them when `diff_base`
/// is known.
pub fn expand_with_fields(
    commands: &Commands,
    name: &str,
    workspace: &Workspace,
    args: Vec<OsString>,
    extra_fields: &[(&str, &OsStr)],
    diff_base: Option<&CommitId>,
) -> Result<Vec<OsString>> {
    let argv = commands
        .get(name)
        .with_context(|| format!("unknown command {name:?}"))?;
    let mut fields = workspace_fields(workspace, diff_base);
    fields.extend_from_slice(extra_fields);
    Ok(substitute(argv, &fields, args))
}

/// `name`'s command `argv` outside a workspace, with `fields`, refusing one
/// that uses a placeholder only a workspace has a value for.
pub fn substitute_outside_workspace(
    name: &str,
    argv: &[String],
    fields: &[(&str, &OsStr)],
    args: Vec<OsString>,
) -> Result<Vec<OsString>> {
    if let Some(placeholder) = workspace_only_placeholder(argv) {
        anyhow::bail!(
            "the {name} command uses {placeholder}, which has no value outside a workspace"
        );
    }
    Ok(substitute(argv, fields, args))
}

/// Render each argument once, putting `args` at `{args}` or after the command.
pub fn substitute(
    argv: &[String],
    fields: &[(&str, &OsStr)],
    args: Vec<OsString>,
) -> Vec<OsString> {
    let mut command = Vec::new();
    let mut args = Some(args);
    for arg in argv {
        if arg == "{args}" {
            command.extend(args.take().unwrap_or_default());
        } else {
            command.push(render(arg, fields));
        }
    }
    command.extend(args.unwrap_or_default());
    command
}

#[cfg(test)]
mod tests;
