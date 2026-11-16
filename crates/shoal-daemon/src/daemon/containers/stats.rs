//! What each workspace's running containers use, as the runtime's `stats`
//! reads them: the containers Compose labelled with the project name Shoal
//! gave it, so no Compose file is read.
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use shoal_ids::space_ids::SpaceId;
use tokio::process::Command;

use super::{compose, records};
use crate::daemon::workspace::Manager;
use shoal_protocol::containers::{ContainerProject, ContainerRuntime};

/// The label Compose gives each container of a project, holding its name.
const PROJECT_LABEL: &str = "com.docker.compose.project";

/// How long reading every project's containers may take: a usage sample
/// waits for it, and with the sampling interval it stays within the time a
/// sample stays current, however slow the runtime.
const READ_LIMIT: Duration = Duration::from_secs(15);

/// How many projects' containers are listed with the runtime at once.
const PROJECTS_LISTED_AT_ONCE: usize = 4;

/// What a project's running containers use together.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct ContainerUsage {
    /// Percent of one core.
    pub(crate) cpu_percent: f64,
    pub(crate) memory_bytes: u64,
}

impl Manager {
    /// What the containers of each workspace whose project may run use, or
    /// why its runtime could not say, within [`READ_LIMIT`], which stops
    /// the runtime calls still running.
    pub(crate) async fn container_usage(&self) -> Result<Vec<(SpaceId, Result<ContainerUsage>)>> {
        let recorded = self.store.read(|db| records::list(db, None)).await?;
        let active: Vec<ContainerProject> = (recorded.into_iter())
            .filter(|recorded| recorded.project.state.active())
            .map(|recorded| recorded.project)
            .collect();
        let read = async {
            let mut usage = Vec::new();
            for runtime in ContainerRuntime::ALL {
                let projects = active.iter().filter(|project| project.runtime == runtime);
                usage.extend(read_runtime(runtime, projects.cloned().collect()).await);
            }
            usage
        };
        (tokio::time::timeout(READ_LIMIT, read).await).with_context(|| {
            format!(
                "the container runtime did not answer within {} seconds",
                READ_LIMIT.as_secs()
            )
        })
    }
}

/// What the running containers of each of `projects` on `runtime` use:
/// each project's listing, then one `stats` for all their containers, as
/// it waits a while for each reading whatever their number.
async fn read_runtime(
    runtime: ContainerRuntime,
    projects: Vec<ContainerProject>,
) -> Vec<(SpaceId, Result<ContainerUsage>)> {
    if projects.is_empty() {
        return Vec::new();
    }
    let program = compose::tool(runtime).program();
    let listed: Vec<(SpaceId, Result<Vec<String>>)> = stream::iter(projects)
        .map(|project| async move {
            let containers = running_containers(program, &project.project).await;
            (project.workspace_id, containers)
        })
        .buffer_unordered(PROJECTS_LISTED_AT_ONCE)
        .collect()
        .await;
    let containers: Vec<&String> = (listed.iter())
        .filter_map(|(_, containers)| containers.as_ref().ok())
        .flatten()
        .collect();
    let readings = read_stats(program, &containers).await;
    (listed.into_iter())
        .map(|(workspace, containers)| {
            let usage = containers.and_then(|containers| match &readings {
                Ok(readings) => Ok(project_usage(&containers, readings)),
                Err(error) => Err(anyhow!("{error:#}")),
            });
            (workspace, usage)
        })
        .collect()
}

/// The IDs of the running containers Compose labelled with `project`.
async fn running_containers(program: &str, project: &str) -> Result<Vec<String>> {
    let mut list = Command::new(program);
    let label = format!("label={PROJECT_LABEL}={project}");
    list.args(["ps", "--quiet", "--no-trunc", "--filter", &label]);
    let listed = run(list).await?;
    Ok(listed.split_whitespace().map(str::to_owned).collect())
}

/// What each of `containers` uses, as one `stats` reads them.
async fn read_stats(program: &str, containers: &[&String]) -> Result<Vec<Reading>> {
    if containers.is_empty() {
        return Ok(Vec::new());
    }
    let mut stats = Command::new(program);
    stats.args(["stats", "--no-stream", "--format", "json"]);
    stats.args(containers);
    parse_stats(&run(stats).await?).with_context(|| format!("{program} stats"))
}

async fn run(command: Command) -> Result<String> {
    let described = describe(&command);
    crate::subprocess::Run::new(command)
        .output()
        .await
        .context(described)
}

/// The program with its first argument, which say what it was asked.
fn describe(command: &Command) -> String {
    let command = command.as_std();
    let first = command.get_args().next().unwrap_or_default();
    format!(
        "{} {}",
        command.get_program().to_string_lossy(),
        first.to_string_lossy()
    )
}

/// One container's reading in `stats`.
#[derive(Debug, PartialEq)]
struct Reading {
    /// Its ID, which `stats` may shorten.
    id: String,
    usage: ContainerUsage,
}

/// What the `readings` of a project's `containers`, by their full IDs, add
/// up to.
fn project_usage(containers: &[String], readings: &[Reading]) -> ContainerUsage {
    let ours = |reading: &&Reading| {
        !reading.id.is_empty() && (containers.iter()).any(|id| id.starts_with(&reading.id))
    };
    let mut total = ContainerUsage::default();
    for reading in readings.iter().filter(ours) {
        total.cpu_percent += reading.usage.cpu_percent;
        total.memory_bytes = (total.memory_bytes).saturating_add(reading.usage.memory_bytes);
    }
    total
}

/// One container of `stats --format json`, in Docker's spelling or
/// Podman's.
#[derive(Deserialize)]
struct Stats {
    #[serde(rename = "ID", alias = "id")]
    id: String,
    #[serde(rename = "CPUPerc", alias = "cpu_percent")]
    cpu: String,
    /// What it uses, then its limit: `1.5MiB / 7.6GiB`.
    #[serde(rename = "MemUsage", alias = "mem_usage")]
    memory: String,
}

/// The reading of each container `stats` lists.
fn parse_stats(output: &str) -> Result<Vec<Reading>> {
    let listed: Vec<Stats> = compose::parse_listing(output)?;
    (listed.into_iter())
        .map(|stats| {
            let used = stats.memory.split('/').next().unwrap_or_default();
            Ok(Reading {
                id: stats.id,
                usage: ContainerUsage {
                    cpu_percent: percent(&stats.cpu)?,
                    memory_bytes: bytes(used)?,
                },
            })
        })
        .collect()
}

/// `12.5%`, or `--` for a container without a reading yet.
fn percent(text: &str) -> Result<f64> {
    let text = text.trim();
    if text == "--" {
        return Ok(0.0);
    }
    let number = text.strip_suffix('%').unwrap_or(text).trim();
    number
        .parse()
        .with_context(|| format!("{text:?} is not a percentage"))
}

/// A size in Docker's binary units or Podman's decimal ones, such as
/// `1.5MiB` or `1.5MB`, or `--` for a container without a reading yet.
fn bytes(text: &str) -> Result<u64> {
    let text = text.trim();
    if text == "--" {
        return Ok(0);
    }
    let unit_at = text
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(unit_at);
    let number: f64 = (number.trim().parse()).with_context(|| format!("{text:?} is not a size"))?;
    let factor = match unit {
        "" | "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => bail!("{text:?} is not a size"),
    };
    Ok((number * factor) as u64)
}

#[cfg(test)]
mod tests;
