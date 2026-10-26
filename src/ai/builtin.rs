//! The AI tools Shoal ships, available without configuration.
use crate::config::named_commands::Commands;

/// A provider available without configuration.
pub struct Provider {
    pub name: &'static str,
    pub(super) command: &'static [&'static str],
    pub(super) skill_dir: SkillDir,
    pub(super) instructions: Option<&'static [&'static str]>,
    pub(super) resume: Option<&'static [&'static str]>,
    pub(super) resume_latest: Option<&'static [&'static str]>,
    pub(super) add_dir: Option<&'static [&'static str]>,
}

pub(super) enum SkillDir {
    /// Claude's config directory, which `CLAUDE_CONFIG_DIR` moves.
    Claude,
    Home(&'static str),
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        name: "codex",
        command: &[
            "codex",
            "{args}",
            "--sandbox",
            "danger-full-access",
            "--ask-for-approval=never",
        ],
        skill_dir: SkillDir::Home(".agents/skills"),
        instructions: Some(&["-c", "developer_instructions={instructions_toml}"]),
        resume: Some(&["resume", "{session}"]),
        // Codex's worktrees feature widens `--last` to every linked worktree
        // of the repository; restore this workspace's session.
        resume_latest: Some(&["resume", "--last", "-c", "features.worktrees=false"]),
        add_dir: Some(&["--add-dir", "{directory}"]),
    },
    Provider {
        name: "claude",
        command: &["claude"],
        skill_dir: SkillDir::Claude,
        instructions: Some(&["--append-system-prompt", "{instructions}"]),
        resume: Some(&["--resume", "{session}"]),
        resume_latest: Some(&["--continue"]),
        // `--add-dir` takes every following argument up to the next option,
        // which would include a prompt; the joined form takes one.
        add_dir: Some(&["--add-dir={directory}"]),
    },
    // opencode and pi also read `~/.agents/skills`, so they share Codex's copy.
    Provider {
        name: "opencode",
        command: &["opencode", "--prompt", "{prompt}", "{args}"],
        skill_dir: SkillDir::Home(".agents/skills"),
        instructions: None,
        resume: None,
        resume_latest: None,
        add_dir: None,
    },
    Provider {
        name: "pi",
        command: &["pi"],
        skill_dir: SkillDir::Home(".agents/skills"),
        instructions: None,
        resume: None,
        resume_latest: None,
        add_dir: None,
    },
    Provider {
        name: "grok",
        command: &["grok"],
        skill_dir: SkillDir::Home(".grok/skills"),
        instructions: None,
        resume: None,
        resume_latest: None,
        add_dir: None,
    },
];

/// Built-in launchers, the lowest layer of `[commands]`.
pub fn default_commands() -> Commands {
    PROVIDERS
        .iter()
        .map(|provider| {
            let argv = provider.command.iter().map(|arg| arg.to_string()).collect();
            (provider.name.to_owned(), argv)
        })
        .collect()
}
