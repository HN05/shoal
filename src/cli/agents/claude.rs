//! Claude Code: its hooks deliver updates, report its turn state, wake it
//! when idle and make the worktrees it creates child workspaces,
//! `.claude.json` records the directories it trusts, and a `claude://` link
//! opens a workspace in the Claude app.
use std::{ffi::OsString, os::unix::ffi::OsStrExt as _, path::Path};

use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::{
    AgentEnvironment, Integration, Wake,
    desktop::App,
    trust::{CLAUDE_CODE, TrustFile},
};
use crate::{
    agent::hooks,
    ai::IntegrationKind,
    state::{AgentState, TurnFailure},
};

pub(super) struct Claude;

impl Integration for Claude {
    /// Its settings carry the proxy's URL once the wrapper merges them, so
    /// a proxied launch's hooks are its plain ones.
    fn hook_args(&self, _proxied: bool) -> std::io::Result<Vec<OsString>> {
        settings(true)
    }

    fn chat_hook_args(&self) -> std::io::Result<Vec<OsString>> {
        settings(false)
    }

    /// Every `--settings` becomes one with the proxy's URL, since Claude
    /// Code takes only the last.
    fn route_through_proxy(
        &self,
        args: &mut Vec<OsString>,
        routing: &super::Routing,
        _environment: &AgentEnvironment,
    ) -> anyhow::Result<()> {
        settings::merge(args, routing)
    }

    /// Claude Code adds `ANTHROPIC_CUSTOM_HEADERS`, one per line, to each
    /// request, so the key joins those the agent has.
    fn proxy_environment(
        &self,
        key: &str,
        variable: &dyn Fn(&str) -> Option<OsString>,
    ) -> Vec<(&'static str, OsString)> {
        let mut headers = variable(CUSTOM_HEADERS).unwrap_or_default();
        if !headers.is_empty() {
            headers.push("\n");
        }
        headers.push(format!("{}: {key}", crate::proxy::KEY_HEADER));
        vec![(CUSTOM_HEADERS, headers)]
    }

    /// The 1-hour prompt cache, which Claude Code chooses itself only on a
    /// claude.ai login within its usage limits; an agent waiting for CI or a
    /// review often idles past the 5-minute one. Claude Code ranks this
    /// below a lifetime its settings or environment name.
    fn proxy_defaults(&self) -> &'static [(&'static str, &'static str)] {
        &[(PROMPT_CACHE_1H, "1")]
    }

    fn prompt_cache(
        &self,
        variable: &dyn Fn(&str) -> Option<OsString>,
        home: &Path,
    ) -> Option<std::time::Duration> {
        Some(prompt_cache::lifetime(variable, home))
    }

    /// Empty settings, which the wrapper's merge gives the proxy's URL.
    fn proxy_args(&self) -> Vec<OsString> {
        vec!["--settings".into(), "{}".into()]
    }

    /// Its hooks deliver updates and start its next turn with them.
    fn wakes(&self) -> Wake {
        Wake::Hook
    }

    /// Esc.
    fn interrupt_key(&self) -> Option<&'static str> {
        Some("\x1b")
    }

    fn trust_file(&self) -> Option<&'static TrustFile> {
        Some(&CLAUDE_CODE)
    }

    fn app(&self) -> Option<&'static App> {
        Some(&APP)
    }

    /// `StopFailure` names Claude Code's error kind; a gateway such as a
    /// proxy reports an overloaded upstream as a server error.
    fn turn_failure(&self, event: &Value) -> Option<TurnFailure> {
        Some(match event["error"].as_str()? {
            "rate_limit" => TurnFailure::RateLimited,
            "overloaded" => TurnFailure::Overloaded,
            "server_error" => TurnFailure::ServerError,
            _ => TurnFailure::Other,
        })
    }
}

/// The Claude app, opened with the link `claude --desktop` opens, which needs
/// no Claude account sign-in of the CLI.
pub(in crate::cli) const APP: App = App {
    name: "Claude app",
    program: "open",
    bundle: Some("/Applications/Claude.app"),
    check: macos_without_arguments,
    args: new_session,
};

/// The Claude app opens through `open` on macOS, and its link carries no
/// arguments.
fn macos_without_arguments(args: &[OsString]) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "the Claude app opens workspaces only on macOS"
    );
    ensure!(args.is_empty(), "the Claude app takes no arguments");
    Ok(())
}

/// `open`'s argument: the link that starts a Claude Code session in
/// `directory`.
fn new_session(directory: &Path, _args: Vec<OsString>) -> Vec<OsString> {
    let folder = percent_encode(directory.as_os_str().as_bytes());
    vec![format!("claude://code/new?folder={folder}").into()]
}

/// `bytes` with every byte but the URL-unreserved ones percent-encoded.
fn percent_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// One `--settings` carries every hook; Claude Code merges it with the user's
/// own settings, and a later `--settings` would replace it. A `workspace`
/// agent also reports its turn's state and may run the commands that finish
/// its assignment. For a proxied agent the wrapper merges these with any
/// others into the one that names the proxy's URL, since these settings win
/// over the user's own `env` and the inherited environment.
fn settings(workspace: bool) -> std::io::Result<Vec<OsString>> {
    let shoal = hooks::shoal()?;
    let (state, subagents, allow) = if workspace {
        (
            state_hooks(&shoal),
            subagent_hooks(&shoal),
            lifecycle_rules(),
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    let groups = hooks::messages(&shoal)
        .chain(state)
        .chain(subagents)
        .chain(wake_hooks(&shoal));
    let mut settings = json!({"hooks": hooks::by_event(groups)});
    if !allow.is_empty() {
        settings["permissions"] = json!({"allow": allow});
    }
    Ok(vec!["--settings".into(), settings.to_string().into()])
}

/// The Shoal commands with which a workspace's agent reports on and finishes
/// its own assignment. Claude Code's auto mode may refuse them as writes to an
/// outside system; the daemon still limits them to the agent's scope.
const LIFECYCLE_COMMANDS: [&str; 6] = [
    "done",
    "ready",
    "alerts send",
    "questions ask",
    "pr",
    "issue",
];

/// Rules allowing each lifecycle command, with or without arguments and
/// `--json`, as the agent's guidance spells them.
fn lifecycle_rules() -> Vec<String> {
    LIFECYCLE_COMMANDS
        .into_iter()
        .flat_map(|command| {
            ["shoal", "shoal --json"]
                .into_iter()
                .flat_map(move |shoal| {
                    let invocation = format!("{shoal} {command}");
                    [
                        format!("Bash({invocation})"),
                        format!("Bash({invocation} *)"),
                    ]
                })
        })
        .collect()
}

/// Each Claude Code hook event with the group that reports its state. A
/// session that starts, resumes or clears waits for its first turn. A
/// finished tool call, failed or not, means a permission prompt or question
/// was answered. No hook follows an interrupted turn, so Claude Code's notice
/// that its prompt has waited for input reports the turn finished. A turn an
/// API error ends reports its failure, which leaves the agent idle.
fn state_hooks(shoal: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "SessionStart",
            hooks::state(shoal, Some(hooks::SESSION_START), AgentState::Idle),
        ),
        (
            "UserPromptSubmit",
            hooks::state(shoal, None, AgentState::Working),
        ),
        (
            "PostToolUse",
            hooks::state(shoal, None, AgentState::Working),
        ),
        (
            "PostToolUseFailure",
            hooks::state(shoal, None, AgentState::Working),
        ),
        (
            "Notification",
            hooks::state(
                shoal,
                Some("permission_prompt|elicitation_dialog"),
                AgentState::Waiting,
            ),
        ),
        (
            "Notification",
            hooks::state(shoal, Some("idle_prompt"), AgentState::Idle),
        ),
        ("Stop", hooks::turn_ended(shoal, IntegrationKind::Claude)),
        (
            "StopFailure",
            hooks::turn_failed(shoal, IntegrationKind::Claude),
        ),
    ]
}

/// How long a worktree hook may take: creating one runs the child's setup.
const WORKTREE_TIMEOUT_SECS: u64 = 60 * 60;

/// The groups that tell each subagent which workspace it works in, create
/// the worktrees Claude Code asks for, as for a subagent with
/// `isolation: "worktree"`, as child workspaces, and finish them when it
/// removes one, instead of its own `git worktree` commands.
fn subagent_hooks(shoal: &str) -> Vec<(&'static str, Value)> {
    [
        ("WorktreeCreate", "worktree-create"),
        ("WorktreeRemove", "worktree-remove"),
    ]
    .into_iter()
    .map(|(event, command)| {
        let group = json!({"hooks": [{
            "type": "command",
            "command": format!("{shoal} internal {command}"),
            "timeout": WORKTREE_TIMEOUT_SECS,
        }]});
        (event, group)
    })
    .chain([hooks::subagent(shoal)])
    .collect()
}

/// How long the wake hook may wait for an update: a week, since an idle agent
/// should hear about a review whenever it arrives.
const WAKE_TIMEOUT_SECS: u64 = 7 * 24 * 60 * 60;

/// The groups that wait in the background for the next update and wake the
/// idle agent with it, after a turn that ends normally and after one an API
/// error ends, which fires `StopFailure` instead of `Stop`. Claude Code runs
/// the command in a shell it starts, whose parent is the agent; the shell
/// becomes the hook, marked as one that waits rather than works.
fn wake_hooks(shoal: &str) -> [(&'static str, Value); 2] {
    let group = json!({"hooks": [{
        "type": "command",
        "command": format!(
            "exec env {}=1 {shoal} internal wake --agent \"$PPID\"",
            crate::env::WAITING_HOOK
        ),
        "asyncRewake": true,
        "timeout": WAKE_TIMEOUT_SECS,
    }]});
    [("Stop", group.clone()), ("StopFailure", group)]
}

/// The variable whose lines Claude Code adds to each request as headers.
const CUSTOM_HEADERS: &str = "ANTHROPIC_CUSTOM_HEADERS";

/// The variable that gives Claude Code the 1-hour prompt cache unless a
/// lifetime is set elsewhere.
const PROMPT_CACHE_1H: &str = "ENABLE_PROMPT_CACHING_1H";

mod prompt_cache;
mod settings;

#[cfg(test)]
mod tests;
