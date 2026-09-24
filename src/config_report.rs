//! Effective repository configuration with the winning layer for every value.
use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::{
    cli::CodexMode,
    client::request,
    config::{self, Config},
    named_commands,
    paths::Paths,
    protocol::{ConfigTarget, Method},
    repo_config::{self, ConfigLayer as Layer, ConfigLayers, RepoConfig},
};

#[derive(Debug, Serialize)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    pub layer: Layer,
}

pub async fn load(paths: &Paths, target: ConfigTarget) -> Result<Vec<Entry>> {
    let layers = request::<Box<ConfigLayers>>(paths, Method::LayeredConfig { target }).await?;
    let (global_config, global) = Config::load_with_repository_layer(paths)?;
    // Retain the same cross-layer validation used by commands that consume the
    // configuration, especially a port range split across two layers.
    global_config.effective(&layers.clone().resolve())?;
    entries(defaults(), global, *layers)
}

fn entries(defaults: RepoConfig, global: RepoConfig, layers: ConfigLayers) -> Result<Vec<Entry>> {
    let ConfigLayers {
        worktree_file,
        saved_repository_config,
    } = layers;
    let sources = [
        (&saved_repository_config, Layer::SavedRepositoryConfig),
        (&worktree_file, Layer::WorktreeFile),
        (&global, Layer::GlobalConfig),
        (&defaults, Layer::BuiltInDefault),
    ];
    let mut result = Vec::new();

    named(&mut result, "commands", &sources, |config| &config.commands)?;
    scalar(&mut result, "issue_template", &sources, |config| {
        config.issue_template.as_ref()
    })?;
    scalar(&mut result, "agent_template", &sources, |config| {
        config.agent_template.as_ref()
    })?;
    scalar(&mut result, "agent_auth.fj", &sources, |config| {
        config.agent_auth.fj.as_ref()
    })?;
    scalar(&mut result, "agent_auth.gh", &sources, |config| {
        config.agent_auth.gh.as_ref()
    })?;
    scalar(&mut result, "git_profile", &sources, |config| {
        config.git_profile.as_ref()
    })?;
    scalar(&mut result, "default_agent", &sources, |config| {
        config.default_agent.as_ref()
    })?;
    scalar(&mut result, "codex.default_mode", &sources, |config| {
        config.codex.default_mode.as_ref()
    })?;
    scalar(&mut result, "pre_setup_cmd", &sources, |config| {
        config.pre_setup_cmd.as_ref()
    })?;
    scalar(&mut result, "post_remove_cmd", &sources, |config| {
        config.post_remove_cmd.as_ref()
    })?;
    scalar(
        &mut result,
        "post_resource_acquire_cmd",
        &sources,
        |config| config.post_resource_acquire_cmd.as_ref(),
    )?;
    scalar(
        &mut result,
        "pre_resource_release_cmd",
        &sources,
        |config| config.pre_resource_release_cmd.as_ref(),
    )?;
    scalar(&mut result, "setup_cmd", &sources, |config| {
        config.setup_cmd.as_ref()
    })?;
    scalar(&mut result, "post_setup_cmd", &sources, |config| {
        config.post_setup_cmd.as_ref()
    })?;
    scalar(&mut result, "pre_remove_cmd", &sources, |config| {
        config.pre_remove_cmd.as_ref()
    })?;
    scalar(&mut result, "ports.on_conflict", &sources, |config| {
        config.ports.on_conflict.as_ref()
    })?;
    scalar(&mut result, "ports.start", &sources, |config| {
        config.ports.start.as_ref()
    })?;
    scalar(&mut result, "ports.end", &sources, |config| {
        config.ports.end.as_ref()
    })?;
    named(&mut result, "ports", &sources, |config| {
        &config.ports.definitions
    })?;
    named(&mut result, "resources", &sources, |config| {
        &config.resources
    })?;
    named(&mut result, "resource_pools", &sources, |config| {
        &config.resource_pools
    })?;
    scalar(
        &mut result,
        "simulators.requires_approval",
        &sources,
        |config| config.simulators.requires_approval.as_ref(),
    )?;
    scalar(
        &mut result,
        "simulators.approval_lifetime",
        &sources,
        |config| config.simulators.approval_lifetime.as_ref(),
    )?;
    let preferred = sources
        .iter()
        .find(|(config, _)| !config.simulators.preferred.is_empty())
        .copied()
        .unwrap_or((&defaults, Layer::BuiltInDefault));
    result.push(Entry {
        key: "simulators.preferred".into(),
        value: serde_json::to_value(&preferred.0.simulators.preferred)?,
        layer: preferred.1,
    });
    scalar(&mut result, "auto_cleanup.enabled", &sources, |config| {
        config.auto_cleanup.enabled.as_ref()
    })?;
    scalar(
        &mut result,
        "auto_cleanup.idle_minutes",
        &sources,
        |config| config.auto_cleanup.idle_minutes.as_ref(),
    )?;
    scalar(&mut result, "pr_cleanup.enabled", &sources, |config| {
        config.pr_cleanup.enabled.as_ref()
    })?;
    Ok(result)
}

fn scalar<T: Serialize + ?Sized>(
    entries: &mut Vec<Entry>,
    key: &str,
    sources: &[(&RepoConfig, Layer)],
    value: impl Fn(&RepoConfig) -> Option<&T>,
) -> Result<()> {
    let resolved = sources
        .iter()
        .find_map(|(config, layer)| value(config).map(|value| (value, *layer)));
    let (value, layer) = match resolved {
        Some((value, layer)) => (serde_json::to_value(value)?, layer),
        None => (Value::Null, Layer::BuiltInDefault),
    };
    entries.push(Entry {
        key: key.into(),
        value,
        layer,
    });
    Ok(())
}

fn named<T: Serialize>(
    entries: &mut Vec<Entry>,
    prefix: &str,
    sources: &[(&RepoConfig, Layer)],
    values: impl Fn(&RepoConfig) -> &BTreeMap<String, T>,
) -> Result<()> {
    let mut resolved = BTreeMap::new();
    for (config, layer) in sources.iter().rev() {
        for (name, value) in values(config) {
            resolved.insert(name, (serde_json::to_value(value)?, *layer));
        }
    }
    entries.extend(resolved.into_iter().map(|(name, (value, layer))| Entry {
        key: format!("{prefix}.{name}"),
        value,
        layer,
    }));
    Ok(())
}

fn defaults() -> RepoConfig {
    RepoConfig {
        commands: named_commands::defaults(),
        codex: repo_config::Codex {
            default_mode: Some(CodexMode::default()),
        },
        ports: repo_config::PortDefaults {
            on_conflict: Some(repo_config::ConflictPolicy::default()),
            start: Some(config::Ports::default().start),
            end: Some(config::Ports::default().end),
            ..Default::default()
        },
        simulators: repo_config::SimulatorPreferences {
            requires_approval: Some(false),
            approval_lifetime: Some(crate::access::Lifetime::Lease),
            ..Default::default()
        },
        auto_cleanup: repo_config::AutoCleanup {
            enabled: Some(config::AutoCleanup::default().enabled),
            idle_minutes: Some(config::AutoCleanup::default().idle_minutes),
        },
        pr_cleanup: repo_config::PrCleanup {
            enabled: Some(config::PrCleanup::default().enabled),
        },
        ..Default::default()
    }
}
