//! How the running command's line supplies one of its arguments, so a
//! refusal to prompt without a terminal can name what to pass instead.
use clap::{Arg, CommandFactory};

use super::{Cli, args::Invocation};

/// How the command at `path`, such as `["repo", "rm"]`, takes `argument`,
/// the ID or value name of one of its arguments: `--workspace <workspace>`
/// for an option, `<repository>` for a positional; `None` when it takes no
/// such argument.
pub fn of(path: &[String], argument: &str) -> Option<String> {
    let mut root = Cli::command();
    // Arguments are deferred until their command is built.
    root.build();
    let (first, rest) = path.split_first()?;
    let mut invocation;
    let mut command = match root.find_subcommand(first) {
        Some(command) => command,
        // A name the root does not define runs a configured command.
        None => {
            invocation = Invocation::command();
            invocation.build();
            &invocation
        }
    };
    for name in rest {
        command = command.find_subcommand(name)?;
    }
    let by_id = command.get_arguments().find(|arg| arg.get_id() == argument);
    let by_value = || {
        command
            .get_arguments()
            .find(|arg| value_name(arg).is_some_and(|name| name.eq_ignore_ascii_case(argument)))
    };
    let arg = by_id.or_else(by_value)?;
    Some(match arg.get_long() {
        Some(long) => format!("--{long} {}", placeholder(arg)),
        None => positionals_through(command, arg),
    })
}

/// The positionals up to and including `arg`, which only follows them on a
/// command line.
fn positionals_through(command: &clap::Command, arg: &Arg) -> String {
    let mut before: Vec<_> = command
        .get_positionals()
        .filter(|positional| positional.get_index() <= arg.get_index())
        .collect();
    before.sort_by_key(|positional| positional.get_index());
    before
        .into_iter()
        .map(placeholder)
        .collect::<Vec<_>>()
        .join(" ")
}

/// `<value>`, named by the argument's value name or ID in lower case.
fn placeholder(arg: &Arg) -> String {
    let value = value_name(arg).map_or_else(|| arg.get_id().to_string(), str::to_lowercase);
    format!("<{value}>")
}

fn value_name(arg: &Arg) -> Option<&str> {
    arg.get_value_names()?.first().map(|name| name.as_str())
}

#[cfg(test)]
mod tests;
