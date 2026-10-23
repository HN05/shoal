//! Claude Code: its hooks deliver updates, report its turn state and wake it
//! when idle, `.claude.json` records the directories it trusts, and each
//! directory's project folder holds the sessions that ran there.
mod sessions;

use std::{ffi::OsString, path::Path};

use anyhow::Result;
use serde_json::{Value, json};

use super::{
    AgentEnvironment, Integration,
    trust::{CLAUDE_CODE, TrustFile},
};
use crate::{
    agent::hooks,
    cli::context::Context,
    config::{Effective, templates::Delivery},
    state::AgentState,
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

    fn has_session(&self, environment: &AgentEnvironment, session: &str) -> bool {
        sessions::projects(environment)
            .and_then(|projects| sessions::find(&projects, session))
            .is_ok_and(|folder| folder.is_some())
    }

    /// Claude Code resumes only sessions in the directory's project folder.
    fn carry_session(
        &self,
        environment: &AgentEnvironment,
        session: &str,
        directory: &Path,
    ) -> Result<()> {
        sessions::carry(&sessions::projects(environment)?, session, directory)
    }
}

/// One `--settings` carries every hook; Claude Code merges it with the user's
/// own settings. `turn_state` adds the hooks that report the turn's state.
fn settings(turn_state: bool) -> std::io::Result<Vec<OsString>> {
    let shoal = hooks::shoal()?;
    let state = if turn_state {
        state_hooks(&shoal)
    } else {
        Vec::new()
    };
    let groups = hooks::messages(&shoal)
        .chain(state)
        .chain([wake_hook(&shoal)]);
    Ok(vec![
        "--settings".into(),
        json!({"hooks": hooks::by_event(groups)}).to_string().into(),
    ])
}

/// Each Claude Code hook event with the group that reports its state. A
/// finished tool call, failed or not, means a permission prompt or question
/// was answered. No hook follows an interrupted turn, so Claude Code's notice
/// that its prompt has waited for input reports the turn finished.
fn state_hooks(shoal: &str) -> Vec<(&'static str, Value)> {
    vec![
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
        ("StopFailure", hooks::state(shoal, None, AgentState::Idle)),
    ]
}

/// How long the wake hook may wait for an update: a week, since an idle agent
/// should hear about a review whenever it arrives.
const WAKE_TIMEOUT_SECS: u64 = 7 * 24 * 60 * 60;

/// The `Stop` group that waits in the background for the next update and
/// wakes the idle agent with it. Claude Code runs the command in a shell it
/// starts, whose parent is the agent.
fn wake_hook(shoal: &str) -> (&'static str, Value) {
    (
        "Stop",
        json!({"hooks": [{
            "type": "command",
            "command": format!("{shoal} internal wake --agent \"$PPID\""),
            "asyncRewake": true,
            "timeout": WAKE_TIMEOUT_SECS,
        }]}),
    )
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
                expected("StopFailure", None, "idle"),
            ]
        );
        // A second Stop group waits in the background to wake the idle agent.
        let wake = &settings["hooks"]["Stop"][1]["hooks"][0];
        assert!(
            wake["command"]
                .as_str()
                .unwrap()
                .ends_with(" internal wake --agent \"$PPID\""),
            "{wake}"
        );
        assert_eq!(wake["asyncRewake"], true);
        assert_eq!(wake["timeout"], WAKE_TIMEOUT_SECS);
    }

    #[test]
    fn chat_settings_deliver_messages_and_wake_without_turn_state() {
        let args = Claude.chat_hook_args().unwrap();
        let settings: Value = serde_json::from_str(args[1].to_str().unwrap()).unwrap();
        assert!(!settings.to_string().contains("agent-state"), "{settings}");
        let stop = &settings["hooks"]["Stop"][0]["hooks"][0]["command"];
        assert!(stop.as_str().unwrap().contains(" internal wake "), "{stop}");
        for event in hooks::MESSAGE_EVENTS {
            assert!(settings["hooks"][event][0]["hooks"][0]["command"].is_string());
        }
    }
}
