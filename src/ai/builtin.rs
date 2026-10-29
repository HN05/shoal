//! The AI tools Shoal ships, available without configuration.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;

use super::IntegrationKind;
use crate::config::named_commands::Commands;

/// A provider available without configuration.
pub struct Provider {
    pub name: &'static str,
    pub(super) command: &'static [&'static str],
    pub(super) integration: IntegrationKind,
    pub(super) skill_dir: SkillDir,
    pub(super) instructions: Option<&'static [&'static str]>,
    pub(super) resume: Option<&'static [&'static str]>,
    pub(super) resume_latest: Option<&'static [&'static str]>,
    pub(super) add_dir: Option<&'static [&'static str]>,
    /// How long its prompt cache lives after its last request, for a tool
    /// that cannot tell Shoal from its own settings.
    pub(super) prompt_cache: Option<Duration>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum SkillDir {
    /// Claude's config directory, which `CLAUDE_CONFIG_DIR` moves.
    Claude,
    Home(&'static str),
}

impl SkillDir {
    /// The directory for a launch that adds `configured` to this process's
    /// environment.
    pub(super) fn path(
        self,
        configured: &BTreeMap<String, OsString>,
        home: &Path,
    ) -> Result<PathBuf> {
        Ok(match self {
            Self::Claude => crate::env::claude_config_dir_with(configured)?
                .unwrap_or_else(|| home.join(".claude"))
                .join("skills"),
            Self::Home(relative) => home.join(relative),
        })
    }
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        name: "codex",
        integration: IntegrationKind::Codex,
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
        // The least OpenAI keeps a GPT-5.6 or later cache, which each read
        // renews; Codex has no setting for it.
        prompt_cache: Some(Duration::from_secs(30 * 60)),
    },
    Provider {
        name: "claude",
        integration: IntegrationKind::Claude,
        command: &["claude"],
        skill_dir: SkillDir::Claude,
        instructions: Some(&["--append-system-prompt", "{instructions}"]),
        resume: Some(&["--resume", "{session}"]),
        resume_latest: Some(&["--continue"]),
        // `--add-dir` takes every following argument up to the next option,
        // which would include a prompt; the joined form takes one.
        add_dir: Some(&["--add-dir={directory}"]),
        // Its settings decide, which its hooks report.
        prompt_cache: None,
    },
    // opencode and pi also read `~/.agents/skills`, so they share Codex's copy.
    Provider {
        name: "opencode",
        integration: IntegrationKind::None,
        command: &["opencode", "--prompt", "{prompt}", "{args}"],
        skill_dir: SkillDir::Home(".agents/skills"),
        instructions: None,
        resume: None,
        resume_latest: None,
        add_dir: None,
        prompt_cache: None,
    },
    Provider {
        name: "pi",
        integration: IntegrationKind::None,
        command: &["pi"],
        skill_dir: SkillDir::Home(".agents/skills"),
        instructions: None,
        resume: None,
        resume_latest: None,
        add_dir: None,
        prompt_cache: None,
    },
    Provider {
        name: "grok",
        integration: IntegrationKind::None,
        command: &["grok"],
        skill_dir: SkillDir::Home(".grok/skills"),
        instructions: None,
        resume: None,
        resume_latest: None,
        add_dir: None,
        prompt_cache: None,
    },
];

/// The built-in tool named `name`, if Shoal ships one.
pub(super) fn find(name: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|provider| provider.name == name)
}

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
