//! AI tools: the providers Shoal ships and the user's `[ai.<name>]` additions.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use clap::CommandFactory;
use serde::Deserialize;

use crate::config::named_commands::{self, Commands};

pub type Agents = BTreeMap<String, Agent>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    /// Parent directory in which Shoal installs `<skill>/SKILL.md`.
    pub skill_dir: Option<PathBuf>,
    /// Launcher at the global layer; a provider with one runs as `shoal <name>`.
    pub command: Option<Vec<String>>,
}

/// A provider available without configuration.
pub struct Provider {
    pub name: &'static str,
    command: &'static [&'static str],
    skill_dir: SkillDir,
}

enum SkillDir {
    /// Claude's config directory, which `CLAUDE_CONFIG_DIR` moves.
    Claude,
    Home(&'static str),
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        name: "codex",
        command: &[
            "codex",
            "{args}",
            "--sandbox",
            "danger-full-access",
            "--ask-for-approval=never",
        ],
        skill_dir: SkillDir::Home(".agents/skills"),
    },
    Provider {
        name: "claude",
        command: &["claude"],
        skill_dir: SkillDir::Claude,
    },
    // opencode and pi also read `~/.agents/skills`, so they share Codex's copy.
    Provider {
        name: "opencode",
        command: &["opencode", "--prompt", "{prompt}", "{args}"],
        skill_dir: SkillDir::Home(".agents/skills"),
    },
    Provider {
        name: "pi",
        command: &["pi"],
        skill_dir: SkillDir::Home(".agents/skills"),
    },
    Provider {
        name: "grok",
        command: &["grok"],
        skill_dir: SkillDir::Home(".grok/skills"),
    },
];

/// Built-in launchers, the lowest layer of `[commands]`.
pub fn default_commands() -> Commands {
    PROVIDERS
        .iter()
        .map(|provider| {
            let argv = provider.command.iter().map(|arg| arg.to_string()).collect();
            (provider.name.to_owned(), argv)
        })
        .collect()
}

/// Launchers configured under `[ai]`, which join the global `[commands]`.
pub fn commands(agents: &Agents) -> impl Iterator<Item = (String, Vec<String>)> + '_ {
    agents
        .iter()
        .filter_map(|(name, agent)| Some((name.clone(), agent.command.clone()?)))
}

/// Every AI tool that runs as an agent: built-in, or configured with a
/// launcher. A tool with only a skill directory is not one.
pub fn providers(agents: &Agents) -> BTreeSet<String> {
    PROVIDERS
        .iter()
        .map(|provider| provider.name.to_owned())
        .chain(commands(agents).map(|(name, _)| name))
        .collect()
}

/// Each provider's user-level skill directory; `None` when it has none.
pub fn skill_dirs(agents: &Agents, home: &Path) -> Result<BTreeMap<String, Option<PathBuf>>> {
    let mut directories = BTreeMap::new();
    for provider in PROVIDERS {
        let directory = match provider.skill_dir {
            SkillDir::Claude => crate::env::claude_config_dir()?
                .unwrap_or_else(|| home.join(".claude"))
                .join("skills"),
            SkillDir::Home(relative) => home.join(relative),
        };
        directories.insert(provider.name.to_owned(), Some(directory));
    }
    for (name, agent) in agents {
        let configured = agent
            .skill_dir
            .as_ref()
            .map(|directory| skill_dir(directory, home))
            .transpose()?;
        let entry = directories.entry(name.clone()).or_default();
        if configured.is_some() {
            *entry = configured;
        }
    }
    Ok(directories)
}

/// `commands` is the same file's `[commands]`, which must not define a
/// launcher `[ai]` also defines.
pub fn validate(agents: &Agents, commands: &Commands, home: &Path) -> Result<()> {
    for (name, agent) in agents {
        crate::validate::name("AI tool", name)?;
        ensure!(
            name != "all",
            "AI tool name 'all' is reserved for skill installation"
        );
        if let Some(directory) = &agent.skill_dir {
            skill_dir(directory, home).with_context(|| format!("ai.{name}.skill_dir"))?;
        }
        if let Some(argv) = &agent.command {
            validate_command(name, argv, commands).with_context(|| format!("ai.{name}.command"))?;
        }
    }
    Ok(())
}

fn validate_command(name: &str, argv: &[String], commands: &Commands) -> Result<()> {
    ensure!(
        name.parse::<crate::agent::Agent>().is_ok(),
        "{name:?} cannot name an agent; use lowercase letters, digits, '-' or '_' without a happy- prefix"
    );
    let built_in_provider = PROVIDERS.iter().any(|provider| provider.name == name);
    ensure!(
        built_in_provider
            || !crate::cli::Cli::command()
                .get_subcommands()
                .any(|command| command.get_name() == name),
        "{name:?} is a built-in Shoal command; choose another name"
    );
    ensure!(
        !commands.contains_key(name),
        "{name:?} is also defined in [commands]; keep one launcher"
    );
    named_commands::validate(&Commands::from([(name.to_owned(), argv.to_vec())]))
}

fn skill_dir(directory: &Path, home: &Path) -> Result<PathBuf> {
    let path = crate::fsutil::expand_home(directory, home);
    ensure!(
        path.is_absolute(),
        "skill_dir must be an absolute path or start with ~/"
    );
    ensure!(
        !path.as_os_str().as_encoded_bytes().contains(&0),
        "skill_dir must not contain NUL"
    );
    Ok(path)
}

/// Skill delivery reads machine config without constructing daemon paths.
pub fn load(home: &Path) -> Result<Agents> {
    let path = crate::config::Config::path_for_home(home);
    let Some(text) =
        crate::fsutil::read_optional(&path).with_context(|| format!("read {}", path.display()))?
    else {
        return Ok(Agents::new());
    };
    let config: crate::config::Config =
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    validate(&config.ai, &config.commands, home)?;
    Ok(config.ai)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Agents> {
        let config: crate::config::Config = toml::from_str(text)?;
        validate(&config.ai, &config.commands, Path::new("/home"))?;
        Ok(config.ai)
    }

    #[test]
    fn configured_providers_add_launchers_and_skill_directories() {
        let agents = parse(
            "[ai.droid]\ncommand = ['droid', '{args}']\n\
             [ai.claude]\nskill_dir = '~/claude-skills'\n\
             [ai.notes]\nskill_dir = '/skills'\n",
        )
        .unwrap();
        assert_eq!(
            commands(&agents).collect::<Vec<_>>(),
            [(
                "droid".to_owned(),
                vec!["droid".to_owned(), "{args}".into()]
            )]
        );
        assert!(providers(&agents).contains("droid"));
        // Skill-only tools are not agents, so `shoal notes` stays a command error.
        assert!(!providers(&agents).contains("notes"));
        let directories = skill_dirs(&agents, Path::new("/home")).unwrap();
        assert_eq!(directories["claude"], Some("/home/claude-skills".into()));
        assert_eq!(directories["codex"], Some("/home/.agents/skills".into()));
        assert_eq!(directories["opencode"], directories["codex"]);
        assert_eq!(directories["grok"], Some("/home/.grok/skills".into()));
        assert_eq!(directories["notes"], Some("/skills".into()));
        assert_eq!(directories["droid"], None);
    }

    #[test]
    fn provider_launchers_need_reachable_unique_names() {
        for (config, message) in [
            ("[ai.ls]\ncommand = ['ls']\n", "built-in Shoal command"),
            ("[ai.happy-x]\ncommand = ['x']\n", "cannot name an agent"),
            ("[ai.Droid]\ncommand = ['droid']\n", "cannot name an agent"),
            ("[ai.droid]\ncommand = []\n", "nonempty executable"),
            (
                "[ai.droid]\ncommand = ['droid']\n[commands]\ndroid = ['droid']\n",
                "keep one launcher",
            ),
        ] {
            let error = format!("{:#}", parse(config).unwrap_err());
            assert!(error.contains(message), "{config}: {error}");
        }
        // Built-in providers may replace their launcher here as in [commands].
        parse("[ai.codex]\ncommand = ['codex']\n").unwrap();
    }
}
