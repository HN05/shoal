//! `[modes.<name>]` and `[rules.<name>]`: what an agent is told. A mode
//! names the rules its system text carries, the instructions its first
//! message ends with, and the kind of space `shoal start` creates for it,
//! with or without a repository.
//! Rules are shared by name; instructions belong to one mode. Each may be
//! written inline or as `rules/<name>.md` and `modes/<name>.md` beside a
//! config file. The `setup` mode is built in apart from them: no config
//! defines or changes it.
use std::{
    borrow::Cow,
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    config::{
        options::Options,
        permissions::{PermissionsConfig, SpaceMode},
        repo::RepoConfig,
    },
    model::SpaceKind,
};

mod name;
pub use name::ModeName;

/// The mode `shoal start` uses without `--mode`.
pub const DEFAULT: &str = "worker";
/// The mode a chat starts in without `--mode`.
pub const CHAT: &str = "chat";
/// The mode whose `--issue` without a value offers the issues that need a
/// plan.
pub const PLAN: &str = "plan";
/// The rule a local review's style comes from, after its fixed instructions.
pub const REVIEW_RULE: &str = "review";
/// The mode of a chat without a repository that manages every space.
pub const ORCHESTRATE: &str = "orchestrate";
/// The mode in which an agent writes Shoal's config, which `shoal repo
/// setup` and `shoal config setup` start. It takes the rules of its
/// [`setup_base`] with instructions of its own.
pub const SETUP: &str = "setup";
/// What a setup agent is told after its subject.
const SETUP_INSTRUCTIONS: &str = include_str!("modes/setup.md");
/// The practices of Shoal's built-in modes, which a user's own modes may
/// list, replace or leave out.
const WORK_RULE: &str = "work";

const MODES_DIR: &str = "modes";
const RULES_DIR: &str = "rules";

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Mode)]
pub struct ModeConfig {
    /// The kind of space `shoal start` creates in this mode.
    #[option(default = SpaceKind::Workspace)]
    pub kind: Option<SpaceKind>,
    /// Whether the space belongs to a repository. Only a chat may have
    /// none: it runs in `root_dir`, and its scope reaches every space.
    #[option(default = true)]
    pub repository: Option<bool>,
    /// The rules the agent's system text carries after the facts of its
    /// space and the space rules, in order.
    #[option(list)]
    pub rules: Vec<String>,
    /// How to do the mode's job, sent once after the subject in the first
    /// message.
    #[option(template, default = String::new())]
    pub instructions: Option<String>,
    /// What the mode's agents may do, over `[permissions]`.
    #[option(section)]
    pub permissions: PermissionsConfig,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Rule)]
pub struct RuleConfig {
    /// What the agent's system text says for the rule.
    #[option(template, default = String::new())]
    pub text: Option<String>,
}

/// The modes Shoal ships, each with the `work` rule before its own.
pub fn built_in_modes() -> BTreeMap<String, ModeConfig> {
    let mode = |kind, rule: &str, instructions: &str| ModeConfig {
        kind: Some(kind),
        repository: None,
        rules: vec![WORK_RULE.to_owned(), rule.to_owned()],
        instructions: Some(instructions.to_owned()),
        permissions: PermissionsConfig::default(),
    };
    BTreeMap::from([
        (
            DEFAULT.to_owned(),
            mode(
                SpaceKind::Workspace,
                "worker",
                include_str!("modes/worker.md"),
            ),
        ),
        (CHAT.to_owned(), mode(SpaceKind::Chat, "chat", "")),
        (
            PLAN.to_owned(),
            mode(SpaceKind::Chat, "chat", include_str!("modes/plan.md")),
        ),
        (
            ORCHESTRATE.to_owned(),
            ModeConfig {
                repository: Some(false),
                rules: vec![
                    WORK_RULE.to_owned(),
                    "chat".to_owned(),
                    ORCHESTRATE.to_owned(),
                ],
                ..mode(SpaceKind::Chat, "chat", "")
            },
        ),
    ])
}

/// The rules Shoal ships; the `worker` and `chat` rules have no text of
/// their own.
pub fn built_in_rules() -> BTreeMap<String, RuleConfig> {
    let rule = |text: &str| RuleConfig {
        text: Some(text.to_owned()),
    };
    BTreeMap::from([
        (WORK_RULE.to_owned(), rule(include_str!("rules/work.md"))),
        ("worker".to_owned(), rule("")),
        ("chat".to_owned(), rule("")),
        (
            REVIEW_RULE.to_owned(),
            rule(crate::config::templates::review_base()),
        ),
        (
            ORCHESTRATE.to_owned(),
            rule(include_str!("rules/orchestrate.md")),
        ),
    ])
}

/// The mode and rule names one layer writes, which name their files, and
/// the kind of the default mode, which always starts in a workspace.
pub fn validate(config: &RepoConfig) -> Result<()> {
    for name in config.modes.keys() {
        crate::validate::name("mode", name)?;
        ensure_configurable(name)?;
    }
    let default_kind = config.modes.get(DEFAULT).and_then(|mode| mode.kind);
    ensure!(
        default_kind.is_none_or(|kind| kind == SpaceKind::Workspace),
        "modes.{DEFAULT}.kind must be workspace: shoal start without --mode starts a \
         workspace; define another mode for chats"
    );
    for name in config.rules.keys() {
        crate::validate::name("rule", name)?;
    }
    Ok(())
}

/// Refuse config for mode `name` when Shoal alone defines it.
fn ensure_configurable(name: &str) -> Result<()> {
    ensure!(
        name != SETUP,
        "mode {SETUP} is built in, and config cannot change it; give your own mode \
         another name to keep it"
    );
    Ok(())
}

/// Refuse a start in mode `name` that only its own commands make.
pub fn ensure_startable(name: &str) -> Result<()> {
    ensure!(
        name != SETUP,
        "mode {SETUP} starts only with shoal repo setup or shoal config setup"
    );
    Ok(())
}

/// The mode `name` of `modes`, refused when no config defines it.
pub fn get<'a>(modes: &'a BTreeMap<String, Mode>, name: &str) -> Result<&'a Mode> {
    modes
        .get(name)
        .with_context(|| format!("no mode {name}; [modes.{name}] in config defines one"))
}

/// The mode `name` of a space of `kind`, with a repository or without one
/// as `repository` says: a setup space's [`setup_base`] with the setup
/// instructions, else the mode `modes` defines.
pub fn resolve<'a>(
    modes: &'a BTreeMap<String, Mode>,
    name: &str,
    kind: SpaceKind,
    repository: bool,
) -> Result<Cow<'a, Mode>> {
    if name != SETUP {
        return get(modes, name).map(Cow::Borrowed);
    }
    let base = get(modes, setup_base(kind, repository)?)?;
    Ok(Cow::Owned(Mode {
        instructions: SETUP_INSTRUCTIONS.to_owned(),
        ..base.clone()
    }))
}

/// The mode whose kind, reach, rules and permissions a setup space of
/// `kind` takes: the worker's for a repository's workspace, the
/// orchestrator's for a chat without a repository, which sets up the
/// global config.
pub fn setup_base(kind: SpaceKind, repository: bool) -> Result<&'static str> {
    match (kind, repository) {
        (SpaceKind::Workspace, _) => Ok(DEFAULT),
        (SpaceKind::Chat, false) => Ok(ORCHESTRATE),
        (SpaceKind::Chat, true) => {
            bail!("mode {SETUP} runs in a workspace or in a chat without a repository")
        }
    }
}

/// The permissions mode `mode` sets in config, none for a mode no TOML layer
/// defines, and those Shoal's mode of that name has by default, which
/// `[permissions]` overrides; a setup mode takes both from its
/// [`setup_base`].
pub fn permission_layers(
    modes: &BTreeMap<String, Mode>,
    mode: SpaceMode,
) -> Result<(PermissionsConfig, PermissionsConfig)> {
    let name = match mode.name {
        SETUP => setup_base(mode.kind, mode.repository)?,
        name => name,
    };
    // A mode only a Markdown file names, which the daemon's settings leave
    // out, configures no permissions.
    let configured = modes.get(name).map(|mode| mode.permissions);
    Ok((configured.unwrap_or_default(), built_in_permissions(name)))
}

/// The permissions Shoal's mode `name` has where config sets none: a
/// worker's are the keys' defaults, and the modes that coordinate decide
/// their children's merges and issues elsewhere; a mode Shoal does not ship
/// has none.
fn built_in_permissions(name: &str) -> PermissionsConfig {
    match name {
        CHAT | PLAN | ORCHESTRATE => PermissionsConfig::COORDINATING,
        _ => PermissionsConfig::default(),
    }
}

/// Refuse a mode that names a rule no layer defines, or a workspace without
/// a repository, once they combine.
pub fn check(modes: &BTreeMap<String, Mode>, rules: &BTreeMap<String, Rule>) -> Result<()> {
    for (name, mode) in modes {
        ensure!(
            mode.repository || mode.kind == SpaceKind::Chat,
            "modes.{name}.repository = false needs kind = \"chat\": a workspace belongs to a \
             repository"
        );
        if let Some(rule) = mode.rules.iter().find(|rule| !rules.contains_key(*rule)) {
            bail!("modes.{name} names rule {rule}, which no config defines");
        }
    }
    Ok(())
}

/// The Markdown file each of one layer's texts came from, by option key.
pub type Files = BTreeMap<String, PathBuf>;

/// Fill one layer's omitted inline texts from the `modes/<name>.md` and
/// `rules/<name>.md` files in `directories`, where each file may be in only
/// one of them. A file names its mode or rule, which it adds when the layer
/// has none of that name. A file an inline value overrides is never read.
pub fn fill(config: &mut RepoConfig, directories: &[PathBuf]) -> Result<Files> {
    let mut files = Files::new();
    for (name, path) in markdown_files(directories, MODES_DIR)? {
        ensure_configurable(&name).with_context(|| path.display().to_string())?;
        let mode = config.modes.entry(name.clone()).or_default();
        if mode.instructions.is_none() {
            mode.instructions = Some(read(&path)?);
            files.insert(format!("{MODES_DIR}.{name}.instructions"), path);
        }
    }
    for (name, path) in markdown_files(directories, RULES_DIR)? {
        let rule = config.rules.entry(name.clone()).or_default();
        if rule.text.is_none() {
            rule.text = Some(read(&path)?);
            files.insert(format!("{RULES_DIR}.{name}.text"), path);
        }
    }
    Ok(files)
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

/// Each `<name>.md` in the `subdirectory` of `directories`, by name; a name
/// in more than one of them is refused.
fn markdown_files(
    directories: &[PathBuf],
    subdirectory: &str,
) -> Result<BTreeMap<String, PathBuf>> {
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
    for directory in directories
        .iter()
        .map(|directory| directory.join(subdirectory))
    {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", directory.display()));
            }
        };
        for entry in entries {
            let path = entry
                .with_context(|| format!("read {}", directory.display()))?
                .path();
            if path.extension().is_none_or(|extension| extension != "md") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            crate::validate::name(subdirectory.trim_end_matches('s'), name)
                .with_context(|| path.display().to_string())?;
            if let Some(first) = found.get(name) {
                bail!(
                    "both {} and {} exist; keep only one {subdirectory}/{name}.md",
                    first.display(),
                    path.display()
                );
            }
            found.insert(name.to_owned(), path);
        }
    }
    Ok(found)
}
