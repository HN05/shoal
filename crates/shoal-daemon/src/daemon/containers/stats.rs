//! What each workspace's running containers use, as the runtime's `stats`
//! reads them: the containers Compose labelled with the project name Shoal
//! gave it, so no Compose file is read.
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use shoal_ids::space_ids::SpaceId;
use tokio::process::Command;

use super::{compose, records};
use crate::daemon::workspace::Manager;
use shoal_protocol::containers::ContainerProject;

/// The label Compose gives each container of a project, holding its name.
const PROJECT_LABEL: &str = "com.docker.compose.project";

/// How long listing or reading a project's containers may take: well
/// within the time a usage sample stays current.
const TIMEOUT: Duration = Duration::from_secs(15);

/// How many projects are read with the runtime at once.
const PROJECTS_READ_AT_ONCE: usize = 4;

/// What a project's running containers use together.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct ContainerUsage {
    /// Percent of one core.
    pub(crate) cpu_percent: f64,
    pub(crate) memory_bytes: u64,
}

impl Manager {
    /// What the containers of each workspace whose project may run use, or
    /// why its runtime could not say.
    pub(crate) async fn container_usage(&self) -> Result<Vec<(SpaceId, Result<ContainerUsage>)>> {
        let recorded = self.store.read(|db| records::list(db, None)).await?;
        let active = (recorded.into_iter()).filter(|recorded| recorded.project.state.active());
        Ok(stream::iter(active)
            .map(|recorded| async move {
                let usage = read_project(&recorded.project).await;
                (recorded.project.workspace_id, usage)
            })
            .buffer_unordered(PROJECTS_READ_AT_ONCE)
            .collect()
            .await)
    }
}

/// What the running containers of `project` use, nothing while none run.
async fn read_project(project: &ContainerProject) -> Result<ContainerUsage> {
    let program = compose::tool(project.runtime).program();
    let containers = running_containers(program, &project.project).await?;
    if containers.is_empty() {
        return Ok(ContainerUsage::default());
    }
    read_stats(program, &containers).await
}

/// The IDs of the running containers Compose labelled with `project`.
async fn running_containers(program: &str, project: &str) -> Result<Vec<String>> {
    let mut list = Command::new(program);
    let label = format!("label={PROJECT_LABEL}={project}");
    list.args(["ps", "--quiet", "--filter", &label]);
    let listed = run(list).await?;
    Ok(listed.split_whitespace().map(str::to_owned).collect())
}

/// What `containers` use together, as one `stats` reads them.
async fn read_stats(program: &str, containers: &[String]) -> Result<ContainerUsage> {
    let mut stats = Command::new(program);
    stats.args(["stats", "--no-stream", "--format", "json"]);
    stats.args(containers);
    parse_stats(&run(stats).await?).with_context(|| format!("{program} stats"))
}

async fn run(command: Command) -> Result<String> {
    let described = describe(&command);
    crate::subprocess::Run::new(command)
        .timeout(TIMEOUT)
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

/// One container of `stats --format json`, in Docker's spelling or
/// Podman's.
#[derive(Deserialize)]
struct Stats {
    #[serde(rename = "CPUPerc", alias = "cpu_percent")]
    cpu: String,
    /// What it uses, then its limit: `1.5MiB / 7.6GiB`.
    #[serde(rename = "MemUsage", alias = "mem_usage")]
    memory: String,
}

/// What the containers `stats` lists use together.
fn parse_stats(output: &str) -> Result<ContainerUsage> {
    let listed: Vec<Stats> = compose::parse_listing(output)?;
    let mut total = ContainerUsage::default();
    for stats in listed {
        total.cpu_percent += percent(&stats.cpu)?;
        let used = stats.memory.split('/').next().unwrap_or_default();
        total.memory_bytes = total.memory_bytes.saturating_add(bytes(used)?);
    }
    Ok(total)
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
