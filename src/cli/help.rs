//! Root help groups existing command definitions without changing their syntax.
use std::sync::OnceLock;

use clap::Subcommand;

const GROUPS: &[(&str, &[&str])] = &[
    (
        "Workspaces",
        &[
            "add",
            "contribute",
            "split",
            "inbox",
            "work",
            "cd",
            "edit",
            "status",
            "messages",
            "questions",
            "approvals",
            "done",
            "rm",
            "cleanup",
            "export",
            "import",
        ],
    ),
    (
        "Run commands and agents",
        &[
            "run", "claude", "codex", "happy", "t3", "chat", "plan", "attach", "demo", "stop",
            "restart", "resume", "queue",
        ],
    ),
    (
        "Changes and review",
        &[
            "branch",
            "diff",
            "conflicts",
            "review",
            "land",
            "forge",
            "pr",
            "issue",
            "ci",
            "link",
            "unlink",
            "view",
            "watch",
            "ready",
        ],
    ),
    (
        "Shared resources",
        &["acquire", "release", "leases", "catalog", "history"],
    ),
    ("Activity", &["alerts"]),
    (
        "Configuration and installation",
        &["repo", "config", "update", "daemon", "shell", "skill"],
    ),
];

pub(super) fn template() -> &'static str {
    static TEMPLATE: OnceLock<String> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        // Build only the subcommands: constructing Cli here would recurse.
        let mut catalog = super::Command::augment_subcommands(clap::Command::new("shoal"));
        // Build the root alone, which adds its `help` command; `build` would
        // also build every command's deferred arguments, which the listing
        // does not show.
        let _ = catalog.render_usage();
        let mut template =
            String::from("{before-help}{about-with-newline}\n{usage-heading} {usage}\n\n");
        for (heading, names) in GROUPS {
            template.push_str(&render_group(&catalog, heading, names));
            template.push('\n');
        }
        let header = catalog.get_styles().get_header();
        template.push_str(&format!(
            "{header}Options:{header:#}\n{{options}}{{after-help}}"
        ));
        template
    })
}

/// A command this build leaves out, such as an optional integration's, is
/// skipped; the tests check that every built command is grouped.
fn render_group(catalog: &clap::Command, heading: &str, names: &[&str]) -> String {
    let commands = names
        .iter()
        .filter_map(|name| catalog.find_subcommand(name))
        .enumerate()
        .map(|(order, command)| command.clone().display_order(order));
    clap::Command::new("shoal")
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .subcommand_help_heading(heading.to_owned())
        .subcommands(commands)
        .help_template("{all-args}")
        .render_help()
        .ansi()
        .to_string()
}

#[cfg(test)]
mod tests;
