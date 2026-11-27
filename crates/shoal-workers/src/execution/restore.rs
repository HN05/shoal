//! The command that restores a tracked agent.
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::executions::TrackedAgent;
use std::{
    borrow::Cow,
    ffi::{OsStr, OsString},
    path::Path,
};

use anyhow::Result;

use crate::{
    agent::{Agent, ModelChoice},
    agents::AgentHome,
    client::connection::CommandPaths,
    config::named_commands::{self, Commands},
    model::ExecutionSpace,
    protocol::ConfigTarget,
};
use shoal_core::execution::recovery::{
    Ending, Record, RecordExt as _, Recovery, Stop, StopExt as _, resumed_prompt,
};
use shoal_core::terminal::WAKE_PROMPT;
use shoal_protocol::space_names::SpaceSelector;

/// What a restore command of agent `name` in `home` is built from: the
/// message it opens with, if any, and the arguments that keep its model and
/// effort.
struct RestoreCommand<'a> {
    paths: &'a CommandPaths,
    settings: &'a crate::config::Effective,
    name: &'a Agent,
    home: AgentHome<'a>,
    prompt: Option<&'a OsStr>,
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
        let carries = self.prompt.is_some() && argv.iter().any(|arg| arg.contains(PROMPT));
        let commands = self.commands(&self.settings.resume_commands);
        let mut command = self.expand(&commands, vec![]).await?;
        crate::agents::insert_options(&mut command, self.model_args);
        // Without Shoal's hooks it still goes through the agent's proxy.
        crate::agents::with_proxy_args(self.settings, self.name, &mut command);
        Ok((command, carries))
    }

    /// The agent's launcher with `resume_args`, its model, effort, rules and
    /// its space's arguments, and the prompt, if any.
    async fn resumed(mut self, mut args: Vec<OsString>) -> Result<Vec<OsString>> {
        args.extend(std::mem::take(&mut self.model_args));
        args.extend(std::mem::take(&mut self.instruction_args));
        args.extend(crate::homes::args(self.paths, self.home, self.settings, self.name).await?);
        let takes_field = self
            .settings
            .launchers
            .get(self.name.name())
            .is_some_and(|argv| argv.iter().any(|arg| arg.contains(PROMPT)));
        if let Some(prompt) = self.prompt.filter(|_| !takes_field) {
            args.push(prompt.to_owned());
        }
        let commands = self.commands(&self.settings.launchers);
        self.expand(&commands, args).await
    }

    /// `commands`, the agent's among them, without the arguments that are
    /// exactly `{prompt}` when there is no prompt to put there.
    fn commands<'c>(&self, commands: &'c Commands) -> Cow<'c, Commands> {
        match self.prompt {
            Some(_) => Cow::Borrowed(commands),
            None => Cow::Owned(named_commands::without_argument(
                commands,
                self.name.name(),
                PROMPT,
            )),
        }
    }

    /// The agent's command among `commands` with `args`, its prompt in
    /// place of `{prompt}`, or nothing without one.
    async fn expand(&self, commands: &Commands, args: Vec<OsString>) -> Result<Vec<OsString>> {
        let prompt = self.prompt.unwrap_or_default();
        let fields = [(PROMPT, prompt)];
        crate::homes::expand(
            self.paths,
            self.home,
            commands,
            self.name.name(),
            args,
            &fields,
        )
        .await
    }
}

/// The placeholder a restore command takes its prompt at.
const PROMPT: &str = "{prompt}";

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
    let resolved = resolve_in(paths, workspace, restored, Ending::default()).await;
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
    ending: Ending<'_>,
) -> Result<Recovery> {
    let records = crate::homes::records(paths, space).await?;
    resolve(paths, records.home()?, restored, None, ending).await
}

/// Restore the `restored` session, which ended as `ending` says. A session
/// that ended mid-turn is asked to continue and told about its stop. Any
/// other waits, or with resume arguments takes the updates that wait for
/// it. Either is told `handoff` where the command accepts a prompt: with
/// resume arguments and `{prompt}` in its `resume_command`.
///
/// Resume arguments restore its session when known, and otherwise the
/// latest session in `home`, a workspace or a chat, with its model and
/// effort.
pub async fn resolve<'a>(
    paths: &CommandPaths,
    home: impl Into<AgentHome<'a>>,
    restored: Restored<'_>,
    handoff: Option<&str>,
    ending: Ending<'_>,
) -> Result<Recovery> {
    let home = home.into();
    let settings =
        crate::settings::of(paths, ConfigTarget::Space(SpaceSelector::id(home.id()))).await?;
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
    let automatic = settings.restores_unattended(&name, restored.session);
    let message = match resumed_prompt(ending, handoff) {
        Some(message) => Some(message),
        // A configured command runs without the hooks that deliver them.
        None if configured.is_none() => waking_prompt(paths, home, &settings, &name).await,
        None => None,
    };
    let restore = RestoreCommand {
        paths,
        settings: &settings,
        name: &name,
        home,
        prompt: message.as_deref().map(OsStr::new),
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

/// What a session that opens between turns with nothing to be told
/// starts with: the prompt its wrapper wakes an idle agent with, whose hook
/// delivers them, while updates wait for it in `home` and its agent has
/// that hook; otherwise nothing, so that it waits for them, as it does
/// when the daemon cannot say, which a later wake makes up for.
async fn waking_prompt(
    paths: &CommandPaths,
    home: AgentHome<'_>,
    settings: &crate::config::Effective,
    name: &Agent,
) -> Option<String> {
    crate::agents::prompt_wake(settings.integration(name))?;
    let waiting = crate::client::agent_updates::waiting(paths, SpaceSelector::id(home.id()));
    match waiting.await {
        Ok(waiting) => waiting.then(|| WAKE_PROMPT.to_owned()),
        Err(error) => {
            eprintln!("warning: cannot tell whether updates wait for the agent: {error:#}");
            None
        }
    }
}

/// The command restoring the agent stopped with `record`, with the
/// proxy it was resolved to go through: the session it saved, told why
/// and when it stopped when that was mid-turn, otherwise the restore
/// command resolved at launch.
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
    let ending = Ending {
        stop: Some(&stop),
        turn: record.turn,
    };
    match resolve_in(paths, &space, restored, ending).await {
        Ok(resolved) if !resolved.command.is_empty() => (resolved.command, resolved.proxy),
        Ok(_) => launched(),
        Err(error) => {
            eprintln!("warning: cannot resolve the restore command: {error:#}");
            launched()
        }
    }
}
