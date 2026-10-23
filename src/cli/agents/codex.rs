//! Codex: its hooks deliver updates and report its turn state, its wrapper
//! wakes it by typing a prompt into its session, `config.toml` records the
//! directories it trusts, its sessions are rollout files under `sessions`,
//! and `codex.default_mode` may hand a launch to the Codex app.
use std::{ffi::OsString, path::Path};

use anyhow::Result;
use futures_util::future::BoxFuture;
use serde_json::Value;

use super::{
    Integration, Launch, desktop,
    trust::{CODEX, TrustFile},
};
use crate::{
    agent::{CodexMode, hooks},
    cli::{
        client,
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    config::{Effective, templates::Delivery},
    protocol::ConfigTarget,
    state::AgentState,
};

pub(super) struct Codex;

impl Integration for Codex {
    fn hook_args(&self) -> std::io::Result<Vec<OsString>> {
        config_overrides(true)
    }

    fn chat_hook_args(&self) -> std::io::Result<Vec<OsString>> {
        config_overrides(false)
    }

    /// Its wrapper starts an idle Codex's next turn only in a session it can
    /// type into.
    fn delivery(&self, ctx: &Context, settings: &Effective) -> Delivery {
        if terminal::agents_can_be_prompted(ctx, settings) {
            Delivery::Pushed
        } else {
            Delivery::Watched
        }
    }

    fn trust_file(&self) -> Option<&'static TrustFile> {
        Some(&CODEX)
    }

    /// Codex resumes a session by ID from any directory.
    fn has_session(&self, ctx: &Context, session: &str) -> bool {
        crate::env::codex_home().is_ok_and(|home| {
            let home = home.unwrap_or_else(|| ctx.paths.home.join(".codex"));
            has_rollout(&home.join("sessions"), session)
        })
    }

    fn wakes_by_prompt(&self) -> bool {
        true
    }

    /// `codex.default_mode = "app"` opens the Codex app, which cannot receive
    /// a prompt, so a launch with one stays in the terminal, as a tracked one
    /// does.
    fn launch<'a>(
        &'a self,
        ctx: &'a Context,
        launch: &'a mut Launch,
    ) -> BoxFuture<'a, Result<Option<i32>>> {
        Box::pin(async move {
            if launch.prompt.is_some()
                || launch.tracked
                || launch.settings.codex.default_mode != CodexMode::App
            {
                return Ok(None);
            }
            let args = std::mem::take(&mut launch.args);
            desktop::open_app(ctx, &launch.workspace, "codex", Some(&CODEX), args)
                .await
                .map(Some)
        })
    }
}

/// `shoal codex`: `--cli` or `--app` overrides `codex.default_mode`.
pub(in crate::cli) async fn shortcut(
    ctx: &Context,
    mode: Option<CodexMode>,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    // Explicit desktop handoffs do not consume prompt templates.
    if mode == Some(CodexMode::App) {
        return desktop::open_app(ctx, &workspace, "codex", Some(&CODEX), args).await;
    }
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let mut launch = Launch {
        name: "codex".into(),
        workspace,
        settings,
        prompt: None,
        args,
        tracked: false,
        replaces: Vec::new(),
    };
    if mode.is_none()
        && let Some(code) = Codex.launch(ctx, &mut launch).await?
    {
        return Ok(code);
    }
    super::launch::tracked(ctx, &Codex, launch).await
}

/// Codex takes each event's groups as a `-c hooks.<event>=` TOML value.
/// `turn_state` adds the hooks that report the turn's state.
fn config_overrides(turn_state: bool) -> std::io::Result<Vec<OsString>> {
    let shoal = hooks::shoal()?;
    let state = if turn_state {
        state_hooks(&shoal)
    } else {
        Vec::new()
    };
    Ok(hooks::by_event(hooks::messages(&shoal).chain(state))
        .into_iter()
        .flat_map(|(event, groups)| {
            let groups: toml::Value =
                serde_json::from_value(groups).expect("hook groups are valid TOML");
            ["-c".into(), format!("hooks.{event}={groups}").into()]
        })
        .collect())
}

/// Whether `sessions` holds the rollout file of `session`, which Codex names
/// `rollout-<time>-<session>.jsonl` in a directory per day.
fn has_rollout(sessions: &Path, session: &str) -> bool {
    if !crate::execution::recovery::valid_session(session) {
        return false;
    }
    let name = format!("-{session}.jsonl");
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return false;
    };
    entries.flatten().any(|entry| match entry.file_type() {
        Ok(kind) if kind.is_dir() => has_rollout(&entry.path(), session),
        Ok(kind) if kind.is_file() => entry
            .file_name()
            .to_str()
            .is_some_and(|file| file.ends_with(&name)),
        _ => false,
    })
}

/// Each Codex hook event with the group that reports its state. Codex has no
/// notice that its prompt waits for input, so an interrupted turn stays
/// working until its next turn ends.
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
        ("Stop", hooks::state(shoal, None, AgentState::Idle)),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn sessions_are_found_by_their_rollout_file() {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        assert!(!has_rollout(&sessions, "abc"));
        let day = sessions.join("2026/10/23");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("rollout-2026-10-23T12-00-00-abc.jsonl"), "").unwrap();
        assert!(has_rollout(&sessions, "abc"));
        assert!(!has_rollout(&sessions, "bc"));
        assert!(!has_rollout(&sessions, "abd"));
        assert!(!has_rollout(&sessions, "-abc"));
    }

    #[test]
    fn config_overrides_deliver_messages_and_report_state() {
        let hooks: BTreeMap<_, _> = Codex
            .hook_args()
            .unwrap()
            .chunks(2)
            .map(|flag| {
                assert_eq!(flag[0], "-c");
                let (key, value) = flag[1].to_str().unwrap().split_once('=').unwrap();
                let parsed: toml::Table = toml::from_str(&format!("value = {value}")).unwrap();
                (key.to_owned(), parsed["value"].clone())
            })
            .collect();
        assert_eq!(
            hooks.keys().collect::<Vec<_>>(),
            ["hooks.PostToolUse", "hooks.Stop", "hooks.UserPromptSubmit"]
        );
        for event in hooks::MESSAGE_EVENTS {
            let groups = &hooks[&format!("hooks.{event}")];
            let handler = &groups[0]["hooks"][0];
            assert_eq!(handler["type"].as_str(), Some("command"));
            assert!(
                handler["command"]
                    .as_str()
                    .unwrap()
                    .ends_with(" messages --hook")
            );
            let state = groups[1]["hooks"][0]["command"].as_str().unwrap();
            assert!(state.contains(" agent-state working "), "{state}");
        }
        let stop = hooks["hooks.Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(stop.contains(" agent-state idle "), "{stop}");
        let chat = Codex.chat_hook_args().unwrap();
        assert!(
            chat.iter()
                .all(|arg| !arg.to_str().unwrap().contains("agent-state"))
        );
        assert!(
            chat.iter()
                .any(|arg| arg.to_str().unwrap().contains("messages --hook"))
        );
    }
}
