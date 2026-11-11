//! The arguments a long-running command hands to the installed release
//! after an upgrade changes the daemon protocol.
use std::ffi::OsString;

/// This command's arguments.
pub(crate) fn arguments() -> Vec<OsString> {
    std::env::args_os().skip(1).collect()
}

/// This command's arguments, with `option` set to any `value` in place of the
/// one it was given.
pub(crate) fn arguments_with(option: &str, value: Option<impl ToString>) -> Vec<OsString> {
    match value {
        Some(value) => replace_option(arguments(), option, &value.to_string()),
        None => arguments(),
    }
}

pub(crate) fn replace_option(
    arguments: impl IntoIterator<Item = OsString>,
    option: &str,
    value: &str,
) -> Vec<OsString> {
    let mut replaced = without_option(arguments, option);
    replaced.extend([option.into(), value.into()]);
    replaced
}

/// `arguments` without `option` and the value it was given, where `option`
/// is a long (`--space`) or short (`-s`) spelling.
pub(crate) fn without_option(
    arguments: impl IntoIterator<Item = OsString>,
    option: &str,
) -> Vec<OsString> {
    // A short option's value may follow it directly, as in `-sapi`.
    let inline = if option.starts_with("--") {
        format!("{option}=")
    } else {
        option.to_owned()
    };
    let mut kept = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if argument == option {
            arguments.next();
        } else if !argument
            .to_str()
            .is_some_and(|argument| argument.starts_with(&inline))
        {
            kept.push(argument);
        }
    }
    kept
}

/// `arguments` with `flag` added unless they already have it.
pub(crate) fn with_flag(mut arguments: Vec<OsString>, flag: &str) -> Vec<OsString> {
    if !arguments.iter().any(|argument| argument == flag) {
        arguments.push(flag.into());
    }
    arguments
}

#[cfg(test)]
mod tests;
