//! Which commands act on another joined machine's space they are given,
//! so their pickers offer the other machines' spaces beside this one's.

/// Whether the command at `path`, such as `["stop"]`, with the flags of
/// its `matches`, acts on a space of another joined machine: its requests
/// go to that machine through the hub, where `Operation::forwarding` lets
/// them, and it passes the space it picked on to them. A stop that
/// interrupts a turn or stops processes acts on this machine's.
pub fn reaches_elsewhere(path: &[&str], matches: &clap::ArgMatches) -> bool {
    let flag = |name| matches.try_get_one::<bool>(name).ok().flatten() == Some(&true);
    match path {
        ["stop"] => !["interrupt", "processes", "leftovers"]
            .into_iter()
            .any(flag),
        [
            "attach" | "diff" | "conflicts" | "restart" | "rename" | "progress" | "sleep"
            | "priority" | "done" | "rm" | "send",
        ]
        | ["link" | "unlink" | "view", ..]
        | ["acquire" | "release", "hold"] => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
