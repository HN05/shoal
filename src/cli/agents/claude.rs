//! Claude Code: its hooks deliver updates, report its turn state, wake it
//! when idle and make the worktrees it creates child workspaces, and
//! `.claude.json` records the directories it trusts.
use std::ffi::OsString;

use serde_json::{Value, json};

use super::{
    Integration,
    trust::{CLAUDE_CODE, TrustFile},
};
use crate::{
    agent::hooks,
    cli::context::Context,
    config::{Effective, templates::Delivery},
    state::{AgentState, TurnFailure},
};

pub(super) struct Claude;

impl Integration for Claude {
    fn hook_args(&self) -> std::io::Result<Vec<OsString>> {
        settings(true)
    }

    fn chat_hook_args(&self) -> std::io::Result<Vec<OsString>> {
        settings(false)
    }

    /// Its hooks deliver updates and start its next turn with them.
    fn delivery(&self, _ctx: &Context, _settings: &Effective) -> Delivery {
        Delivery::Pushed
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
/// own settings. A `workspace` agent also reports its turn's state and may run
/// the commands that finish its assignment.
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
        ("Stop", hooks::state(shoal, None, AgentState::Idle)),
        ("StopFailure", hooks::turn_failed(shoal, "claude")),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_deliver_messages_report_state_and_wake_the_idle_agent() {
        let args = Claude.hook_args().unwrap();
        assert_eq!(args[0], "--settings");
        let settings: Value = serde_json::from_str(args[1].to_str().unwrap()).unwrap();
        for event in hooks::MESSAGE_EVENTS {
            let command = settings["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert!(command.ends_with(" messages --hook"), "{command}");
        }
        let states: Vec<_> = state_hooks("shoal")
            .into_iter()
            .filter(|(event, _)| *event != "StopFailure")
            .map(|(event, group)| {
                let command = group["hooks"][0]["command"].as_str().unwrap().to_owned();
                let state = command.split(' ').nth(3).unwrap().to_owned();
                (event, group["matcher"].as_str().map(str::to_owned), state)
            })
            .collect();
        let expected = |event, matcher: Option<&str>, state: &str| {
            (event, matcher.map(str::to_owned), state.to_owned())
        };
        assert_eq!(
            states,
            [
                expected("SessionStart", Some("startup|resume|clear"), "idle"),
                expected("UserPromptSubmit", None, "working"),
                expected("PostToolUse", None, "working"),
                expected("PostToolUseFailure", None, "working"),
                expected(
                    "Notification",
                    Some("permission_prompt|elicitation_dialog"),
                    "waiting"
                ),
                expected("Notification", Some("idle_prompt"), "idle"),
                expected("Stop", None, "idle"),
            ]
        );
        // A turn an API error ended reports its failure, read from the event.
        let failed = settings["hooks"]["StopFailure"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            failed.contains(" internal turn-failed --integration claude "),
            "{failed}"
        );
        // A second group waits in the background to wake the idle agent,
        // after a failed turn as after one that ended normally.
        for event in ["Stop", "StopFailure"] {
            let wake = &settings["hooks"][event][1]["hooks"][0];
            assert!(
                wake["command"]
                    .as_str()
                    .unwrap()
                    .ends_with(" internal wake --agent \"$PPID\""),
                "{event}: {wake}"
            );
            assert_eq!(wake["asyncRewake"], true);
            assert_eq!(wake["timeout"], WAKE_TIMEOUT_SECS);
        }
        // Worktrees Claude Code creates are child workspaces, and each
        // subagent learns which workspace it works in.
        for (event, command) in [
            ("WorktreeCreate", " internal worktree-create"),
            ("WorktreeRemove", " internal worktree-remove"),
            ("SubagentStart", " internal subagent-start"),
        ] {
            let handler = &settings["hooks"][event][0]["hooks"][0];
            assert!(
                handler["command"].as_str().unwrap().ends_with(command),
                "{handler}"
            );
        }
        // The agent may finish its own assignment without a permission check.
        let allow = settings["permissions"]["allow"].as_array().unwrap();
        for rule in [
            "Bash(shoal done)",
            "Bash(shoal --json done)",
            "Bash(shoal --json done *)",
            "Bash(shoal --json pr *)",
        ] {
            assert!(allow.contains(&json!(rule)), "{rule}: {allow:?}");
        }
    }

    #[test]
    fn stop_failure_errors_name_the_turn_failure() {
        for (error, failure) in [
            ("rate_limit", Some(TurnFailure::RateLimited)),
            ("overloaded", Some(TurnFailure::Overloaded)),
            ("server_error", Some(TurnFailure::ServerError)),
            ("authentication_failed", Some(TurnFailure::Other)),
        ] {
            let event = json!({"hook_event_name": "StopFailure", "error": error});
            assert_eq!(Claude.turn_failure(&event), failure, "{error}");
        }
        assert_eq!(
            Claude.turn_failure(&json!({"hook_event_name": "Stop"})),
            None
        );
    }

    #[test]
    fn chat_settings_deliver_messages_and_wake_without_turn_state() {
        let args = Claude.chat_hook_args().unwrap();
        let settings: Value = serde_json::from_str(args[1].to_str().unwrap()).unwrap();
        assert!(!settings.to_string().contains("agent-state"), "{settings}");
        // A chat belongs to no workspace and finishes no assignment.
        assert!(settings.get("permissions").is_none(), "{settings}");
        assert!(
            settings["hooks"].get("WorktreeCreate").is_none(),
            "{settings}"
        );
        for event in ["Stop", "StopFailure"] {
            let wake = &settings["hooks"][event][0]["hooks"][0]["command"];
            assert!(wake.as_str().unwrap().contains(" internal wake "), "{wake}");
        }
        for event in hooks::MESSAGE_EVENTS {
            assert!(settings["hooks"][event][0]["hooks"][0]["command"].is_string());
        }
    }
}
