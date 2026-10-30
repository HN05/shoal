use super::*;

fn path(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn arguments_show_as_the_command_line_passes_them() {
    for (command, argument, shown) in [
        (&["add"][..], "repository", Some("<repository>")),
        (&["status"], "workspace", Some("<target>")),
        (&["run"], "workspace", Some("--workspace <workspace>")),
        (&["attach"], "workspace", Some("<target>")),
        (&["release", "hold"], "name", Some("--name <name>")),
        (&["repo", "rm"], "repository", Some("<repository>")),
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
