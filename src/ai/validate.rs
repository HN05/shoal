//! Validation of the `[machine.ai.<name>]` entries a config file sets.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use clap::CommandFactory;

use super::{Agent, Agents, PROVIDERS, resolve::runs_as_agent};
use crate::config::named_commands::{self, Commands};

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
            skill_dir(directory, home).with_context(|| format!("machine.ai.{name}.skill_dir"))?;
        }
        if let Some(argv) = &agent.command {
            validate_command(name, argv, commands)
                .with_context(|| format!("machine.ai.{name}.command"))?;
        }
        if let Some(base) = &agent.base {
            validate_launcher_name(name, commands).with_context(|| format!("machine.ai.{name}"))?;
            validate_base(agents, name, base).with_context(|| format!("machine.ai.{name}.base"))?;
        }
        validate_environment(agents, name, agent).with_context(|| format!("machine.ai.{name}"))?;
        ensure!(
            agent.retry.is_none() || runs_as_agent(agents, name),
            "machine.ai.{name}.retry needs a command or base, or a built-in AI tool"
        );
        ensure!(
            agent.prompt_cache_minutes.is_none() || runs_as_agent(agents, name),
            "machine.ai.{name}.prompt_cache_minutes needs a command or base, or a built-in AI tool"
        );
        ensure!(
            agent.prompt_cache_minutes != Some(0),
            "machine.ai.{name}.prompt_cache_minutes must be at least 1"
        );
        for (key, argv, placeholders) in [
            (
                "instructions",
                &agent.instructions,
                &["{instructions}", "{instructions_toml}"][..],
            ),
            ("resume", &agent.resume, &["{session}"][..]),
            ("resume_latest", &agent.resume_latest, &[][..]),
            ("fork", &agent.fork, &["{session}"][..]),
        ] {
            if let Some(argv) = argv {
                validate_arguments(argv, placeholders)
                    .with_context(|| format!("machine.ai.{name}.{key}"))?;
            }
        }
        // An empty `add_dir` turns the built-in one off.
        if let Some(argv) = agent.add_dir.as_ref().filter(|argv| !argv.is_empty()) {
            validate_arguments(argv, &["{directory}"])
                .with_context(|| format!("machine.ai.{name}.add_dir"))?;
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
        runs_as_agent(agents, base),
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
        runs_as_agent(agents, name),
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

pub(super) fn skill_dir(directory: &Path, home: &Path) -> Result<PathBuf> {
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
