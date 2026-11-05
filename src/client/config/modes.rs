//! `[modes.<name>]` and `[rules.<name>]`: what an agent is told. A mode
//! names the rules its system text carries, the instructions its first
//! message ends with, and the kind of space `shoal start` creates for it.
//! Rules are shared by name; instructions belong to one mode. Each may be
//! written inline or as `rules/<name>.md` and `modes/<name>.md` beside a
//! config file.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    config::{options::Options, repo::RepoConfig},
    model::SpaceKind,
};

/// The mode `shoal start` uses without `--mode`.
pub const DEFAULT: &str = "worker";
/// The rule whose text starts with Shoal's description of a chat's place.
pub const CHAT_RULE: &str = "chat";
/// The rule a local review's style comes from, after its fixed instructions.
pub const REVIEW_RULE: &str = "review";

const MODES_DIR: &str = "modes";
const RULES_DIR: &str = "rules";

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Mode)]
pub struct ModeConfig {
    /// The kind of space `shoal start` creates in this mode.
    #[option(default = SpaceKind::Workspace)]
    pub kind: Option<SpaceKind>,
    /// The rules the agent's system text carries after Shoal's guidance, in
    /// order.
    #[option(list)]
    pub rules: Vec<String>,
    /// How to do the mode's job, sent once after the subject in the first
    /// message.
    #[option(template, default = String::new())]
    pub instructions: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Rule)]
pub struct RuleConfig {
    /// What the agent's system text says for the rule.
    #[option(template, default = String::new())]
    pub text: Option<String>,
}

/// The modes Shoal ships.
pub fn built_in_modes() -> BTreeMap<String, ModeConfig> {
    let mode = |kind, rule: &str, instructions: &str| ModeConfig {
        kind: Some(kind),
        rules: vec![rule.to_owned()],
        instructions: Some(instructions.to_owned()),
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
        ("chat".to_owned(), mode(SpaceKind::Chat, CHAT_RULE, "")),
        (
            "plan".to_owned(),
            mode(SpaceKind::Chat, CHAT_RULE, include_str!("modes/plan.md")),
        ),
    ])
}

/// The rules Shoal ships; only the review style has text of its own.
pub fn built_in_rules() -> BTreeMap<String, RuleConfig> {
    let rule = |text: &str| RuleConfig {
        text: Some(text.to_owned()),
    };
    BTreeMap::from([
        ("worker".to_owned(), rule("")),
        (CHAT_RULE.to_owned(), rule("")),
        (
            REVIEW_RULE.to_owned(),
            rule(crate::config::templates::review_base()),
        ),
    ])
}

/// The mode and rule names one layer writes, which name their files, and
/// the kind of the default mode, which always starts in a workspace.
pub fn validate(config: &RepoConfig) -> Result<()> {
    for name in config.modes.keys() {
        crate::validate::name("mode", name)?;
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

/// The mode `name` of `modes`, refused when no config defines it.
pub fn get<'a>(modes: &'a BTreeMap<String, Mode>, name: &str) -> Result<&'a Mode> {
    modes
        .get(name)
        .with_context(|| format!("no mode {name}; [modes.{name}] in config defines one"))
}

/// Refuse a mode that names a rule no layer defines, once they combine.
pub fn check(modes: &BTreeMap<String, Mode>, rules: &BTreeMap<String, Rule>) -> Result<()> {
    for (name, mode) in modes {
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
