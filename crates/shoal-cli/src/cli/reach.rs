//! Which commands act on another joined machine's space they are given,
//! so their pickers offer the other machines' spaces beside this one's.

/// Whether the command at `path`, such as `["stop"]`, with the flags of
/// its `matches`, acts on a space of another joined machine: its requests
/// go to that machine through the hub, where `Operation::forwarding` lets
/// them, and it passes the space it picked on to them. A stop that
/// interrupts a turn or stops processes acts on this machine's, as does a
/// view of a local review, which stays on the machine that ran it, and a
/// command that attaches files, which another machine cannot read.
pub fn reaches_elsewhere(path: &[&str], matches: &clap::ArgMatches) -> bool {
    let flag = |name| matches.try_get_one::<bool>(name).ok().flatten() == Some(&true);
    let attaches = (matches.try_get_many::<String>("attach").ok().flatten())
        .is_some_and(|mut files| files.next().is_some());
    if attaches {
        return false;
    }
    match path {
        ["view"] => !names_review(matches),
        ["stop"] => !["interrupt", "processes", "leftovers"]
            .into_iter()
            .any(flag),
        [
            "attach" | "diff" | "conflicts" | "restart" | "rename" | "progress" | "sleep"
            | "priority" | "done" | "rm" | "send",
        ]
        | ["link" | "unlink", ..]
        | ["acquire" | "release", "hold"] => true,
        _ => false,
    }
}

/// Whether `matches` select a local review as the item kind.
fn names_review(matches: &clap::ArgMatches) -> bool {
    (matches.try_get_raw("kind_or_url").ok().flatten())
        .is_some_and(|mut words| words.next().is_some_and(|word| word == "review"))
}

#[cfg(test)]
mod tests;
