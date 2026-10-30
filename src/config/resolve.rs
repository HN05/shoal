//! One table of repository-overridable options drives both layering and
//! provenance, so a reported layer is the one whose value is in effect.
use std::{collections::BTreeMap, sync::LazyLock};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::{
    config::{
        self, Api, Approvals, AutoCleanup, Cleanup, Config, Done, IdleStop, Issue, Land,
        MergedCleanup, Ports, Pr, PrCleanup, Ready, Restore, Review, Shortcut, Simulators, Unready,
        named_commands,
        repo::{ConfigLayer as Layer, ConfigLayers, RepoConfig},
    },
    hooks::HookKind,
};

/// Settings after every layer, including repository additions to global templates.
#[derive(Debug, Serialize)]
pub struct Effective {
    pub commands: named_commands::Commands,
    /// Every AI tool; machine config, so no layer overrides it.
    #[serde(skip)]
    pub tools: crate::ai::Tools,
    /// The proxy each AI tool's tracked agents reach their upstream through.
    #[serde(skip)]
    pub proxies: BTreeMap<String, crate::proxy::Proxy>,
    pub agent_resume: named_commands::Commands,
    pub worker_template: Option<String>,
    pub chat_template: Option<String>,
    pub agent_auth: crate::agent_auth::Config,
    pub git_profile: Option<String>,
    pub default_agent: Option<crate::agent::Agent>,
    pub max_agents: Option<config::AgentLimit>,
    pub focus: Option<bool>,
    pub close_when_done: bool,
    pub default_host: crate::sessions::HostName,
    pub claude: Shortcut,
    pub codex: Shortcut,
    pub herdr: config::Herdr,
    pub rex: config::Rex,
    pub happy: config::Happy,
    pub copy_ignored: bool,
    pub agent_dirs: Vec<String>,
    pub setup_cmd: Option<String>,
    pub pre_setup_cmd: Option<String>,
    pub post_remove_cmd: Option<String>,
    pub post_done_cmd: Option<String>,
    pub post_ready_cmd: Option<String>,
    pub post_agent_exit_cmd: Option<String>,
    pub post_resource_acquire_cmd: Option<String>,
    pub pre_resource_release_cmd: Option<String>,
    pub post_setup_cmd: Option<String>,
    pub pre_remove_cmd: Option<String>,
    pub ports: Ports,
    pub env: BTreeMap<String, String>,
    pub caches: BTreeMap<String, config::repo::Cache>,
    pub demos: BTreeMap<String, crate::demos::Demo>,
    /// Unset when no layer names presets.
    pub presets: Option<Vec<crate::presets::Preset>>,
    pub secrets: crate::secrets::Secrets,
    pub resources: BTreeMap<String, crate::daemon::resources::ResourceConfig>,
    pub resource_pools: BTreeMap<String, crate::daemon::resources::PoolConfig>,
    pub simulators: Simulators,
    pub cleanup: Cleanup,
    pub done: Done,
    pub idle_stop: IdleStop,
    pub review: Review,
    pub ready: Ready,
    pub unready: Unready,
    pub pr: Pr,
    pub issue: Issue,
    pub land: Land,
    pub approvals: Approvals,
    pub api: Api,
    pub seed: config::repo::Seed,
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
    /// Settings a layer sets that repeat Shoal's guidance.
    warnings: Vec<String>,
}

impl Stack {
    pub fn new(global: &Config, repository: &ConfigLayers) -> Self {
        let mut warnings = Vec::new();
        let mut normalize = |config: RepoConfig, layer: Layer| {
            warnings.extend(
                config
                    .normalize()
                    .into_iter()
                    .map(|warning| format!("{layer}: {warning}")),
            );
            (config, layer)
        };
        let layers = [
            normalize(built_in(), Layer::BuiltInDefault),
            normalize(global.repository_layer(), Layer::GlobalConfig),
            normalize(repository.worktree_file.clone(), Layer::WorktreeFile),
            normalize(
                repository.saved_repository_config.clone(),
                Layer::SavedRepositoryConfig,
            ),
        ];
        Self { layers, warnings }
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
        Effective::from_merged(repository.over_global(global.over(built_in)))
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

    /// Warnings for the settings each layer sets that repeat Shoal's guidance.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
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

    fn over_global(mut self, mut base: Self) -> Self {
        for field in fields() {
            field.layer_repository(&mut self, &mut base);
        }
        base
    }
}

impl Effective {
    /// Agent `name`'s launch profile; an agent no AI tool describes takes no
    /// arguments and changes nothing in its environment.
    pub fn launch(&self, name: &str) -> &crate::ai::LaunchProfile {
        static NONE: LazyLock<crate::ai::LaunchProfile> = LazyLock::new(Default::default);
        self.tools.get(name).map_or(&NONE, |tool| &tool.launch)
    }

    /// The integration agent `name` runs with; none for an agent no AI tool
    /// describes.
    pub fn integration(&self, name: &str) -> crate::ai::IntegrationKind {
        self.tools
            .get(name)
            .map_or(crate::ai::IntegrationKind::None, |tool| tool.integration)
    }

    /// The proxy agent `name`'s tracked launches reach their upstream
    /// through.
    pub fn proxy(&self, name: &str) -> Option<&crate::proxy::Proxy> {
        self.proxies.get(name)
    }

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
        let presets = merged.presets;
        let named = presets.as_deref().unwrap_or_default();
        Ok(Self {
            commands: merged.commands,
            tools: Default::default(),
            proxies: Default::default(),
            agent_resume: merged.agent_resume,
            worker_template: merged.worker_template,
            chat_template: merged.chat_template,
            agent_auth: merged.agent_auth,
            git_profile: merged.git_profile,
            default_agent: merged.default_agent,
            max_agents: merged.max_agents,
            focus: merged.focus,
            close_when_done: built_in(merged.close_when_done, "close_when_done")?,
            herdr: config::Herdr {
                enabled: built_in(merged.herdr.enabled, "herdr.enabled")?,
                tab_name: merged.herdr.tab_name,
                new_tab: built_in(merged.herdr.new_tab, "herdr.new_tab")?,
                workspace_name: built_in(merged.herdr.workspace_name, "herdr.workspace_name")?,
                close_when_done: merged.herdr.close_when_done,
            },
            default_host: built_in(merged.default_host, "default_host")?.into(),
            rex: config::Rex {
                enabled: built_in(merged.rex.enabled, "rex.enabled")?,
                new_window: built_in(merged.rex.new_window, "rex.new_window")?,
                placement: built_in(merged.rex.placement, "rex.placement")?,
                window_name: merged.rex.window_name,
                session_name: built_in(merged.rex.session_name, "rex.session_name")?,
                close_when_done: merged.rex.close_when_done,
            },
            happy: config::Happy {
                enabled: built_in(merged.happy.enabled, "happy.enabled")?,
            },
            claude: Shortcut {
                default_mode: built_in(merged.claude.default_mode, "claude.default_mode")?,
            },
            codex: Shortcut {
                default_mode: built_in(merged.codex.default_mode, "codex.default_mode")?,
            },
            copy_ignored: built_in(merged.copy_ignored, "copy_ignored")?,
            agent_dirs: built_in(merged.agent_dirs, "agent_dirs")?,
            setup_cmd: merged.setup_cmd,
            pre_setup_cmd: merged.pre_setup_cmd,
            post_remove_cmd: merged.post_remove_cmd,
            post_done_cmd: merged.post_done_cmd,
            post_ready_cmd: merged.post_ready_cmd,
            post_agent_exit_cmd: merged.post_agent_exit_cmd,
            post_resource_acquire_cmd: merged.post_resource_acquire_cmd,
            pre_resource_release_cmd: merged.pre_resource_release_cmd,
            post_setup_cmd: merged.post_setup_cmd,
            pre_remove_cmd: merged.pre_remove_cmd,
            ports,
            env: crate::presets::environment(named, merged.env),
            caches: crate::presets::caches(named, merged.caches),
            demos: merged.demos,
            presets,
            secrets: merged.secrets,
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
            cleanup: Cleanup {
                grace_minutes: built_in(merged.cleanup.grace_minutes, "cleanup.grace_minutes")?,
                auto: AutoCleanup {
                    enabled: built_in(merged.cleanup.auto.enabled, "cleanup.auto.enabled")?,
                    idle_minutes: built_in(
                        merged.cleanup.auto.idle_minutes,
                        "cleanup.auto.idle_minutes",
                    )?,
                },
                pr: PrCleanup {
                    enabled: built_in(merged.cleanup.pr.enabled, "cleanup.pr.enabled")?,
                },
                merged: MergedCleanup {
                    enabled: built_in(merged.cleanup.merged.enabled, "cleanup.merged.enabled")?,
                    idle_minutes: built_in(
                        merged.cleanup.merged.idle_minutes,
                        "cleanup.merged.idle_minutes",
                    )?,
                },
                restore: Restore {
                    retention_days: built_in(
                        merged.cleanup.restore.retention_days,
                        "cleanup.restore.retention_days",
                    )?,
                    archive_max_mb: built_in(
                        merged.cleanup.restore.archive_max_mb,
                        "cleanup.restore.archive_max_mb",
                    )?,
                },
            },
            done: Done {
                cleanup: built_in(merged.done.cleanup, "done.cleanup")?,
            },
            idle_stop: IdleStop {
                enabled: built_in(merged.idle_stop.enabled, "idle_stop.enabled")?,
            },
            review: Review {
                post: built_in(merged.review.post, "review.post")?,
            },
            ready: Ready {
                undraft: built_in(merged.ready.undraft, "ready.undraft")?,
            },
            unready: Unready {
                draft: built_in(merged.unready.draft, "unready.draft")?,
            },
            pr: Pr {
                draft: built_in(merged.pr.draft, "pr.draft")?,
                required_labels: built_in(merged.pr.required_labels, "pr.required_labels")?,
            },
            issue: Issue {
                required_labels: built_in(merged.issue.required_labels, "issue.required_labels")?,
                other_repositories: built_in(
                    merged.issue.other_repositories,
                    "issue.other_repositories",
                )?,
                plan_label: merged.issue.plan_label,
                planned_label: merged.issue.planned_label,
            },
            land: Land {
                push: built_in(merged.land.push, "land.push")?,
                requires_approval: built_in(
                    merged.land.requires_approval,
                    "land.requires_approval",
                )?,
            },
            approvals: Approvals {
                issue_open: built_in(merged.approvals.issue_open, "approvals.issue_open")?,
                pr_open: built_in(merged.approvals.pr_open, "approvals.pr_open")?,
            },
            api: Api {
                agent_writes: built_in(merged.api.agent_writes, "api.agent_writes")?,
            },
            seed: merged.seed,
        })
    }
}

/// The lowest layer: the typed defaults, stated where a repository could
/// override them.
fn built_in() -> RepoConfig {
    let ports = Ports::default();
    let simulators = Simulators::default();
    let auto_cleanup = AutoCleanup::default();
    let merged_cleanup = MergedCleanup::default();
    let restore = Restore::default();
    RepoConfig {
        max_agents: Some(config::AgentLimit::Dynamic),
        close_when_done: Some(true),
        chat_template: Some(config::templates::CHAT_DEFAULT.into()),
        commands: named_commands::defaults(),
        herdr: config::repo::Herdr {
            enabled: Some(true),
            tab_name: None,
            new_tab: Some(true),
            workspace_name: Some("{repo}".into()),
            close_when_done: None,
        },
        default_host: Some(config::repo::DefaultHost::default()),
        rex: config::repo::Rex {
            enabled: Some(true),
            new_window: Some(true),
            placement: Some(config::repo::RexPlacement::default()),
            window_name: None,
            session_name: Some("{repo}".into()),
            close_when_done: None,
        },
        happy: config::repo::Happy {
            enabled: Some(false),
        },
        claude: config::repo::Shortcut {
            default_mode: Some(Shortcut::default().default_mode),
        },
        codex: config::repo::Shortcut {
            default_mode: Some(Shortcut::default().default_mode),
        },
        copy_ignored: Some(true),
        agent_dirs: Some(vec!["/tmp".into()]),
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
        cleanup: config::repo::Cleanup {
            grace_minutes: Some(Cleanup::default().grace_minutes),
            auto: config::repo::AutoCleanup {
                enabled: Some(auto_cleanup.enabled),
                idle_minutes: Some(auto_cleanup.idle_minutes),
            },
            pr: config::repo::PrCleanup {
                enabled: Some(PrCleanup::default().enabled),
            },
            merged: config::repo::MergedCleanup {
                enabled: Some(merged_cleanup.enabled),
                idle_minutes: Some(merged_cleanup.idle_minutes),
            },
            restore: config::repo::Restore {
                retention_days: Some(restore.retention_days),
                archive_max_mb: Some(restore.archive_max_mb),
            },
        },
        done: config::repo::Done {
            cleanup: Some(Done::default().cleanup),
        },
        idle_stop: config::repo::IdleStop {
            enabled: Some(IdleStop::default().enabled),
        },
        review: config::repo::Review {
            post: Some(Review::default().post),
        },
        ready: config::repo::Ready {
            undraft: Some(Ready::default().undraft),
        },
        unready: config::repo::Unready {
            draft: Some(Unready::default().draft),
        },
        pr: config::repo::Pr {
            draft: Some(Pr::default().draft),
            required_labels: Some(Vec::new()),
        },
        issue: config::repo::Issue {
            required_labels: Some(Vec::new()),
            other_repositories: Some(Issue::default().other_repositories),
            ..Default::default()
        },
        land: config::repo::Land {
            push: Some(Land::default().push),
            requires_approval: Some(Land::default().requires_approval),
        },
        approvals: config::repo::Approvals {
            issue_open: Some(Approvals::default().issue_open),
            pr_open: Some(Approvals::default().pr_open),
        },
        api: config::repo::Api {
            agent_writes: Some(Api::default().agent_writes),
        },
        ..Default::default()
    }
}

/// One option's merge rule and its provenance lookup.
trait Field {
    /// Move `top`'s value for this option onto `base` where `top` sets it.
    fn layer(&self, top: &mut RepoConfig, base: &mut RepoConfig);
    /// Apply the selected repository value to the global/default value.
    fn layer_repository(&self, top: &mut RepoConfig, base: &mut RepoConfig) {
        self.layer(top, base);
    }
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

/// Repository templates select one local addition to the global/default text.
struct Template(Scalar<String>);

impl Field for Template {
    fn layer(&self, top: &mut RepoConfig, base: &mut RepoConfig) {
        self.0.layer(top, base);
    }

    fn layer_repository(&self, top: &mut RepoConfig, base: &mut RepoConfig) {
        let addition = (self.0.get_mut)(top).take();
        let value = (self.0.get_mut)(base);
        *value = append_template(value.take(), addition);
    }

    fn entries(&self, sources: &[(&RepoConfig, Layer)], entries: &mut Vec<Entry>) -> Result<()> {
        let find = |repository| {
            sources.iter().find_map(|(config, layer)| {
                let is_repository =
                    matches!(layer, Layer::WorktreeFile | Layer::SavedRepositoryConfig);
                if is_repository != repository {
                    return None;
                }
                (self.0.get)(config).as_ref().map(|value| (value, *layer))
            })
        };
        let base = find(false);
        let addition = find(true);
        entries.push(Entry {
            key: self.0.key.into(),
            value: serde_json::to_value(append_template(
                base.map(|(value, _)| value.clone()),
                addition.map(|(value, _)| value.clone()),
            ))?,
            layer: addition
                .or(base)
                .map_or(Layer::BuiltInDefault, |(_, layer)| layer),
        });
        Ok(())
    }
}

fn append_template(base: Option<String>, addition: Option<String>) -> Option<String> {
    match (base, addition) {
        (Some(base), Some(addition)) if !base.is_empty() && !addition.is_empty() => {
            Some(format!("{base}\n\n{addition}"))
        }
        (Some(base), Some(addition)) if base.is_empty() => Some(addition),
        (Some(base), _) => Some(base),
        (None, addition) => addition,
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
        named!("agent_resume", agent_resume),
        Box::new(Template(Scalar {
            key: "worker_template",
            get: |config| &config.worker_template,
            get_mut: |config| &mut config.worker_template,
        })),
        Box::new(Template(Scalar {
            key: "chat_template",
            get: |config| &config.chat_template,
            get_mut: |config| &mut config.chat_template,
        })),
        scalar!(agent_auth.fj),
        scalar!(agent_auth.fj_home),
        scalar!(agent_auth.gh),
        scalar!(agent_auth.glab),
        scalar!(agent_auth.tea),
        scalar!(agent_auth.git_profile),
        scalar!(git_profile),
        scalar!(default_agent),
        scalar!(max_agents),
        scalar!(focus),
        scalar!(close_when_done),
        scalar!(claude.default_mode),
        scalar!(codex.default_mode),
        scalar!(herdr.enabled),
        scalar!(herdr.new_tab),
        scalar!(herdr.tab_name),
        scalar!(herdr.workspace_name),
        scalar!(herdr.close_when_done),
        scalar!(default_host),
        scalar!(rex.enabled),
        scalar!(rex.new_window),
        scalar!(rex.placement),
        scalar!(rex.window_name),
        scalar!(rex.session_name),
        scalar!(rex.close_when_done),
        scalar!(happy.enabled),
        scalar!(copy_ignored),
        scalar!(agent_dirs),
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
        named!("env", env),
        named!("caches", caches),
        named!("demos", demos),
        scalar!(presets),
        named!("secrets", secrets),
        named!("resources", resources),
        named!("resource_pools", resource_pools),
        scalar!(simulators.requires_approval),
        scalar!(simulators.approval_lifetime),
        list!(simulators.preferred),
        scalar!(cleanup.grace_minutes),
        scalar!(cleanup.auto.enabled),
        scalar!(cleanup.auto.idle_minutes),
        scalar!(cleanup.pr.enabled),
        scalar!(cleanup.merged.enabled),
        scalar!(cleanup.merged.idle_minutes),
        scalar!(cleanup.restore.retention_days),
        scalar!(cleanup.restore.archive_max_mb),
        scalar!(done.cleanup),
        scalar!(idle_stop.enabled),
        scalar!(review.post),
        scalar!(ready.undraft),
        scalar!(unready.draft),
        scalar!(pr.draft),
        scalar!(pr.required_labels),
        scalar!(issue.required_labels),
        scalar!(issue.other_repositories),
        scalar!(issue.plan_label),
        scalar!(issue.planned_label),
        scalar!(land.push),
        scalar!(land.requires_approval),
        scalar!(approvals.issue_open),
        scalar!(approvals.pr_open),
        scalar!(api.agent_writes),
        scalar!(seed.cmd),
        list!(seed.paths),
    ];
    fields.extend(rest);
    fields
}

#[cfg(test)]
mod tests;
