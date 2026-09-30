//! Root help groups existing command definitions without changing their syntax.
use std::sync::OnceLock;

use clap::Subcommand;

const GROUPS: &[(&str, &[&str])] = &[
    (
        "Workspaces",
        &[
            "add", "issue", "ls", "cd", "status", "setup", "adopt", "done", "rm",
        ],
    ),
    (
        "Run commands and agents",
        &[
            "exec", "run", "claude", "codex", "happy", "t3", "stop", "pause", "resume",
        ],
    ),
    (
        "Changes and review",
        &["diff", "review", "merge", "land", "pr"],
    ),
    ("Shared resources", &["port", "sim", "resource", "access"]),
    (
        "Diagnostics and activity",
        &["inspect", "notifications", "doctor"],
    ),
    (
        "Configuration and installation",
        &[
            "repo",
            "config",
            "install",
            "daemon",
            "shell",
            "completions",
            "skill",
            "help",
        ],
    ),
];

pub(super) fn template() -> &'static str {
    static TEMPLATE: OnceLock<String> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        // Build only the subcommands: constructing Cli here would recurse.
        let mut catalog = super::Command::augment_subcommands(clap::Command::new("shoal"));
        catalog.build();
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

fn render_group(catalog: &clap::Command, heading: &str, names: &[&str]) -> String {
    let commands = names.iter().enumerate().map(|(order, name)| {
        catalog
            .find_subcommand(name)
            .expect("help group names an existing command")
            .clone()
            .display_order(order)
    });
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
mod tests {
    use clap::CommandFactory;

    #[test]
    fn grouped_help_shows_every_public_command_once_and_no_internal_workers() {
        let mut command = crate::cli::Cli::command();
        let help = command.render_help().to_string();
        let listed: Vec<_> = help
            .lines()
            .filter(|line| line.starts_with("  ") && !line.starts_with("  shoal "))
            .filter_map(|line| line.split_whitespace().next())
            .filter(|name| !name.starts_with('-'))
            .collect();
        let expected: Vec<_> = command
            .get_subcommands()
            .filter(|command| !command.is_hide_set())
            .map(|command| command.get_name())
            .collect();
        assert_eq!(listed.len(), expected.len(), "{help}");
        for name in expected {
            assert_eq!(listed.iter().filter(|listed| **listed == name).count(), 1);
        }
        assert!(help.find("Workspaces:").unwrap() < help.find("Options:").unwrap());
    }
}
