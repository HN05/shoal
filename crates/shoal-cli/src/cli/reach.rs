//! Which commands act on another joined machine's space they are given,
//! so their pickers offer the other machines' spaces beside this one's.

/// Whether the command at `path`, such as `["stop"]`, acts on a space of
/// another joined machine: its requests go to that machine through the
/// hub, where `Operation::forwarding` lets them, and it passes the space it
/// picked on to them.
pub fn reaches_elsewhere(path: &[String]) -> bool {
    let path: Vec<&str> = path.iter().map(String::as_str).collect();
    matches!(
        path.as_slice(),
        ["attach"
            | "diff"
            | "conflicts"
            | "stop"
            | "restart"
            | "rename"
            | "progress"
            | "sleep"
            | "priority"
            | "done"
            | "rm"
            | "send"]
            | ["link" | "unlink" | "view", ..]
            | ["acquire" | "release", "hold"]
    )
}
