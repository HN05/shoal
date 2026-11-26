//! The AI tools Shoal ships, available without configuration.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::Result;

use super::IntegrationKind;
use crate::{
    config::named_commands::Commands,
    reviews::output::{self, ResultFormat},
};

/// A provider available without configuration.
pub struct Provider {
    pub name: &'static str,
    pub(super) command: &'static [&'static str],
    pub(super) integration: IntegrationKind,
    pub(super) skill_dir: SkillDir,
    pub(super) instructions: Option<&'static [&'static str]>,
    pub(super) resume: Option<&'static [&'static str]>,
    pub(super) resume_latest: Option<&'static [&'static str]>,
    pub(super) fork: Option<&'static [&'static str]>,
    pub(super) add_dir: Option<&'static [&'static str]>,
    pub(super) model_args: Option<&'static [&'static str]>,
    pub(super) effort_args: Option<&'static [&'static str]>,
    /// The variable that sets the reasoning effort over the effort
    /// arguments, which a launch with an effort sets to the same value.
    pub(super) effort_variable: Option<&'static str>,
    /// The arguments of a review's one-shot run, or an answer's: read-only,
    /// but able to reach the daemon for Shoal's commands, which write the
    /// review or the answer.
    pub(super) review: Option<&'static [&'static str]>,
    pub(super) review_resume: Option<&'static [&'static str]>,
    /// The arguments of the gate's one-shot run, as `review`'s, that returns
    /// the result its output schema describes.
    pub(super) gate: Option<&'static [&'static str]>,
    pub(super) gate_resume: Option<&'static [&'static str]>,
    pub(super) review_result: &'static ResultFormat,
    /// How long its prompt cache lives after a turn when neither its
    /// configuration nor its agent's own settings say; `None` leaves it to
    /// [`super::DEFAULT_PROMPT_CACHE`].
    pub(super) prompt_cache_minutes: Option<u64>,
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

/// How long the prompt cache of a Codex session lives after its last
/// request, which a resume in a new process reads as well: the least the
/// OpenAI models Codex runs keep it, though some keep it for hours.
const CODEX_PROMPT_CACHE_MINUTES: u64 = 45;

pub const PROVIDERS: &[Provider] = &[
    Provider {
        name: "codex",
        integration: IntegrationKind::Codex,
        command: &["codex", "{args}"],
        skill_dir: SkillDir::Home(".agents/skills"),
        instructions: Some(&["-c", "developer_instructions={instructions_toml}"]),
        resume: Some(&["resume", "{session}"]),
        // Codex's worktrees feature widens `--last` to every linked worktree
        // of the repository; restore this workspace's session.
        resume_latest: Some(&["resume", "--last", "-c", "features.worktrees=false"]),
        // A forked session keeps its directory, or asks for one, unless
        // told where to work.
        fork: Some(&["fork", "{session}", "--cd", "{directory}"]),
        add_dir: Some(&["--add-dir", "{directory}"]),
        model_args: Some(&["-m", "{model}"]),
        effort_args: Some(&["-c", "model_reasoning_effort=\"{effort}\""]),
        effort_variable: None,
        // Its permissions read every file and write none, and its network
        // reaches Unix sockets, the daemon's among them, which Linux cannot
        // allow one by one, but no host: the proxy it goes through allows
        // none. The reviewer writes its review through Shoal's commands.
        review: Some(&[
            "exec",
            "-c",
            "default_permissions=\"shoal_review\"",
            "-c",
            "permissions.shoal_review.filesystem={\":root\"=\"read\"}",
            "-c",
            "permissions.shoal_review.network.enabled=true",
            "-c",
            "permissions.shoal_review.network.dangerously_allow_all_unix_sockets=true",
            "-c",
            "features.network_proxy=true",
            "-c",
            "approval_policy=\"never\"",
            "--output-last-message",
            "{output}",
            "{prompt}",
        ]),
        review_resume: Some(&[
            "exec",
            "resume",
            "-c",
            "default_permissions=\"shoal_review\"",
            "-c",
            "permissions.shoal_review.filesystem={\":root\"=\"read\"}",
            "-c",
            "permissions.shoal_review.network.enabled=true",
            "-c",
            "permissions.shoal_review.network.dangerously_allow_all_unix_sockets=true",
            "-c",
            "features.network_proxy=true",
            "-c",
            "approval_policy=\"never\"",
            "--output-last-message",
            "{output}",
            "{session}",
            "{prompt}",
        ]),
        // As `review`, returning the result its schema describes.
        gate: Some(&[
            "exec",
            "-c",
            "default_permissions=\"shoal_gate\"",
            "-c",
            "permissions.shoal_gate.filesystem={\":root\"=\"read\"}",
            "-c",
            "permissions.shoal_gate.network.enabled=true",
            "-c",
            "permissions.shoal_gate.network.dangerously_allow_all_unix_sockets=true",
            "-c",
            "features.network_proxy=true",
            "-c",
            "approval_policy=\"never\"",
            "--output-schema",
            "{schema}",
            "--output-last-message",
            "{output}",
            "{prompt}",
        ]),
        gate_resume: Some(&[
            "exec",
            "resume",
            "-c",
            "default_permissions=\"shoal_gate\"",
            "-c",
            "permissions.shoal_gate.filesystem={\":root\"=\"read\"}",
            "-c",
            "permissions.shoal_gate.network.enabled=true",
            "-c",
            "permissions.shoal_gate.network.dangerously_allow_all_unix_sockets=true",
            "-c",
            "features.network_proxy=true",
            "-c",
            "approval_policy=\"never\"",
            "--output-schema",
            "{schema}",
            "--output-last-message",
            "{output}",
            "{session}",
            "{prompt}",
        ]),
        review_result: &output::CODEX,
        prompt_cache_minutes: Some(CODEX_PROMPT_CACHE_MINUTES),
    },
    Provider {
        name: "claude",
        integration: IntegrationKind::Claude,
        command: &["claude"],
        skill_dir: SkillDir::Claude,
        instructions: Some(&["--append-system-prompt", "{instructions}"]),
        resume: Some(&["--resume", "{session}"]),
        resume_latest: Some(&["--continue"]),
        fork: Some(&["--resume", "{session}", "--fork-session"]),
        // `--add-dir` takes every following argument up to the next option,
        // which would include a prompt; the joined form takes one.
        add_dir: Some(&["--add-dir={directory}"]),
        model_args: Some(&["--model", "{model}"]),
        effort_args: Some(&["--effort", "{effort}"]),
        effort_variable: Some("CLAUDE_CODE_EFFORT_LEVEL"),
        // Tool lists take every following argument, so they use the joined
        // form and the prompt comes first.
        review: Some(&[
            "-p",
            "{prompt}",
            "--output-format",
            "json",
            // Read-only whatever the user's or the reviewed repository's
            // settings allow: none of them load, anything not approved here
            // is denied, and the sandbox keeps commands from writing
            // outside the checkout, as `git diff --output` would. Its
            // sandbox reaches Unix sockets, the daemon's among them, and
            // the allowed tools reach them only through `shoal`, whose
            // commands write the review.
            "--setting-sources=",
            "--permission-mode=dontAsk",
            "--tools=Read,Grep,Glob,Bash",
            "--allowedTools=Read,Grep,Glob,Bash(git diff:*),Bash(git log:*),Bash(git show:*),Bash(git blame:*),Bash(shoal:*)",
            "--settings={\"sandbox\":{\"enabled\":true,\"failIfUnavailable\":true,\"autoAllowBashIfSandboxed\":false,\"allowUnsandboxedCommands\":false,\"network\":{\"allowAllUnixSockets\":true}}}",
        ]),
        review_resume: Some(&[
            "-p",
            "{prompt}",
            "--output-format",
            "json",
            "--setting-sources=",
            "--permission-mode=dontAsk",
            "--tools=Read,Grep,Glob,Bash",
            "--allowedTools=Read,Grep,Glob,Bash(git diff:*),Bash(git log:*),Bash(git show:*),Bash(git blame:*),Bash(shoal:*)",
            "--settings={\"sandbox\":{\"enabled\":true,\"failIfUnavailable\":true,\"autoAllowBashIfSandboxed\":false,\"allowUnsandboxedCommands\":false,\"network\":{\"allowAllUnixSockets\":true}}}",
            "--resume",
            "{session}",
        ]),
        // As `review`, returning the result its schema describes.
        gate: Some(&[
            "-p",
            "{prompt}",
            "--output-format",
            "json",
            "--json-schema",
            "{schema_json}",
            "--setting-sources=",
            "--permission-mode=dontAsk",
            "--tools=Read,Grep,Glob,Bash",
            "--allowedTools=Read,Grep,Glob,Bash(git diff:*),Bash(git log:*),Bash(git show:*),Bash(git blame:*),Bash(shoal:*)",
            "--settings={\"sandbox\":{\"enabled\":true,\"failIfUnavailable\":true,\"autoAllowBashIfSandboxed\":false,\"allowUnsandboxedCommands\":false,\"network\":{\"allowAllUnixSockets\":true}}}",
        ]),
        gate_resume: Some(&[
            "-p",
            "{prompt}",
            "--output-format",
            "json",
            "--json-schema",
            "{schema_json}",
            "--setting-sources=",
            "--permission-mode=dontAsk",
            "--tools=Read,Grep,Glob,Bash",
            "--allowedTools=Read,Grep,Glob,Bash(git diff:*),Bash(git log:*),Bash(git show:*),Bash(git blame:*),Bash(shoal:*)",
            "--settings={\"sandbox\":{\"enabled\":true,\"failIfUnavailable\":true,\"autoAllowBashIfSandboxed\":false,\"allowUnsandboxedCommands\":false,\"network\":{\"allowAllUnixSockets\":true}}}",
            "--resume",
            "{session}",
        ]),
        review_result: &output::CLAUDE,
        prompt_cache_minutes: None,
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
        fork: None,
        add_dir: None,
        model_args: None,
        effort_args: None,
        effort_variable: None,
        review: None,
        review_resume: None,
        gate: None,
        gate_resume: None,
        review_result: &output::PLAIN,
        prompt_cache_minutes: None,
    },
    Provider {
        name: "pi",
        integration: IntegrationKind::None,
        command: &["pi"],
        skill_dir: SkillDir::Home(".agents/skills"),
        instructions: None,
        resume: None,
        resume_latest: None,
        fork: None,
        add_dir: None,
        model_args: None,
        effort_args: None,
        effort_variable: None,
        review: None,
        review_resume: None,
        gate: None,
        gate_resume: None,
        review_result: &output::PLAIN,
        prompt_cache_minutes: None,
    },
    Provider {
        name: "grok",
        integration: IntegrationKind::None,
        command: &["grok"],
        skill_dir: SkillDir::Home(".grok/skills"),
        instructions: None,
        resume: None,
        resume_latest: None,
        fork: None,
        add_dir: None,
        model_args: None,
        effort_args: None,
        effort_variable: None,
        review: None,
        review_resume: None,
        gate: None,
        gate_resume: None,
        review_result: &output::PLAIN,
        prompt_cache_minutes: None,
    },
];

/// The built-in tool named `name`, if Shoal ships one.
pub(super) fn find(name: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|provider| provider.name == name)
}

/// Built-in launchers, the lowest layer of `[commands]`.
pub(crate) fn default_commands() -> Commands {
    PROVIDERS
        .iter()
        .map(|provider| {
            let argv = provider.command.iter().map(|arg| arg.to_string()).collect();
            (provider.name.to_owned(), argv)
        })
        .collect()
}
