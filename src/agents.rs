//! Agents Shoal launches by name. A launch runs the agent's configured
//! command with the arguments its provider describes (see [`crate::ai`]); an
//! [`Integration`] adds what configuration cannot say for a tool Shoal knows,
//! such as hooks, trust entries or a launch of its own. Adding an agent means
//! configuration, plus an integration its [`IntegrationKind`] selects when
//! the tool needs one; an integration may also add agents of its own
//! ([`ADDED`], behind its Cargo feature). The CLI, the execution wrapper and
//! the daemon share what this module decides about a launch.
mod args;
mod checks;
mod claude;
mod codex;
pub(crate) mod detached;
mod happy;
pub(crate) mod hooks;
mod proxy;
pub(crate) mod trust;

use std::{ffi::OsString, path::Path};

use anyhow::Result;

use crate::{
    ai::IntegrationKind,
    config::{Effective, templates},
    model::Workspace,
};

pub(crate) use args::{
    dir_args, insert_options, model_args, restored_instruction_args, workspace_args,
    workspace_dir_args,
};
pub(crate) use checks::{
    check_model, check_queued_launch, ensure_available, ensure_forks, programs, resolve_model,
};
pub(crate) use proxy::{
    Routing, launch_proxy, proxy_defaults, proxy_ends_turns, proxy_environment,
    route_through_proxy, with_proxy_args,
};
pub(crate) use trust::forget_repository_trust;

use crate::client::transcripts::AgentEnvironment;

/// What an agent needs beyond its configured command and described
/// arguments. Each default adds nothing.
pub(crate) trait Integration: Sync {
    /// Arguments that register Shoal's hooks for the session of `space`'s
    /// agent and, when `proxied`, what [`Self::route_through_proxy`] points
    /// at Shoal's proxy. They inherit the space's identity.
    fn hook_args(&self, _space: AgentSpace, _proxied: bool) -> std::io::Result<Vec<OsString>> {
        Ok(Vec::new())
    }

    /// Whether its launches can send their API requests to Shoal's proxy:
    /// those of an integration with arguments that point it there.
    fn routes_through_proxy(&self) -> bool {
        !self.proxy_args().is_empty()
    }

    /// Whether a response stream its proxy ends with a failure ends its
    /// turn, which the proxy then reports: for an agent with no hook of its
    /// own for a failed turn. One with a hook reports its turn's end itself,
    /// since a subagent's request may fail while the turn goes on.
    fn proxy_ends_turns(&self) -> bool {
        false
    }

    /// What its environment gets so its requests name its proxy's `key`:
    /// by default [`crate::proxy::KEY_VARIABLE`], which its proxy arguments
    /// name. `variable` reads the environment it runs with otherwise.
    fn proxy_environment(
        &self,
        key: &str,
        _variable: &dyn Fn(&str) -> Option<OsString>,
    ) -> Vec<(&'static str, OsString)> {
        vec![(crate::proxy::KEY_VARIABLE, key.into())]
    }

    /// Variables a launch through its proxy gets when the environment it
    /// runs with leaves them unset.
    fn proxy_defaults(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// Arguments that only send its API requests to Shoal's proxy, for a
    /// launch Shoal gives no hooks, such as a configured resume command,
    /// with the placeholders of [`Self::hook_args`].
    fn proxy_args(&self) -> Vec<OsString> {
        Vec::new()
    }

    /// Point `args` at the proxy by `routing`, for the agent running with
    /// `environment`; nothing for an integration that does not route.
    fn route_through_proxy(
        &self,
        _args: &mut Vec<OsString>,
        _routing: &Routing,
        _environment: &AgentEnvironment,
    ) -> Result<()> {
        Ok(())
    }

    /// The config file in which the agent records trusted directories.
    fn trust_file(&self) -> Option<&'static trust::TrustFile> {
        None
    }

    /// How the idle agent wakes for an update.
    fn wakes(&self) -> Wake {
        Wake::Never
    }

    /// The key that cancels the agent's running turn in its terminal and
    /// leaves it waiting for the next message, unless Shoal knows none.
    fn interrupt_key(&self) -> Option<&'static str> {
        None
    }

    /// The programs a launch starts, when not its configured command's.
    fn programs(&self) -> Option<Vec<&'static str>> {
        None
    }

    /// How long the agent's prompt cache lives after a turn, as its own
    /// settings give it; `variable` reads the environment it runs with.
    fn prompt_cache(
        &self,
        _variable: &dyn Fn(&str) -> Option<OsString>,
        _home: &Path,
    ) -> Option<std::time::Duration> {
        None
    }

    /// The failure that `event`, from its hook for a turn an API error ended,
    /// reports; `None` for an agent without one.
    fn turn_failure(&self, _event: &serde_json::Value) -> Option<crate::state::TurnFailure> {
        None
    }

    /// The change to the agent's plan that `event`, from its hook after a
    /// plan tool call, reports; `None` for an agent without plan hooks or
    /// an event that changes no plan.
    fn plan_change(
        &self,
        _event: &serde_json::Value,
    ) -> Option<crate::client::progress::ProgressChange> {
        None
    }

    /// The app that runs the agent's workspace sessions and chats detached
    /// from any terminal, in place of the tracked terminal launch and the
    /// chat run in place.
    fn detached(&self) -> Option<&dyn detached::App> {
        None
    }
}

/// How an idle agent wakes for an update, which decides whether updates are
/// pushed to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    /// It does not; it finds updates by watching for them.
    Never,
    /// A hook of its own waits for the update and starts its next turn.
    Hook,
    /// Its wrapper types a prompt into its session, for an agent without a
    /// hook that can start its turn; only a session the wrapper can type
    /// into wakes it.
    Prompt,
}

/// An agent without an integration: its configuration says everything.
struct Configured;

impl Integration for Configured {}

/// The integration of an AI tool of `kind`.
pub(crate) fn integration(kind: IntegrationKind) -> &'static dyn Integration {
    match kind {
        IntegrationKind::Claude => &claude::Claude,
        IntegrationKind::Codex => &codex::Codex,
        IntegrationKind::None => &Configured,
    }
}

/// An agent an integration adds besides the AI tools, with the integration
/// that runs it and why `settings` turn that integration off, if they do;
/// pickers and completion leave the agents of one that is off out.
pub(crate) struct Added {
    pub name: &'static str,
    pub integration: &'static dyn Integration,
    off: fn(&Effective) -> Option<&'static str>,
}

/// Agents integrations add.
pub(crate) const ADDED: &[Added] = &[
    Added {
        name: "happy-claude",
        integration: &happy::CLAUDE,
        off: happy::off,
    },
    Added {
        name: "happy-codex",
        integration: &happy::CODEX,
        off: happy::off,
    },
];

/// The agent `name`, when an integration adds it.
pub(crate) fn added(name: &str) -> Option<&'static Added> {
    ADDED.iter().find(|added| added.name == name)
}

/// The integration agent `name` runs with under `settings`: its AI tool's,
/// else that of the integration adding it.
pub(crate) fn agent_integration(settings: &Effective, name: &str) -> &'static dyn Integration {
    match settings.integration(name) {
        IntegrationKind::None => added(name).map_or(&Configured, |added| added.integration),
        kind => integration(kind),
    }
}

/// Why `settings` turn agent `name` off, if they do: its own `enabled =
/// false`, or what turns off the integration that adds it.
pub(crate) fn agent_off(settings: &Effective, name: &str) -> Option<&'static str> {
    if settings.turned_off(name) {
        return Some("its [agents] entry sets enabled = false");
    }
    match (settings.integration(name), added(name)) {
        (IntegrationKind::None, Some(added)) => (added.off)(settings),
        _ => None,
    }
}

/// How updates reach an agent with `integration` launched under `settings`:
/// pushed when it wakes for them.
pub(crate) fn delivery(integration: &dyn Integration, settings: &Effective) -> templates::Delivery {
    match integration.wakes() {
        Wake::Hook => templates::Delivery::Pushed,
        Wake::Prompt if crate::terminal::agents_can_be_prompted(settings) => {
            templates::Delivery::Pushed
        }
        Wake::Prompt | Wake::Never => templates::Delivery::Watched,
    }
}

/// What a workspace's agent learns from its hooks about the workspace it
/// works in.
#[derive(Clone, Copy)]
pub(crate) struct AgentWorkspace<'a> {
    pub name: &'a str,
    /// `None` for a workspace at a commit.
    pub branch: Option<&'a str>,
}

impl<'a> From<&'a Workspace> for AgentWorkspace<'a> {
    fn from(workspace: &'a Workspace) -> Self {
        Self {
            name: &workspace.name,
            branch: workspace.branch.as_deref(),
        }
    }
}

/// The space whose agent Shoal's hooks serve: a workspace, whose agent
/// also gets the hooks and rules that need its worktree, or a chat.
#[derive(Clone, Copy)]
pub(crate) enum AgentSpace<'a> {
    Workspace(AgentWorkspace<'a>),
    Chat,
}

impl<'a> AgentSpace<'a> {
    /// The workspace, unless the space is a chat.
    pub fn workspace(self) -> Option<AgentWorkspace<'a>> {
        match self {
            Self::Workspace(workspace) => Some(workspace),
            Self::Chat => None,
        }
    }
}

impl<'a> From<AgentWorkspace<'a>> for AgentSpace<'a> {
    fn from(workspace: AgentWorkspace<'a>) -> Self {
        Self::Workspace(workspace)
    }
}

impl<'a> From<&'a Workspace> for AgentSpace<'a> {
    fn from(workspace: &'a Workspace) -> Self {
        Self::Workspace(workspace.into())
    }
}

/// Arguments that register Shoal's hooks for the session of `space`'s agent
/// with integration `kind`, and when `proxied` route its API requests
/// through Shoal's proxy.
pub(crate) fn hook_args(
    kind: IntegrationKind,
    space: AgentSpace,
    proxied: bool,
) -> std::io::Result<Vec<OsString>> {
    integration(kind).hook_args(space, proxied)
}

/// How long the prompt cache of an agent with integration `kind` lives after
/// a turn, as its own settings in the environment `variable` reads give it.
pub(crate) fn prompt_cache(
    kind: IntegrationKind,
    variable: &dyn Fn(&str) -> Option<OsString>,
    home: &Path,
) -> Option<std::time::Duration> {
    integration(kind).prompt_cache(variable, home)
}

/// When the wrapper of an agent wakes it by typing a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptWake {
    /// Whenever it idles: no hook of its own starts its turn.
    Always,
    /// While it idles where the hook that wakes it otherwise cannot: after
    /// an interrupted turn, which the hook does not follow, or once no hook
    /// has waited for its updates for a few minutes.
    WhenHookCannot,
}

/// When the wrapper of an agent with integration `kind` wakes it by typing
/// a prompt, unless it never does.
pub(crate) fn prompt_wake(kind: IntegrationKind) -> Option<PromptWake> {
    let integration = integration(kind);
    match integration.wakes() {
        Wake::Prompt => Some(PromptWake::Always),
        Wake::Hook => Some(PromptWake::WhenHookCannot),
        Wake::Never => None,
    }
}

#[cfg(test)]
mod tests;
