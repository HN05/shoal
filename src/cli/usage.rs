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
    let invocation;
    let mut command = match root.find_subcommand(first) {
        Some(command) => command,
        // A name the root does not define runs a configured command.
        None => {
            invocation = Invocation::command();
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
    by_id.or_else(by_value).map(|arg| shown(arg, argument))
}

fn value_name(arg: &Arg) -> Option<&str> {
    arg.get_value_names()?.first().map(|name| name.as_str())
}

/// `arg` as a command line passes it, its value named in lower case.
fn shown(arg: &Arg, argument: &str) -> String {
    let value = value_name(arg).map_or_else(|| argument.to_owned(), str::to_lowercase);
    match arg.get_long() {
        Some(long) => format!("--{long} <{value}>"),
        None => format!("<{value}>"),
    }
}

#[cfg(test)]
mod tests;
