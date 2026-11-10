use super::*;

fn path(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn arguments_show_as_the_command_line_passes_them() {
    for (command, argument, shown) in [
        (&["start"][..], "repository", Some("<repository>")),
        (&["status"], "space", Some("<target>")),
        (&["run"], "workspace", Some("--workspace <workspace>")),
        (&["attach"], "workspace", Some("<target>")),
        (&["release", "hold"], "name", Some("--name <name>")),
        (&["repo", "rm"], "repository", Some("<repository>")),
        (&["start"], "branch", Some("<repository> <branch>")),
        (&["repo", "rename"], "name", Some("<repository> <name>")),
        (&["status"], "repository", None),
        (&["check"], "workspace", Some("<workspace>")),
        (&["status", "no-such-command"], "workspace", None),
    ] {
        assert_eq!(
            of(&path(command), argument).as_deref(),
            shown,
            "{command:?} {argument}"
        );
    }
}
