//! The command that restores a tracked agent, resolved from its space's
//! settings and records as the daemon has them now.
use shoal_ids::agent_session_ids::AgentSessionId;
use std::{ffi::OsString, path::Path};

use anyhow::Result;

use crate::{
    agent::{Agent, ModelChoice},
    agents::AgentHome,
    client::connection::CommandPaths,
    model::ExecutionSpace,
    protocol::ConfigTarget,
};
use shoal_core::execution::{
    TrackedAgent,
    recovery::{Record, RecordExt as _, Recovery, Stop, StopExt as _, resumed_prompt},
};
use shoal_protocol::space_names::SpaceSelector;

/// What a restore command of agent `name` in `home` is built from: the
/// message it continues with and the arguments that keep its model and
/// effort.
struct RestoreCommand<'a> {
    paths: &'a CommandPaths,
    settings: &'a crate::config::Effective,
    name: &'a Agent,
    home: AgentHome<'a>,
    prompt: &'a std::ffi::OsStr,
    model_args: Vec<OsString>,
    /// The arguments that give the agent its mode's rules again, which only
    /// its built-in resume arguments carry.
    instruction_args: Vec<OsString>,
}

impl RestoreCommand<'_> {
    /// The agent's configured `argv` restore command, with its model, effort
    /// and proxy arguments, and whether it carries the prompt. It is the
    /// user's own, so Shoal adds no rules to it.
    async fn configured(self, argv: &[String]) -> Result<(Vec<OsString>, bool)> {
        let mut command = (self.home)
            .expand(
                self.paths,
                &self.settings.resume_commands,
                self.name.name(),
                vec![],
                &[("{prompt}", self.prompt)],
            )
            .await?;
        crate::agents::insert_options(&mut command, self.model_args);
        // Without Shoal's hooks it still goes through the agent's proxy.
        crate::agents::with_proxy_args(self.settings, self.name, &mut command);
        Ok((command, argv.iter().any(|arg| arg.contains("{prompt}"))))
    }

    /// The agent's launcher with `resume_args`, its model, effort, rules and
    /// its space's arguments, and the prompt.
    async fn resumed(self, mut args: Vec<OsString>) -> Result<Vec<OsString>> {
        args.extend(self.model_args);
        args.extend(self.instruction_args);
        args.extend(self.home.args(self.paths, self.settings, self.name).await?);
        let takes_field = self
            .settings
            .launchers
            .get(self.name.name())
            .is_some_and(|argv| argv.iter().any(|arg| arg.contains("{prompt}")));
        if !takes_field {
            args.push(self.prompt.to_owned());
        }
        (self.home)
            .expand(
                self.paths,
                &self.settings.launchers,
                self.name.name(),
                args,
                &[("{prompt}", self.prompt)],
            )
            .await
    }
}

/// The agent a restore continues: its name, the model and effort it ran
/// with, the mode and narrowing it started with, and the session it
/// reported, if any.
pub struct Restored<'a> {
    pub agent: &'a Agent,
    pub model: &'a ModelChoice,
    pub mode: &'a crate::config::modes::ModeName,
    pub narrowing: crate::config::permissions::PermissionsConfig,
    pub session: Option<&'a AgentSessionId>,
}

/// The restore of `agent` as its launch in `workspace` resolves it, or one
/// that restores nothing when that fails.
pub(super) async fn for_launch(
    paths: &CommandPaths,
    workspace: &SpaceSelector,
    agent: &TrackedAgent,
) -> Recovery {
    let restored = Restored {
        agent: &agent.name,
        model: &agent.model,
        mode: &agent.mode,
        narrowing: agent.narrowing,
        session: None,
    };
    let resolved = resolve_in(paths, workspace, restored, None).await;
    resolved.unwrap_or_else(|error| {
        eprintln!("warning: automatic agent recovery unavailable: {error:#}");
        Recovery {
            agent: agent.name.normalized(),
            model: agent.model.clone(),
            mode: agent.mode.clone(),
            narrowing: agent.narrowing,
            command: vec![],
            automatic: false,
            handoff_delivered: false,
            proxy: None,
        }
    })
}

/// [`resolve`] in the space `space` as the daemon records it now.
async fn resolve_in(
    paths: &CommandPaths,
    space: &SpaceSelector,
    restored: Restored<'_>,
    stop: Option<&Stop>,
) -> Result<Recovery> {
    let records = crate::agents::HomeRecords::read(paths, space).await?;
    resolve(paths, records.home()?, restored, None, stop).await
}

/// Ask the `restored` session to continue, telling it about its `stop`
/// and including `handoff` where the command accepts a prompt: agents
/// with resume arguments and `{prompt}` in its `resume_command`. Resume
/// arguments restore its session when known, otherwise the workspace's
/// latest session, with its model and effort, in `home`, a workspace or
/// a chat.
pub async fn resolve<'a>(
    paths: &CommandPaths,
    home: impl Into<AgentHome<'a>>,
    restored: Restored<'_>,
    handoff: Option<&str>,
    stop: Option<&Stop>,
) -> Result<Recovery> {
    let home = home.into();
    let settings =
        crate::config::settings(paths, ConfigTarget::Space(SpaceSelector::id(home.id()))).await?;
    let name = restored.agent.normalized();
    // A restore of an agent config turned off waits, as one it cannot
    // run with its model does.
    anyhow::ensure!(
        !settings.turned_off(&name),
        "{name} is turned off: its [agents] entry sets enabled = false"
    );
    let configured = settings.resume_commands.get(name.name());
    let resume_args = settings
        .launch(&name)
        .arguments
        .resume_args(restored.session);
    let automatic = configured.is_some() || resume_args.is_some();
    let message = resumed_prompt(stop, handoff);
    let restore = RestoreCommand {
        paths,
        settings: &settings,
        name: &name,
        home,
        prompt: std::ffi::OsStr::new(&message),
        // A restore runs with what the agent ran with, or not at all, so
        // the record stays for once its templates take the values again.
        model_args: crate::agents::model_args(&settings, &name, restored.model)?,
        instruction_args: crate::agents::restored_instruction_args(
            &settings,
            &name,
            home,
            restored.mode,
        ),
    };
    let (command, handoff_delivered) = match (configured, resume_args) {
        (Some(argv), _) => restore.configured(argv).await?,
        (None, Some(args)) => (restore.resumed(args).await?, true),
        (None, None) => (vec![], false),
    };
    let proxy = crate::agents::launch_proxy(&settings, &name)?.cloned();
    Ok(Recovery {
        agent: name,
        model: restored.model.clone(),
        mode: restored.mode.to_owned(),
        narrowing: restored.narrowing,
        command,
        automatic,
        handoff_delivered: handoff_delivered && handoff.is_some(),
        proxy,
    })
}

/// The command restoring the agent stopped with `record`, with the
/// proxy it was resolved to go through: the session it saved, told why
/// and when it stopped, otherwise the restore command resolved at launch.
pub(super) async fn restore_command(
    recovery: &Recovery,
    paths: &CommandPaths,
    space: &ExecutionSpace,
    record: Option<&Path>,
) -> (Vec<OsString>, Option<shoal_core::proxy::Proxy>) {
    let launched = || (recovery.command.clone(), recovery.proxy.clone());
    let Some((path, record)) =
        record.and_then(|path| Some((path, Record::load(path).ok().flatten()?)))
    else {
        return launched();
    };
    let stop = Stop::of(&record, path);
    let restored = Restored {
        narrowing: recovery.narrowing,
        agent: &recovery.agent,
        model: &recovery.model,
        mode: &recovery.mode,
        session: record.session.as_ref(),
    };
    let space = SpaceSelector::id(&space.id);
    match resolve_in(paths, &space, restored, Some(&stop)).await {
        Ok(resolved) if !resolved.command.is_empty() => (resolved.command, resolved.proxy),
        Ok(_) => launched(),
        Err(error) => {
            eprintln!("warning: cannot resolve the restore command: {error:#}");
            launched()
        }
    }
}
