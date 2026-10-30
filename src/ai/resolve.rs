//! Each AI tool resolved once from the `[ai]` entries along its `base`
//! lineage, which nothing else walks.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;

use super::{
    Agent, Agents, Arguments, Environment,
    builtin::{self, Provider},
    validate::skill_dir,
};

/// Every built-in and configured AI tool by name.
pub type Tools = BTreeMap<String, Tool>;

/// One AI tool after its lineage: each setting is its own, else the
/// nearest base's, else the built-in tool's its lineage ends at.
#[derive(Debug, Clone)]
pub struct Tool {
    /// Identity: what `--agent`, `shoal <name>` and recovery records use.
    pub name: String,
    /// Whether it runs as an agent of its own, as `shoal <name>` does:
    /// built in, or configured with a launcher or a base. A tool with only
    /// a skill directory or argument templates is not one.
    pub runs_as_agent: bool,
    /// What its launcher takes, including that of a `[commands]` agent
    /// whose `[ai]` entry only describes arguments.
    pub launch: LaunchProfile,
    /// Which Shoal integration it runs with.
    pub integration: IntegrationKind,
    /// The tools it is a launch profile of, nearest first. A launcher is
    /// resolved per config layer, so it is looked up along them there.
    pub bases: Vec<String>,
    skill_dir: Option<SkillDir>,
    /// Whether Shoal retries a turn an API failure ended; on by default for
    /// the Claude integration.
    pub retry: bool,
    /// The `[proxy.<name>]` its tracked agents reach their API through: the
    /// one named nearest in its lineage.
    pub proxy: Option<String>,
    /// `prompt_cache_minutes` configured nearest in its lineage.
    configured_prompt_cache: Option<Duration>,
}

/// How long an agent's prompt cache lives after a turn when neither its AI
/// tool's configuration nor its own settings say: the least OpenAI keeps a
/// GPT-5.6 or later cache, which Codex has no setting for, and a guess for
/// other tools that keeps idle stop from treating their caches as expired.
pub const DEFAULT_PROMPT_CACHE: Duration = Duration::from_secs(30 * 60);

/// What a tool's launcher takes besides its command, which `[commands]` may
/// replace per repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchProfile {
    pub arguments: Arguments,
    pub environment: Environment,
}

crate::state::states!(
    /// The Shoal integration a tool runs with: that of the built-in tool its
    /// lineage ends at. Agent hooks name it to the commands they run.
    IntegrationKind: ValueEnum {
        Claude => "claude",
        Codex => "codex",
        /// Configuration says everything about the tool.
        None => "none",
    }
);

#[derive(Debug, Clone)]
enum SkillDir {
    /// A `skill_dir` as written.
    Configured(PathBuf),
    Builtin(builtin::SkillDir),
}

impl Tool {
    /// How long its prompt cache lives after a turn whose agent `reported`
    /// the lifetime its own settings give: as configured, else as reported,
    /// else [`DEFAULT_PROMPT_CACHE`].
    pub fn prompt_cache(&self, reported: Option<Duration>) -> Duration {
        self.configured_prompt_cache
            .or(reported)
            .unwrap_or(DEFAULT_PROMPT_CACHE)
    }

    /// The tool's user-level skill directory for a launch from this process;
    /// `None` when it has none.
    pub fn skill_dir(&self, home: &Path) -> Result<Option<PathBuf>> {
        match &self.skill_dir {
            None => Ok(None),
            Some(SkillDir::Configured(directory)) => skill_dir(directory, home).map(Some),
            Some(SkillDir::Builtin(directory)) => {
                let environment = self.launch.environment.values(home);
                directory.path(&environment, home).map(Some)
            }
        }
    }
}

/// Every built-in tool and every configured one, resolved.
pub(super) fn tools(entries: &Agents) -> Tools {
    builtin::PROVIDERS
        .iter()
        .map(|provider| provider.name)
        .chain(entries.keys().map(String::as_str))
        .map(|name| (name.to_owned(), tool(entries, name)))
        .collect()
}

/// Whether `name` runs as an agent: built in, or configured with a launcher
/// or a base. A tool with only a skill directory is not one.
pub(super) fn runs_as_agent(entries: &Agents, name: &str) -> bool {
    builtin::find(name).is_some()
        || entries
            .get(name)
            .is_some_and(|agent| agent.command.is_some() || agent.base.is_some())
}

/// Give each launch profile without a launcher of its own in `commands` the
/// nearest one of its bases there, so it follows whatever layer set it.
/// `commands` may carry each launcher's layer with it.
pub fn inherit_launchers<T: Clone>(tools: &Tools, commands: &mut BTreeMap<String, T>) {
    for (name, tool) in tools {
        if commands.contains_key(name) {
            continue;
        }
        let inherited = tool
            .bases
            .iter()
            .find_map(|base| commands.get(base))
            .cloned();
        if let Some(argv) = inherited {
            commands.insert(name.clone(), argv);
        }
    }
}

fn tool(entries: &Agents, name: &str) -> Tool {
    let lineage = lineage(entries, name);
    let configured: Vec<&Agent> = lineage
        .iter()
        .filter_map(|name| entries.get(*name))
        .collect();
    let builtin = lineage.last().and_then(|root| builtin::find(root));
    let launch = LaunchProfile {
        arguments: arguments(&configured, builtin),
        environment: environment(&configured),
    };
    let integration = builtin.map_or(IntegrationKind::None, |provider| provider.integration);
    Tool {
        name: name.to_owned(),
        runs_as_agent: runs_as_agent(entries, name),
        launch,
        integration,
        bases: lineage[1..].iter().map(|base| (*base).to_owned()).collect(),
        skill_dir: skill_directory(&configured, builtin),
        retry: retries(&configured, integration),
        proxy: configured.iter().find_map(|agent| agent.proxy.clone()),
        configured_prompt_cache: configured
            .iter()
            .find_map(|agent| agent.prompt_cache_minutes)
            .map(|minutes| Duration::from_secs(minutes * 60)),
    }
}

/// `name` and the tools it is a launch profile of, nearest first.
/// Validation rejects cycles; the bound keeps an unvalidated one finite.
fn lineage<'a>(entries: &'a Agents, name: &'a str) -> Vec<&'a str> {
    std::iter::successors(Some(name), |name| entries.get(*name)?.base.as_deref())
        .take(entries.len() + 1)
        .collect()
}

/// The `skill_dir` configured nearest in `configured`, the entries of a
/// lineage, else `builtin`'s.
fn skill_directory(configured: &[&Agent], builtin: Option<&Provider>) -> Option<SkillDir> {
    configured
        .iter()
        .find_map(|agent| agent.skill_dir.clone())
        .map(SkillDir::Configured)
        .or_else(|| builtin.map(|provider| SkillDir::Builtin(provider.skill_dir)))
}

/// The `retry` configured nearest in `configured`, the entries of a lineage;
/// without one, on for the Claude integration.
fn retries(configured: &[&Agent], integration: IntegrationKind) -> bool {
    configured
        .iter()
        .find_map(|agent| agent.retry)
        .unwrap_or(integration == IntegrationKind::Claude)
}

/// Each argument template configured nearest in `configured`, the entries
/// of a lineage, else `builtin`'s.
fn arguments(configured: &[&Agent], builtin: Option<&Provider>) -> Arguments {
    let owned = |argv: &[&str]| argv.iter().map(|arg| arg.to_string()).collect();
    let pick = |entry: fn(&Agent) -> Option<&Vec<String>>, builtin: Option<&[&str]>| {
        configured
            .iter()
            .find_map(|agent| entry(agent))
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
        fork: pick(
            |agent| agent.fork.as_ref(),
            builtin.and_then(|provider| provider.fork),
        ),
        add_dir: pick(
            |agent| agent.add_dir.as_ref(),
            builtin.and_then(|provider| provider.add_dir),
        ),
    }
}

/// The environment changes of `configured`, the entries of a lineage: a
/// profile's own win over its base's.
fn environment(configured: &[&Agent]) -> Environment {
    let mut environment = Environment::default();
    for agent in configured.iter().rev() {
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
