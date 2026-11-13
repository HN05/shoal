//! One declaration of each repository-overridable option drives both
//! layering and provenance, so a reported layer is the one whose value is in
//! effect.
use std::{collections::BTreeMap, sync::LazyLock};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::agent::Agent;
use crate::config::{
    Config,
    agents::AgentTool,
    modes,
    options::{self, Entry, Merge, Options},
    repo::{ConfigLayer as Layer, ConfigLayers, RepoConfig, Settings},
    same_as,
};

/// Settings after every layer, with each AI tool resolved from them and
/// what the machine adds to it.
#[derive(Debug, Serialize)]
pub struct Effective {
    #[serde(flatten)]
    settings: Settings,
    /// Every AI tool.
    #[serde(skip)]
    pub tools: crate::ai::Tools,
    /// Each agent's launcher, or the nearest of its bases'.
    #[serde(skip)]
    pub launchers: crate::config::named_commands::Commands,
    /// Each agent's command that restores a session.
    #[serde(skip)]
    pub resume_commands: crate::config::named_commands::Commands,
    /// The proxy each AI tool's tracked agents reach their upstream through.
    #[serde(skip)]
    pub proxies: BTreeMap<String, crate::config::proxies::Proxy>,
}

impl std::ops::Deref for Effective {
    type Target = Settings;

    fn deref(&self) -> &Settings {
        &self.settings
    }
}

impl std::ops::DerefMut for Effective {
    fn deref_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }
}

/// Every layer of one target, lowest first.
#[derive(Debug, Clone)]
pub struct Stack {
    layers: [(RepoConfig, Layer); 4],
    /// The file each template of a layer that reads them came from.
    template_files: [(modes::Files, Layer); 3],
    /// Settings a layer sets that repeat Shoal's guidance.
    warnings: Vec<String>,
}

impl Stack {
    pub fn new(global: &Config, repository: &ConfigLayers) -> Result<Self> {
        Self::over(
            global.repository_layer(),
            global.template_files.clone(),
            repository,
        )
    }

    /// The stack of `global`, the global config's repository layer with its
    /// templates read from `global_files`, under `repository`'s layers, with
    /// each key a `same_as` reference names holding its target's value;
    /// fails for a reference that cannot be followed.
    pub fn over(
        global: RepoConfig,
        global_files: modes::Files,
        repository: &ConfigLayers,
    ) -> Result<Self> {
        let mut configs = [
            RepoConfig::built_in(),
            global,
            repository.worktree_file.clone(),
            repository.local_config.clone(),
        ];
        follow_references(&mut configs)?;
        let [built_in, global, worktree_file, local_config] = configs;
        let layers = [
            (built_in, Layer::BuiltInDefault),
            (global, Layer::GlobalConfig),
            (worktree_file, Layer::WorktreeFile),
            (local_config, Layer::LocalConfig),
        ];
        let warnings = layers
            .iter()
            .flat_map(|(config, layer)| {
                (config.normalize().into_iter()).map(move |warning| format!("{layer}: {warning}"))
            })
            .collect();
        let template_files = [
            (global_files, Layer::GlobalConfig),
            (
                repository.worktree_template_files.clone(),
                Layer::WorktreeFile,
            ),
            (repository.local_template_files.clone(), Layer::LocalConfig),
        ];
        Ok(Self {
            layers,
            template_files,
            warnings,
        })
    }

    /// The settings in effect; fails when the layers combine into an invalid
    /// setting, such as a repository `ports.start` above the global `ports.end`.
    pub fn resolve(self) -> Result<Settings> {
        let configs = self.layers.map(|(config, _)| config);
        let (mut base, repository) = levels(&configs);
        // Each layer is valid alone; the repository's layered names must agree
        // too. A repository name that shadows a global one is allocation's
        // concern, so a conflict there never blocks the workspace's settings.
        crate::resources::definitions(&repository.resources, &repository.resource_pools)
            .context("layered repository config")?;
        base.merge(repository, Merge::RepositoryOverGlobal);
        let settings = base.resolve("")?;
        settings.ports.validate()?;
        Ok(settings)
    }

    /// Every option with its value and winning layer, and the file a
    /// template came from, in configuration order.
    pub fn report(&self) -> Result<Vec<Entry>> {
        self.clone().resolve()?;
        let mut entries = Vec::new();
        RepoConfig::entries("", &self.sources(), &mut entries)?;
        for entry in &mut entries {
            entry.file = self.template_file(entry.layer, &entry.key);
        }
        Ok(entries)
    }

    /// The file `layer` read the template `key` from.
    fn template_file(&self, layer: Layer, key: &str) -> Option<std::path::PathBuf> {
        self.template_files
            .iter()
            .find(|(_, files_layer)| *files_layer == layer)
            .and_then(|(files, _)| files.get(key))
            .cloned()
    }

    /// Warnings for the settings each layer sets that repeat Shoal's guidance.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The entries of one named table with the layer each came from.
    pub fn named<T: Clone>(
        &self,
        table: fn(&RepoConfig) -> &BTreeMap<String, T>,
    ) -> BTreeMap<String, (T, Layer)> {
        options::winners(&options::project(&self.sources(), table))
            .into_iter()
            .map(|(name, (value, layer))| (name.clone(), (value.clone(), layer)))
            .collect()
    }

    /// Highest layer first, the order provenance is looked up in.
    fn sources(&self) -> Vec<(&RepoConfig, Layer)> {
        self.layers
            .iter()
            .rev()
            .map(|(config, layer)| (config, *layer))
            .collect()
    }
}

/// Give each key a `same_as` reference of `layers`, the built-in default,
/// global config, worktree file and local config, names its target's final
/// value.
fn follow_references(layers: &mut [RepoConfig]) -> Result<()> {
    same_as::follow(
        layers,
        |config| &config.same_as,
        |layers| Ok(serde_json::to_value(merged(layers))?),
        "",
    )
}

/// `layers`, the built-in default, global config, worktree file and local
/// config, combined as written.
fn merged(layers: &[RepoConfig]) -> RepoConfig {
    let (mut base, repository) = levels(layers);
    base.merge(repository, Merge::RepositoryOverGlobal);
    base
}

/// The global level of `layers`, the built-in default and global config
/// merged, and the repository level, the worktree file and local config.
fn levels(layers: &[RepoConfig]) -> (RepoConfig, RepoConfig) {
    let level = |layers: &[RepoConfig]| {
        let mut layers = layers.iter().cloned();
        let mut merged = layers.next().unwrap_or_default();
        for layer in layers {
            merged.merge(layer, Merge::SameLevel);
        }
        merged
    };
    let (global, repository) = layers.split_at(2);
    (level(global), level(repository))
}

#[cfg(any(test, feature = "test-support"))]
impl RepoConfig {
    /// This config layered over `base`: an option set here wins, an omitted
    /// one falls through, and a named port, resource, pool or command
    /// replaces the one below it whole.
    pub fn over(self, mut base: Self) -> Self {
        base.merge(self, Merge::SameLevel);
        base
    }
}

impl Effective {
    pub fn new(
        settings: Settings,
        tools: crate::ai::Tools,
        proxies: BTreeMap<String, crate::config::proxies::Proxy>,
    ) -> Self {
        let commands = |command: fn(&AgentTool) -> &Option<Vec<String>>| {
            settings
                .agents
                .iter()
                .filter_map(|(name, agent)| Some((name.clone(), command(agent).clone()?)))
                .collect::<crate::config::named_commands::Commands>()
        };
        let mut launchers = commands(|agent| &agent.command);
        crate::ai::inherit_launchers(&tools, &mut launchers);
        let resume_commands = commands(|agent| &agent.resume_command);
        Self {
            settings,
            tools,
            launchers,
            resume_commands,
            proxies,
        }
    }

    pub fn into_settings(self) -> Settings {
        self.settings
    }

    /// `agent`'s launch profile; an agent no AI tool describes takes no
    /// arguments and changes nothing in its environment.
    pub fn launch(&self, agent: &Agent) -> &crate::ai::LaunchProfile {
        static NONE: LazyLock<crate::ai::LaunchProfile> = LazyLock::new(Default::default);
        self.tools
            .get(agent.name())
            .map_or(&NONE, |tool| &tool.launch)
    }

    /// Whether the new tab or session of a space started in `mode` takes
    /// the terminal: the mode's `focus`, else `terminal.focus`, while
    /// `None` follows the launch. The setup mode always does, since the
    /// user works with its agent straight away.
    pub fn focus(&self, mode: &modes::ModeName) -> Option<bool> {
        if *mode == modes::ModeName::SETUP {
            return Some(true);
        }
        let configured = self.modes.get(mode.as_str()).and_then(|mode| mode.focus);
        configured.or(self.terminal.focus)
    }

    /// How many minutes an agent of `mode` may stay idle before Shoal wakes
    /// it; none when its mode never does. A mode only a Markdown file names,
    /// which the daemon's settings leave out, sets none.
    pub fn idle_wake_minutes(&self, mode: crate::config::permissions::SpaceMode) -> Option<u64> {
        let mode = modes::resolve(&self.modes, mode.name, mode.kind, mode.repository).ok()?;
        Some(mode.idle_wake_minutes).filter(|minutes| *minutes > 0)
    }

    /// The model and effort a launch of `agent` runs with: each
    /// `requested` value, else `[agent] model` and `effort` when `[agent]
    /// default` names it, else its AI tool's.
    pub fn model(
        &self,
        agent: &Agent,
        requested: crate::agent::ModelChoice,
    ) -> crate::agent::ModelChoice {
        let chosen = match &self.agent.default {
            Some(default) if default == agent => crate::agent::ModelChoice {
                model: self.agent.model.clone(),
                effort: self.agent.effort.clone(),
            },
            _ => Default::default(),
        };
        let configured = (self.tools.get(agent.name())).map(|tool| tool.model.clone());
        requested.or(chosen).or(configured.unwrap_or_default())
    }

    /// Refuse `agent` as a reviewer unless it is an AI tool with a review
    /// template that config does not turn off.
    pub fn check_reviewer(&self, agent: &Agent) -> anyhow::Result<()> {
        let name = agent.name();
        anyhow::ensure!(
            !self.turned_off(agent),
            "{name} is turned off: its [agents] entry sets enabled = false"
        );
        let tool = self
            .tools
            .get(name)
            .filter(|tool| tool.runs_as_agent)
            .with_context(|| format!("{name} is not an AI tool, so it cannot review"))?;
        anyhow::ensure!(
            tool.launch.arguments.review.is_some(),
            "{name} has no review template; set agents.{name}.review to review with it"
        );
        Ok(())
    }

    /// Whether `agent`'s own `[agents]` entry turns it off with `enabled =
    /// false`.
    pub fn turned_off(&self, agent: &Agent) -> bool {
        (self.agents.get(agent.name())).and_then(|agent| agent.enabled) == Some(false)
    }

    /// The integration `agent` runs with; none for an agent no AI tool
    /// describes.
    pub fn integration(&self, agent: &Agent) -> crate::ai::IntegrationKind {
        self.tools
            .get(agent.name())
            .map_or(crate::ai::IntegrationKind::None, |tool| tool.integration)
    }

    /// The proxy `agent`'s tracked launches reach their upstream through.
    pub fn proxy(&self, agent: &Agent) -> Option<&crate::config::proxies::Proxy> {
        self.proxies.get(agent.name())
    }
}

#[cfg(test)]
mod tests;
