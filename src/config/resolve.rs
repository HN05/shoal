//! One table of repository-overridable options drives both layering and
//! provenance, so a reported layer is the one whose value is in effect.
use std::{collections::BTreeMap, sync::LazyLock};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::{
    config::{
        self, AutoCleanup, Codex, Config, Ports, PrCleanup, Simulators, named_commands,
        repo::{ConfigLayer as Layer, ConfigLayers, RepoConfig},
    },
    hooks::HookKind,
};

/// Settings a repository may set, after every layer: a repository value
/// wins, an omitted one keeps the global value or the built-in default.
#[derive(Debug, Serialize)]
pub struct Effective {
    pub commands: named_commands::Commands,
    pub issue_template: Option<String>,
    pub agent_template: Option<String>,
    pub agent_auth: crate::agent_auth::Config,
    pub git_profile: Option<String>,
    pub default_agent: Option<crate::agent::Agent>,
    pub codex: Codex,
    pub setup_cmd: Option<String>,
    pub pre_setup_cmd: Option<String>,
    pub post_remove_cmd: Option<String>,
    pub post_resource_acquire_cmd: Option<String>,
    pub pre_resource_release_cmd: Option<String>,
    pub post_setup_cmd: Option<String>,
    pub pre_remove_cmd: Option<String>,
    pub ports: Ports,
    pub resources: BTreeMap<String, crate::daemon::resources::ResourceConfig>,
    pub resource_pools: BTreeMap<String, crate::daemon::resources::PoolConfig>,
    pub simulators: Simulators,
    pub auto_cleanup: AutoCleanup,
    pub pr_cleanup: PrCleanup,
}

/// One effective value and the layer that supplied it.
#[derive(Debug, Serialize)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    pub layer: Layer,
}

/// Every layer of one target, lowest first.
#[derive(Debug, Clone)]
pub struct Stack {
    layers: [(RepoConfig, Layer); 4],
}

impl Stack {
    pub fn new(global: &Config, repository: &ConfigLayers) -> Self {
        Self {
            layers: [
                (built_in(), Layer::BuiltInDefault),
                (global.repository_layer(), Layer::GlobalConfig),
                (repository.worktree_file.clone(), Layer::WorktreeFile),
                (
                    repository.saved_repository_config.clone(),
                    Layer::SavedRepositoryConfig,
                ),
            ],
        }
    }

    /// The settings in effect; fails when the layers combine into an invalid
    /// setting, such as a repository `ports.start` above the global `ports.end`.
    pub fn resolve(self) -> Result<Effective> {
        let [(built_in, _), (global, _), (worktree_file, _), (saved, _)] = self.layers;
        let repository = saved.over(worktree_file);
        // Each layer is valid alone; the repository's layered names must agree
        // too. A repository name that shadows a global one is allocation's
        // concern, so a conflict there never blocks the workspace's settings.
        crate::daemon::resources::definitions(&repository.resources, &repository.resource_pools)
            .context("layered repository config")?;
        Effective::from_merged(repository.over(global.over(built_in)))
    }

    /// Every option with its value and winning layer, in configuration order.
    pub fn report(&self) -> Result<Vec<Entry>> {
        self.clone().resolve()?;
        let sources = self.sources();
        let mut entries = Vec::new();
        for field in fields() {
            field.entries(&sources, &mut entries)?;
        }
        Ok(entries)
    }

    /// The entries of one named table with the layer each came from.
    pub fn named<T: Clone>(
        &self,
        table: fn(&RepoConfig) -> &BTreeMap<String, T>,
    ) -> BTreeMap<String, (T, Layer)> {
        winners(&self.sources(), table)
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

impl RepoConfig {
    /// This config layered over `base`: an option set here wins, an omitted
    /// one falls through, and a named port, resource, pool or command
    /// replaces the one below it whole.
    pub fn over(mut self, mut base: Self) -> Self {
        for field in fields() {
            field.layer(&mut self, &mut base);
        }
        base
    }
}

impl Effective {
    fn from_merged(merged: RepoConfig) -> Result<Self> {
        fn built_in<T>(value: Option<T>, key: &str) -> Result<T> {
            value.with_context(|| format!("{key} has no built-in default"))
        }
        let ports = Ports {
            start: built_in(merged.ports.start, "ports.start")?,
            end: built_in(merged.ports.end, "ports.end")?,
            on_conflict: built_in(merged.ports.on_conflict, "ports.on_conflict")?,
            definitions: merged.ports.definitions,
        };
        ports.validate()?;
        Ok(Self {
            commands: merged.commands,
            issue_template: merged.issue_template,
            agent_template: merged.agent_template,
            agent_auth: merged.agent_auth,
            git_profile: merged.git_profile,
            default_agent: merged.default_agent,
            codex: Codex {
                default_mode: built_in(merged.codex.default_mode, "codex.default_mode")?,
            },
            setup_cmd: merged.setup_cmd,
            pre_setup_cmd: merged.pre_setup_cmd,
            post_remove_cmd: merged.post_remove_cmd,
            post_resource_acquire_cmd: merged.post_resource_acquire_cmd,
            pre_resource_release_cmd: merged.pre_resource_release_cmd,
            post_setup_cmd: merged.post_setup_cmd,
            pre_remove_cmd: merged.pre_remove_cmd,
            ports,
            resources: merged.resources,
            resource_pools: merged.resource_pools,
            simulators: Simulators {
                requires_approval: built_in(
                    merged.simulators.requires_approval,
                    "simulators.requires_approval",
                )?,
                approval_lifetime: built_in(
                    merged.simulators.approval_lifetime,
                    "simulators.approval_lifetime",
                )?,
                preferred: merged.simulators.preferred,
            },
            auto_cleanup: AutoCleanup {
                enabled: built_in(merged.auto_cleanup.enabled, "auto_cleanup.enabled")?,
                idle_minutes: built_in(
                    merged.auto_cleanup.idle_minutes,
                    "auto_cleanup.idle_minutes",
                )?,
            },
            pr_cleanup: PrCleanup {
                enabled: built_in(merged.pr_cleanup.enabled, "pr_cleanup.enabled")?,
            },
        })
    }
}

/// The lowest layer: the typed defaults, stated where a repository could
/// override them.
fn built_in() -> RepoConfig {
    let ports = Ports::default();
    let simulators = Simulators::default();
    let auto_cleanup = AutoCleanup::default();
    RepoConfig {
        commands: named_commands::defaults(),
        codex: config::repo::Codex {
            default_mode: Some(Codex::default().default_mode),
        },
        ports: config::repo::PortDefaults {
            on_conflict: Some(ports.on_conflict),
            start: Some(ports.start),
            end: Some(ports.end),
            definitions: BTreeMap::new(),
        },
        simulators: config::repo::SimulatorPreferences {
            requires_approval: Some(simulators.requires_approval),
            approval_lifetime: Some(simulators.approval_lifetime),
            preferred: simulators.preferred,
        },
        auto_cleanup: config::repo::AutoCleanup {
            enabled: Some(auto_cleanup.enabled),
            idle_minutes: Some(auto_cleanup.idle_minutes),
        },
        pr_cleanup: config::repo::PrCleanup {
            enabled: Some(PrCleanup::default().enabled),
        },
        ..Default::default()
    }
}

/// One option's merge rule and its provenance lookup.
trait Field {
    /// Move `top`'s value for this option onto `base` where `top` sets it.
    fn layer(&self, top: &mut RepoConfig, base: &mut RepoConfig);
    /// Append the effective value(s) with the layer each came from.
    fn entries(&self, sources: &[(&RepoConfig, Layer)], entries: &mut Vec<Entry>) -> Result<()>;
}

/// An option a layer either sets or omits.
struct Scalar<T> {
    key: &'static str,
    get: fn(&RepoConfig) -> &Option<T>,
    get_mut: fn(&mut RepoConfig) -> &mut Option<T>,
}

impl<T: Serialize> Field for Scalar<T> {
    fn layer(&self, top: &mut RepoConfig, base: &mut RepoConfig) {
        if let Some(value) = (self.get_mut)(top).take() {
            *(self.get_mut)(base) = Some(value);
        }
    }

    fn entries(&self, sources: &[(&RepoConfig, Layer)], entries: &mut Vec<Entry>) -> Result<()> {
        let found = sources
            .iter()
            .find_map(|(config, layer)| (self.get)(config).as_ref().map(|value| (value, *layer)));
        let (value, layer) = match found {
            Some((value, layer)) => (serde_json::to_value(value)?, layer),
            None => (Value::Null, Layer::BuiltInDefault),
        };
        entries.push(Entry {
            key: self.key.into(),
            value,
            layer,
        });
        Ok(())
    }
}

/// A table whose entries replace the same-named entry below them whole.
struct Named<T> {
    prefix: &'static str,
    get: fn(&RepoConfig) -> &BTreeMap<String, T>,
    get_mut: fn(&mut RepoConfig) -> &mut BTreeMap<String, T>,
}

impl<T: Serialize> Field for Named<T> {
    fn layer(&self, top: &mut RepoConfig, base: &mut RepoConfig) {
        (self.get_mut)(base).extend(std::mem::take((self.get_mut)(top)));
    }

    fn entries(&self, sources: &[(&RepoConfig, Layer)], entries: &mut Vec<Entry>) -> Result<()> {
        for (name, (value, layer)) in winners(sources, self.get) {
            entries.push(Entry {
                key: format!("{}.{name}", self.prefix),
                value: serde_json::to_value(value)?,
                layer,
            });
        }
        Ok(())
    }
}

/// A list an empty value leaves to the layer below.
struct List<T> {
    key: &'static str,
    get: fn(&RepoConfig) -> &Vec<T>,
    get_mut: fn(&mut RepoConfig) -> &mut Vec<T>,
}

impl<T: Serialize> Field for List<T> {
    fn layer(&self, top: &mut RepoConfig, base: &mut RepoConfig) {
        if !(self.get_mut)(top).is_empty() {
            *(self.get_mut)(base) = std::mem::take((self.get_mut)(top));
        }
    }

    fn entries(&self, sources: &[(&RepoConfig, Layer)], entries: &mut Vec<Entry>) -> Result<()> {
        let (config, layer) = sources
            .iter()
            .find(|(config, _)| !(self.get)(config).is_empty())
            .or(sources.last())
            .context("no configuration layers")?;
        entries.push(Entry {
            key: self.key.into(),
            value: serde_json::to_value((self.get)(config))?,
            layer: *layer,
        });
        Ok(())
    }
}

/// A lifecycle hook from the typed hook table.
struct Hook(HookKind);

impl Field for Hook {
    fn layer(&self, top: &mut RepoConfig, base: &mut RepoConfig) {
        if let Some(command) = self.0.repository_command_mut(top).take() {
            *self.0.repository_command_mut(base) = Some(command);
        }
    }

    fn entries(&self, sources: &[(&RepoConfig, Layer)], entries: &mut Vec<Entry>) -> Result<()> {
        let found = sources.iter().find_map(|(config, layer)| {
            self.0
                .repository_command(config)
                .map(|command| (command, *layer))
        });
        let (value, layer) = match found {
            Some((command, layer)) => (Value::String(command.clone()), layer),
            None => (Value::Null, Layer::BuiltInDefault),
        };
        entries.push(Entry {
            key: self.0.key().into(),
            value,
            layer,
        });
        Ok(())
    }
}

fn winners<'a, T>(
    sources: &[(&'a RepoConfig, Layer)],
    table: fn(&RepoConfig) -> &BTreeMap<String, T>,
) -> BTreeMap<&'a String, (&'a T, Layer)> {
    let mut result = BTreeMap::new();
    for (config, layer) in sources.iter().rev() {
        for (name, value) in table(config) {
            result.insert(name, (value, *layer));
        }
    }
    result
}

macro_rules! scalar {
    ($($path:ident).+) => {
        Box::new(Scalar {
            key: stringify!($($path).+),
            get: |config| &config.$($path).+,
            get_mut: |config| &mut config.$($path).+,
        })
    };
}

macro_rules! named {
    ($prefix:literal, $($path:ident).+) => {
        Box::new(Named {
            prefix: $prefix,
            get: |config| &config.$($path).+,
            get_mut: |config| &mut config.$($path).+,
        })
    };
}

macro_rules! list {
    ($($path:ident).+) => {
        Box::new(List {
            key: stringify!($($path).+),
            get: |config| &config.$($path).+,
            get_mut: |config| &mut config.$($path).+,
        })
    };
}

/// Every repository-overridable option, in the order `config show` lists them.
fn fields() -> &'static [Box<dyn Field + Send + Sync>] {
    static FIELDS: LazyLock<Vec<Box<dyn Field + Send + Sync>>> = LazyLock::new(build_fields);
    &FIELDS
}

fn build_fields() -> Vec<Box<dyn Field + Send + Sync>> {
    let mut fields: Vec<Box<dyn Field + Send + Sync>> = vec![
        named!("commands", commands),
        scalar!(issue_template),
        scalar!(agent_template),
        scalar!(agent_auth.fj),
        scalar!(agent_auth.gh),
        scalar!(git_profile),
        scalar!(default_agent),
        scalar!(codex.default_mode),
    ];
    fields.extend(
        HookKind::ALL
            .iter()
            .map(|&kind| Box::new(Hook(kind)) as Box<dyn Field + Send + Sync>),
    );
    let rest: Vec<Box<dyn Field + Send + Sync>> = vec![
        scalar!(ports.on_conflict),
        scalar!(ports.start),
        scalar!(ports.end),
        named!("ports", ports.definitions),
        named!("resources", resources),
        named!("resource_pools", resource_pools),
        scalar!(simulators.requires_approval),
        scalar!(simulators.approval_lifetime),
        list!(simulators.preferred),
        scalar!(auto_cleanup.enabled),
        scalar!(auto_cleanup.idle_minutes),
        scalar!(pr_cleanup.enabled),
    ];
    fields.extend(rest);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::repo::parse;

    /// Every option set, so a field missing from the table shows up as a
    /// value that does not survive layering. Adding a field to `RepoConfig`
    /// fails the destructuring below until the fixture states it too.
    const FULL: &str = "\
issue_template = 'issue'\nagent_template = 'agent'\ngit_profile = 'work'\n\
default_agent = 'claude'\nsetup_cmd = 'setup'\npre_setup_cmd = 'pre-setup'\n\
post_remove_cmd = 'post-remove'\npost_resource_acquire_cmd = 'acquire'\n\
pre_resource_release_cmd = 'release'\npost_setup_cmd = 'attach'\npre_remove_cmd = 'detach'\n\
[commands]\nreview = ['review']\n[agent_auth]\nfj = '/fj'\ngh = '/gh'\n[codex]\ndefault_mode = 'app'\n\
[ports]\non_conflict = 'auto'\nstart = 3000\nend = 3100\n[ports.web]\nport = 3000\n\
[resources.lock]\ncapacity = 1\n[resource_pools.devices]\ncapacity = 2\n\
[resource_pools.devices.resources.phone]\ncapacity = 1\n\
[simulators]\nrequires_approval = true\napproval_lifetime = 'workspace'\npreferred = ['phone']\n\
[auto_cleanup]\nenabled = false\nidle_minutes = 30\n[pr_cleanup]\nenabled = false\n";

    fn json<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }

    fn stack(global: &str, worktree: &str, saved: &str) -> Stack {
        let global: Config = toml::from_str(global).unwrap();
        let layers = ConfigLayers {
            worktree_file: parse(worktree).unwrap(),
            saved_repository_config: parse(saved).unwrap(),
        };
        Stack::new(&global, &layers)
    }

    #[test]
    fn every_option_survives_layering_in_both_directions() {
        let full = parse(FULL).unwrap();
        let RepoConfig {
            commands,
            issue_template,
            agent_template,
            agent_auth,
            git_profile,
            default_agent,
            codex,
            setup_cmd,
            pre_setup_cmd,
            post_remove_cmd,
            post_resource_acquire_cmd,
            pre_resource_release_cmd,
            post_setup_cmd,
            pre_remove_cmd,
            ports,
            resources,
            resource_pools,
            simulators,
            auto_cleanup,
            pr_cleanup,
        } = full.clone();
        assert!(!commands.is_empty() && !resources.is_empty() && !resource_pools.is_empty());
        assert!(!ports.definitions.is_empty() && !simulators.preferred.is_empty());
        for present in [
            issue_template.is_some(),
            agent_template.is_some(),
            agent_auth.fj.is_some(),
            agent_auth.gh.is_some(),
            git_profile.is_some(),
            default_agent.is_some(),
            codex.default_mode.is_some(),
            setup_cmd.is_some(),
            pre_setup_cmd.is_some(),
            post_remove_cmd.is_some(),
            post_resource_acquire_cmd.is_some(),
            pre_resource_release_cmd.is_some(),
            post_setup_cmd.is_some(),
            pre_remove_cmd.is_some(),
            ports.on_conflict.is_some(),
            ports.start.is_some(),
            ports.end.is_some(),
            simulators.requires_approval.is_some(),
            simulators.approval_lifetime.is_some(),
            auto_cleanup.enabled.is_some(),
            auto_cleanup.idle_minutes.is_some(),
            pr_cleanup.enabled.is_some(),
        ] {
            assert!(present, "the fixture must set every option");
        }
        assert_eq!(json(&full.clone().over(RepoConfig::default())), json(&full));
        assert_eq!(json(&RepoConfig::default().over(full.clone())), json(&full));
        // The same values reported from any single layer name that layer.
        for (index, layer) in [
            (1, Layer::GlobalConfig),
            (2, Layer::WorktreeFile),
            (3, Layer::SavedRepositoryConfig),
        ] {
            let mut stack = stack("", "", "");
            stack.layers[index].0 = full.clone();
            for entry in stack.report().unwrap() {
                let built_in_command = entry
                    .key
                    .strip_prefix("commands.")
                    .is_some_and(|name| named_commands::defaults().contains_key(name));
                if !built_in_command {
                    assert_eq!(entry.layer, layer, "{}", entry.key);
                }
            }
        }
    }

    #[test]
    fn reported_provenance_names_the_layer_whose_value_is_in_effect() {
        let cases = [
            ("", "", ""),
            // Explicit defaults win their layer; omitted values fall through.
            (
                "[auto_cleanup]\nenabled = true\n[codex]\ndefault_mode = 'cli'\n",
                "[auto_cleanup]\nidle_minutes = 5\n",
                "",
            ),
            // Partial nested tables merge per option across all three layers.
            (
                "[ports]\nstart = 3000\nend = 4000\n[commands]\nreview = ['global']\ncheck = ['check']\n",
                "[ports]\nstart = 3500\n[ports.web]\nport = 3600\n[commands]\nreview = ['file']\n",
                "[ports]\nend = 3900\n[ports.web]\nenv = 'WEB'\n[commands]\nreview = ['saved']\n\
                 [simulators]\npreferred = ['tablet']\n[agent_auth]\nfj = '/saved/fj'\n",
            ),
            (
                "[agent_auth]\ngh = '/global/gh'\n[resources.lock]\ncapacity = 1\n",
                "[simulators]\npreferred = ['phone']\nrequires_approval = true\n",
                "[resources.lock]\ncapacity = 3\n[pr_cleanup]\nenabled = false\n",
            ),
        ];
        for (global, worktree, saved) in cases {
            let stack = stack(global, worktree, saved);
            let effective = json(&stack.clone().resolve().unwrap());
            let entries = stack.report().unwrap();
            assert!(!entries.is_empty());
            for entry in &entries {
                let pointer = format!("/{}", entry.key.replace('.', "/"));
                let in_effect = effective.pointer(&pointer).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    entry.value, in_effect,
                    "{global}|{worktree}|{saved}: {pointer}"
                );
                // Nothing below the reported layer decides the value: the
                // same stack without the higher layers reports it unchanged.
                let layer_of = |layer: Layer| stack.layers.iter().position(|(_, l)| *l == layer);
                let mut lower = stack.clone();
                for (config, layer) in &mut lower.layers {
                    if layer_of(*layer) > layer_of(entry.layer) {
                        *config = RepoConfig::default();
                    }
                }
                let below = lower.report().unwrap();
                let same = below.iter().find(|e| e.key == entry.key).unwrap();
                assert_eq!(
                    (&same.value, same.layer),
                    (&entry.value, entry.layer),
                    "{pointer}"
                );
            }
        }
        let entries = stack(
            "[auto_cleanup]\nenabled = true\n",
            "[auto_cleanup]\nidle_minutes = 5\n",
            "",
        )
        .report()
        .unwrap();
        let entry = |key: &str| entries.iter().find(|e| e.key == key).unwrap();
        assert_eq!(entry("auto_cleanup.enabled").layer, Layer::GlobalConfig);
        assert_eq!(
            entry("auto_cleanup.idle_minutes").layer,
            Layer::WorktreeFile
        );
        assert_eq!(entry("codex.default_mode").layer, Layer::BuiltInDefault);
        assert_eq!(entry("codex.default_mode").value, "cli");
        assert_eq!(entry("setup_cmd").value, Value::Null);
        assert_eq!(
            entry("simulators.preferred").value,
            json(&Vec::<String>::new())
        );
    }

    #[test]
    fn conflicting_ranges_fail_after_the_layers_combine() {
        for (global, worktree, saved) in [
            (
                "[ports]\nstart = 3000\nend = 3100\n",
                "",
                "[ports]\nstart = 3200\n",
            ),
            (
                "[ports]\nstart = 3000\nend = 3100\n",
                "[ports]\nend = 2000\n",
                "",
            ),
            ("", "[ports]\nstart = 5000\n", "[ports]\nend = 4000\n"),
            (
                "",
                "[resource_pools.lock]\ncapacity = 2\n[resource_pools.lock.resources.a]\ncapacity = 1\n",
                "[resources.lock]\ncapacity = 1\n",
            ),
        ] {
            let stack = stack(global, worktree, saved);
            assert!(
                stack.clone().resolve().is_err(),
                "{global}|{worktree}|{saved}"
            );
            assert!(stack.report().is_err(), "{global}|{worktree}|{saved}");
        }
        let split = stack(
            "[ports]\nstart = 3000\nend = 3100\n",
            "",
            "[ports]\nstart = 3050\n",
        );
        let ports = split.resolve().unwrap().ports;
        assert_eq!((ports.start, ports.end), (3050, 3100));
        // A repository name shadowing a global one is checked at allocation,
        // so it never blocks the settings that hooks and removal read.
        let shadowed = stack(
            "[resource_pools.lock]\ncapacity = 2\n[resource_pools.lock.resources.a]\ncapacity = 1\n",
            "[resources.lock]\ncapacity = 1\n",
            "",
        );
        assert!(shadowed.resolve().is_ok());
    }

    #[test]
    fn hooks_resolve_by_kind_through_the_settings() {
        let settings = stack(
            "pre_setup_cmd = 'global/pre'\npost_remove_cmd = 'global/remove'\n",
            "post_setup_cmd = 'scripts/attach.sh'\npre_setup_cmd = 'file/pre'\n",
            "pre_remove_cmd = '/opt/detach'\n",
        )
        .resolve()
        .unwrap();
        for (kind, expected) in [
            (HookKind::Setup, None),
            (HookKind::PreSetup, Some("file/pre")),
            (HookKind::PostSetup, Some("scripts/attach.sh")),
            (HookKind::PreRemove, Some("/opt/detach")),
            (HookKind::PostRemove, Some("global/remove")),
            (HookKind::PostResourceAcquire, None),
        ] {
            assert_eq!(
                kind.command(&settings).map(String::as_str),
                expected,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn named_tables_report_replacement_by_name() {
        let stack = stack(
            "[commands]\nreview = ['global']\ncheck = ['check']\n",
            "[commands]\nreview = ['file']\nlint = ['lint']\n",
            "[commands]\nreview = ['saved', 'two words']\n",
        );
        let entries = stack.report().unwrap();
        let command = |name: &str| {
            entries
                .iter()
                .find(|entry| entry.key == format!("commands.{name}"))
                .map(|entry| (entry.value.clone(), entry.layer))
                .unwrap()
        };
        assert_eq!(
            command("review"),
            (json(&["saved", "two words"]), Layer::SavedRepositoryConfig)
        );
        assert_eq!(command("check").1, Layer::GlobalConfig);
        assert_eq!(command("lint").1, Layer::WorktreeFile);
        assert_eq!(command("codex").1, Layer::BuiltInDefault);
        let commands = stack.named(|config| &config.commands);
        for (name, (value, layer)) in &commands {
            assert_eq!((json(value), *layer), command(name), "{name}");
        }
        assert_eq!(commands.len(), 5);
        let effective = stack.resolve().unwrap();
        assert_eq!(effective.commands["review"], ["saved", "two words"]);
        assert_eq!(effective.commands["check"], ["check"]);
        assert!(effective.commands.contains_key("claude"));
    }
}
