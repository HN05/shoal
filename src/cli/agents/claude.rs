//! Claude Code: its hooks deliver updates, report its turn state, wake it
//! when idle and make the worktrees it creates child workspaces, and
//! `.claude.json` records the directories it trusts.
use std::ffi::OsString;

use serde_json::{Value, json};

use super::{
    AgentEnvironment, Integration, Wake,
    trust::{CLAUDE_CODE, TrustFile},
};
use crate::{
    agent::hooks,
    ai::IntegrationKind,
    state::{AgentState, TurnFailure},
};

pub(super) struct Claude;

impl Integration for Claude {
    fn hook_args(&self, proxied: bool) -> std::io::Result<Vec<OsString>> {
        settings(true, proxied)
    }

    fn chat_hook_args(&self) -> std::io::Result<Vec<OsString>> {
        settings(false, false)
    }

    /// Every `--settings` becomes one with the proxy's URL, since Claude
    /// Code takes only the last.
    fn route_through_proxy(
        &self,
        args: &mut Vec<OsString>,
        routing: &super::Routing,
        _environment: &AgentEnvironment,
    ) -> anyhow::Result<()> {
        crate::proxy::fill_url(args, routing.url);
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

    fn proxy_args(&self) -> Vec<OsString> {
        let settings = json!({"env": {"ANTHROPIC_BASE_URL": crate::proxy::URL}});
        vec!["--settings".into(), settings.to_string().into()]
    }

    /// Its hooks deliver updates and start its next turn with them.
    fn wakes(&self) -> Wake {
        Wake::Hook
    }

    fn routes_through_proxy(&self) -> bool {
        true
    }

    fn trust_file(&self) -> Option<&'static TrustFile> {
        Some(&CLAUDE_CODE)
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

/// One `--settings` carries every hook; Claude Code merges it with the user's
/// own settings, and a later `--settings` would replace it. A `workspace`
/// agent also reports its turn's state and may run the commands that finish
/// its assignment. A `proxied` one sends its API requests to Shoal's proxy:
/// the base URL goes here because these settings win over the user's own
/// `env`, which may set one, and over the inherited environment.
fn settings(workspace: bool, proxied: bool) -> std::io::Result<Vec<OsString>> {
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
    if proxied {
        settings["env"] = json!({"ANTHROPIC_BASE_URL": crate::proxy::URL});
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
        ("Stop", hooks::state(shoal, None, AgentState::Idle)),
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
/// the command in a shell it starts, whose parent is the agent.
fn wake_hooks(shoal: &str) -> [(&'static str, Value); 2] {
    let group = json!({"hooks": [{
        "type": "command",
        "command": format!("{shoal} internal wake --agent \"$PPID\""),
        "asyncRewake": true,
        "timeout": WAKE_TIMEOUT_SECS,
    }]});
    [("Stop", group.clone()), ("StopFailure", group)]
}

/// The variable whose lines Claude Code adds to each request as headers.
const CUSTOM_HEADERS: &str = "ANTHROPIC_CUSTOM_HEADERS";

mod settings;

#[cfg(test)]
mod tests;
