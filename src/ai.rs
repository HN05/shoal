//! AI tools: the providers Shoal ships and the user's `[ai.<name>]` additions.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
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
    /// Arguments that give the agent instructions apart from its first
    /// message, with `{instructions}`, or `{instructions_toml}` for a TOML
    /// string. Without them, instructions join the prompt.
    pub instructions: Option<Vec<String>>,
    /// Arguments that resume the session with ID `{session}`.
    pub resume: Option<Vec<String>>,
    /// Arguments that resume the workspace's latest session.
    pub resume_latest: Option<Vec<String>>,
    /// Arguments that give the agent another working directory,
    /// `{directory}`; empty gives none.
    pub add_dir: Option<Vec<String>>,
    /// The AI tool this one is a launch profile of: it inherits that tool's
    /// integration, launcher and arguments, each replaced by its own.
    pub base: Option<String>,
    /// Variables set for the tool's tracked launches, after the workspace's
    /// `[env]`; a value starting with `~/` names a path under the home.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Variables removed from the tool's tracked launches, whether inherited
    /// or set by `[env]`.
    #[serde(default)]
    pub env_remove: Vec<String>,
    /// Whether Shoal retries a turn an API failure ended, for an agent whose
    /// integration reports failed turns.
    pub retry: Option<bool>,
}

/// What an AI tool's tracked launches change in the environment they would
/// otherwise run with: values as written, and the variables they remove.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    set: BTreeMap<String, String>,
    pub remove: BTreeSet<String>,
}

impl Environment {
    /// The values set, with a leading `~/` naming a path under `home`.
    pub fn values(&self, home: &Path) -> BTreeMap<String, OsString> {
        self.set
            .iter()
            .map(|(variable, value)| {
                let value = match value.strip_prefix("~/") {
                    Some(relative) => home.join(relative).into_os_string(),
                    None => value.into(),
                };
                (variable.clone(), value)
            })
            .collect()
    }

    /// `configured`, the variables a launch adds to this process's, with
    /// these changes.
    pub fn layer(&self, configured: &mut BTreeMap<String, OsString>, home: &Path) {
        for variable in &self.remove {
            configured.remove(variable);
        }
        configured.extend(self.values(home));
    }

    /// Make these changes to `command`'s environment.
    pub fn apply(&self, command: &mut std::process::Command, home: &Path) {
        for variable in &self.remove {
            command.env_remove(variable);
        }
        command.envs(self.values(home));
    }
}

/// What a provider's launcher takes after its command, from `[ai.<name>]`,
/// else the built-in provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Arguments {
    pub instructions: Option<Vec<String>>,
    pub resume: Option<Vec<String>>,
    pub resume_latest: Option<Vec<String>>,
    pub add_dir: Option<Vec<String>>,
}

/// A provider available without configuration.
pub struct Provider {
    pub name: &'static str,
    command: &'static [&'static str],
    skill_dir: SkillDir,
    instructions: Option<&'static [&'static str]>,
    resume: Option<&'static [&'static str]>,
    resume_latest: Option<&'static [&'static str]>,
    add_dir: Option<&'static [&'static str]>,
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
        instructions: Some(&["-c", "developer_instructions={instructions_toml}"]),
        resume: Some(&["resume", "{session}"]),
        // Codex's worktrees feature widens `--last` to every linked worktree
        // of the repository; restore this workspace's session.
        resume_latest: Some(&["resume", "--last", "-c", "features.worktrees=false"]),
        add_dir: Some(&["--add-dir", "{directory}"]),
    },
    Provider {
        name: "claude",
        command: &["claude"],
        skill_dir: SkillDir::Claude,
        instructions: Some(&["--append-system-prompt", "{instructions}"]),
        resume: Some(&["--resume", "{session}"]),
        resume_latest: Some(&["--continue"]),
        // `--add-dir` takes every following argument up to the next option,
        // which would include a prompt; the joined form takes one.
        add_dir: Some(&["--add-dir={directory}"]),
    },
    // opencode and pi also read `~/.agents/skills`, so they share Codex's copy.
    Provider {
        name: "opencode",
        command: &["opencode", "--prompt", "{prompt}", "{args}"],
        skill_dir: SkillDir::Home(".agents/skills"),
        instructions: None,
        resume: None,
        resume_latest: None,
        add_dir: None,
    },
    Provider {
        name: "pi",
        command: &["pi"],
        skill_dir: SkillDir::Home(".agents/skills"),
        instructions: None,
        resume: None,
        resume_latest: None,
        add_dir: None,
    },
    Provider {
        name: "grok",
        command: &["grok"],
        skill_dir: SkillDir::Home(".grok/skills"),
        instructions: None,
        resume: None,
        resume_latest: None,
        add_dir: None,
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
/// launcher or a base. A tool with only a skill directory is not one.
pub fn providers(agents: &Agents) -> BTreeSet<String> {
    PROVIDERS
        .iter()
        .map(|provider| provider.name.to_owned())
        .chain(
            agents
                .iter()
                .filter(|(_, agent)| agent.command.is_some() || agent.base.is_some())
                .map(|(name, _)| name.clone()),
        )
        .collect()
}

/// `name` and the tools it is a launch profile of, nearest first.
/// Validation rejects cycles; the bound keeps an unvalidated one finite.
fn lineage<'a>(agents: &'a Agents, name: &'a str) -> impl Iterator<Item = &'a str> {
    std::iter::successors(Some(name), |name| agents.get(*name)?.base.as_deref())
        .take(agents.len() + 1)
}

/// The tool whose integration `name` runs with: the end of its lineage.
pub fn integration<'a>(agents: &'a Agents, name: &'a str) -> &'a str {
    lineage(agents, name).last().unwrap_or(name)
}

/// Whether Shoal retries `name`'s failed turns: the `retry` set nearest in
/// its lineage, off without one.
pub fn retries(agents: &Agents, name: &str) -> bool {
    lineage(agents, name)
        .find_map(|name| agents.get(name)?.retry)
        .unwrap_or(false)
}

/// The tool each launch profile runs with the integration of.
pub fn integrations(agents: &Agents) -> BTreeMap<String, String> {
    agents
        .iter()
        .filter(|(_, agent)| agent.base.is_some())
        .map(|(name, _)| (name.clone(), integration(agents, name).to_owned()))
        .collect()
}

/// Give each launch profile without a launcher of its own in `commands` the
/// nearest one of its lineage there, so it follows whatever layer set it.
/// `commands` may carry each launcher's layer with it.
pub fn inherit_launchers<T: Clone>(agents: &Agents, commands: &mut BTreeMap<String, T>) {
    for name in agents.keys() {
        if commands.contains_key(name) {
            continue;
        }
        let inherited = lineage(agents, name)
            .skip(1)
            .find_map(|base| commands.get(base))
            .cloned();
        if let Some(argv) = inherited {
            commands.insert(name.clone(), argv);
        }
    }
}

/// The environment changes of every tool configured with any. A profile's
/// own entries win over its base's.
pub fn environments(agents: &Agents) -> BTreeMap<String, Environment> {
    agents
        .keys()
        .map(|name| (name.clone(), environment(agents, name)))
        .filter(|(_, environment)| *environment != Environment::default())
        .collect()
}

fn environment(agents: &Agents, name: &str) -> Environment {
    let lineage: Vec<_> = lineage(agents, name)
        .filter_map(|name| agents.get(name))
        .collect();
    let mut environment = Environment::default();
    for agent in lineage.into_iter().rev() {
        for variable in &agent.env_remove {
            environment.set.remove(variable);
            environment.remove.insert(variable.clone());
        }
        for (variable, value) in &agent.env {
            environment.remove.remove(variable);
            environment.set.insert(variable.clone(), value.clone());
        }
    }
    environment
}

/// The launcher arguments of every built-in or configured AI tool.
pub fn all_arguments(agents: &Agents) -> BTreeMap<String, Arguments> {
    PROVIDERS
        .iter()
        .map(|provider| provider.name)
        .chain(agents.keys().map(String::as_str))
        .map(|name| (name.to_owned(), arguments(agents, name)))
        .collect()
}

/// The arguments `name`'s launcher takes: each entry configured nearest in
/// its lineage, else the built-in provider's at its end.
pub fn arguments(agents: &Agents, name: &str) -> Arguments {
    let root = integration(agents, name);
    let builtin = PROVIDERS.iter().find(|provider| provider.name == root);
    let owned = |argv: &[&str]| argv.iter().map(|arg| arg.to_string()).collect();
    let pick = |configured: fn(&Agent) -> Option<&Vec<String>>, builtin: Option<&[&str]>| {
        lineage(agents, name)
            .find_map(|name| agents.get(name).and_then(configured))
            .cloned()
            .or_else(|| builtin.map(owned))
    };
    Arguments {
        instructions: pick(
            |agent| agent.instructions.as_ref(),
            builtin.and_then(|provider| provider.instructions),
        ),
        resume: pick(
            |agent| agent.resume.as_ref(),
            builtin.and_then(|provider| provider.resume),
        ),
        resume_latest: pick(
            |agent| agent.resume_latest.as_ref(),
            builtin.and_then(|provider| provider.resume_latest),
        ),
        add_dir: pick(
            |agent| agent.add_dir.as_ref(),
            builtin.and_then(|provider| provider.add_dir),
        ),
    }
}

impl Arguments {
    /// The arguments that give the agent `instructions`; `None` when it takes
    /// them only in its prompt. Empty instructions need no arguments.
    pub fn instruction_args(&self, instructions: &str) -> Option<Vec<OsString>> {
        let template = self.instructions.as_ref()?;
        if instructions.is_empty() {
            return Some(Vec::new());
        }
        let toml = toml::Value::String(instructions.to_owned()).to_string();
        Some(substitute(
            template,
            &[
                ("{instructions}", instructions),
                ("{instructions_toml}", &toml),
            ],
        ))
    }

    /// The arguments that resume `session`, or the workspace's latest session
    /// when none was reported; `None` when the agent cannot resume.
    pub fn resume_args(&self, session: Option<&str>) -> Option<Vec<OsString>> {
        match session {
            Some(session) => self
                .resume
                .as_ref()
                .map(|template| substitute(template, &[("{session}", session)])),
            None => self
                .resume_latest
                .as_ref()
                .map(|template| substitute(template, &[])),
        }
    }

    /// The arguments that give the agent `directory` as another working
    /// directory; none when it takes no such arguments.
    pub fn add_dir_args(&self, directory: &Path) -> Vec<OsString> {
        let Some(template) = &self.add_dir else {
            return Vec::new();
        };
        // Placeholders render into text, which a path need not be.
        let Some(directory) = directory.to_str() else {
            return Vec::new();
        };
        substitute(template, &[("{directory}", directory)])
    }
}

/// Each argument with every placeholder in `fields` replaced once.
fn substitute(template: &[String], fields: &[(&str, &str)]) -> Vec<OsString> {
    template
        .iter()
        .map(|arg| crate::config::templates::render(arg, fields).into())
        .collect()
}

/// Each provider's user-level skill directory; `None` when it has none.
/// A tool's own or nearest inherited `skill_dir`, else the directory of the
/// built-in provider its lineage ends at, in the tool's environment.
pub fn skill_dirs(agents: &Agents, home: &Path) -> Result<BTreeMap<String, Option<PathBuf>>> {
    let names = PROVIDERS
        .iter()
        .map(|provider| provider.name)
        .chain(agents.keys().map(String::as_str));
    let mut directories = BTreeMap::new();
    for name in names {
        let configured = lineage(agents, name)
            .find_map(|name| agents.get(name)?.skill_dir.as_ref())
            .map(|directory| skill_dir(directory, home))
            .transpose()?;
        let directory = match configured {
            Some(directory) => Some(directory),
            None => PROVIDERS
                .iter()
                .find(|provider| provider.name == integration(agents, name))
                .map(|provider| {
                    provider_skill_dir(provider, &environment(agents, name).values(home), home)
                })
                .transpose()?,
        };
        directories.insert(name.to_owned(), directory);
    }
    Ok(directories)
}

/// `provider`'s skill directory for a launch that adds `configured` to this
/// process's environment.
fn provider_skill_dir(
    provider: &Provider,
    configured: &BTreeMap<String, OsString>,
    home: &Path,
) -> Result<PathBuf> {
    Ok(match provider.skill_dir {
        SkillDir::Claude => crate::env::claude_config_dir_with(configured)?
            .unwrap_or_else(|| home.join(".claude"))
            .join("skills"),
        SkillDir::Home(relative) => home.join(relative),
    })
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
        if let Some(base) = &agent.base {
            validate_launcher_name(name, commands).with_context(|| format!("ai.{name}"))?;
            validate_base(agents, name, base).with_context(|| format!("ai.{name}.base"))?;
        }
        validate_environment(agents, name, agent).with_context(|| format!("ai.{name}"))?;
        ensure!(
            agent.retry.is_none() || providers(agents).contains(name),
            "ai.{name}.retry needs a command or base, or a built-in AI tool"
        );
        for (key, argv, placeholders) in [
            (
                "instructions",
                &agent.instructions,
                &["{instructions}", "{instructions_toml}"][..],
            ),
            ("resume", &agent.resume, &["{session}"][..]),
            ("resume_latest", &agent.resume_latest, &[][..]),
        ] {
            if let Some(argv) = argv {
                validate_arguments(argv, placeholders)
                    .with_context(|| format!("ai.{name}.{key}"))?;
            }
        }
        // An empty `add_dir` turns the built-in one off.
        if let Some(argv) = agent.add_dir.as_ref().filter(|argv| !argv.is_empty()) {
            validate_arguments(argv, &["{directory}"])
                .with_context(|| format!("ai.{name}.add_dir"))?;
        }
    }
    Ok(())
}

fn validate_command(name: &str, argv: &[String], commands: &Commands) -> Result<()> {
    validate_launcher_name(name, commands)?;
    named_commands::validate(&Commands::from([(name.to_owned(), argv.to_vec())]))
}

/// A tool that runs as `shoal <name>` needs a name that reaches it.
fn validate_launcher_name(name: &str, commands: &Commands) -> Result<()> {
    ensure!(
        name.parse::<crate::agent::Agent>().is_ok(),
        "{name:?} cannot name an agent; use lowercase letters, digits, '-' or '_'"
    );
    let built_in_provider = PROVIDERS.iter().any(|provider| provider.name == name);
    ensure!(
        built_in_provider || !built_in_commands().iter().any(|command| command == name),
        "{name:?} is a built-in Shoal command; choose another name"
    );
    ensure!(
        !commands.contains_key(name),
        "{name:?} is also defined in [commands]; keep one launcher"
    );
    Ok(())
}

/// A base is another AI tool that runs as an agent, and following bases
/// from `name` ends at one without.
fn validate_base(agents: &Agents, name: &str, base: &str) -> Result<()> {
    ensure!(
        providers(agents).contains(base),
        "{base:?} is not a built-in AI tool or one configured with a command or base"
    );
    let mut seen = BTreeSet::from([name]);
    let mut next = Some(base);
    while let Some(tool) = next {
        ensure!(
            seen.insert(tool),
            "bases of {name:?} form a cycle through {tool:?}"
        );
        next = agents.get(tool).and_then(|agent| agent.base.as_deref());
    }
    Ok(())
}

/// Variables follow `[env]`'s naming rules. Shoal reads where an agent keeps
/// its config from the variables it sets, so those can be set, not removed.
fn validate_environment(agents: &Agents, name: &str, agent: &Agent) -> Result<()> {
    if agent.env.is_empty() && agent.env_remove.is_empty() {
        return Ok(());
    }
    ensure!(
        providers(agents).contains(name),
        "env and env_remove need a command or base, or a built-in AI tool"
    );
    for (variable, value) in &agent.env {
        crate::env::validate_configured_name("env", variable)?;
        ensure!(
            !value.contains('\0'),
            "env value for {variable} contains NUL"
        );
    }
    for variable in &agent.env_remove {
        crate::env::validate_configured_name("env_remove", variable)?;
        ensure!(
            !crate::env::AGENT_CONFIG_VARIABLES.contains(&variable.as_str()),
            "env_remove cannot remove {variable}; set it in env to the directory instead"
        );
    }
    for variable in crate::env::AGENT_CONFIG_VARIABLES {
        if let Some(directory) = agent.env.get(variable) {
            ensure!(
                directory.starts_with('/') || directory.starts_with("~/"),
                "env {variable} must be an absolute path or start with ~/"
            );
        }
    }
    Ok(())
}

/// The names of Shoal's own subcommands. Building the CLI to read them
/// takes more stack in a debug build than a worker thread has, so it is
/// built once, on a thread of its own.
fn built_in_commands() -> &'static [String] {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let names = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(|| {
                crate::cli::Cli::command()
                    .get_subcommands()
                    .map(|command| command.get_name().to_owned())
                    .collect()
            })
            .expect("spawn a thread to build the CLI");
        names.join().expect("building the CLI does not panic")
    })
}

/// Arguments must name one of `placeholders` when there are any.
fn validate_arguments(argv: &[String], placeholders: &[&str]) -> Result<()> {
    ensure!(
        placeholders.is_empty()
            || argv
                .iter()
                .any(|arg| placeholders.iter().any(|field| arg.contains(field))),
        "use {}",
        placeholders.join(" or ")
    );
    ensure!(
        !argv.iter().any(|arg| arg.contains('\0')),
        "arguments must not contain NUL"
    );
    Ok(())
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
mod tests;
