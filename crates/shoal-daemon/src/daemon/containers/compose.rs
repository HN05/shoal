//! A runtime's `compose`, run on one workspace's project by its name, with
//! its file while the worktree has it.
use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use shoal_ids::space_ids::SpaceId;
use tokio::process::Command;

use crate::{fsutil::off_runtime, model::PortReservation, tools::Tool};
use shoal_protocol::containers::{
    ContainerProject, ContainerRuntime, ContainerService, PublishedPort,
};

/// How long `up --wait` may pull, build and start the services.
const UP_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How long stopping, removing or listing the services may take.
const TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The Compose project name of the workspace `id`, which no other
/// workspace's project has.
pub(super) fn project_name(id: &SpaceId) -> String {
    // Compose takes lowercase letters, digits, dashes and underscores.
    let id: String = (id.as_str().chars())
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    format!("shoal-{id}")
}

/// The runtime the machine names, else the first one on `PATH`.
pub(super) fn find_runtime(configured: Option<ContainerRuntime>) -> Result<ContainerRuntime> {
    let candidates = match configured {
        Some(runtime) => vec![runtime],
        None => ContainerRuntime::ALL.to_vec(),
    };
    match candidates
        .into_iter()
        .find(|runtime| tool(*runtime).installed())
    {
        Some(runtime) => Ok(runtime),
        None => match configured {
            Some(runtime) => bail!("{runtime} is not installed or not on PATH"),
            None => bail!("neither docker nor podman is installed or on PATH"),
        },
    }
}

fn tool(runtime: ContainerRuntime) -> Tool {
    match runtime {
        ContainerRuntime::Docker => Tool::Docker,
        ContainerRuntime::Podman => Tool::Podman,
    }
}

/// `compose` of the runtime that runs `project`.
pub(super) struct Compose<'a> {
    runtime: ContainerRuntime,
    project: &'a str,
    /// The worktree and its Compose file, while both exist.
    file: Option<(&'a Path, &'a str)>,
    /// The workspace's reserved ports, which the file reads from their
    /// variables.
    ports: Vec<PortReservation>,
}

impl<'a> Compose<'a> {
    /// `compose` on `project` in its workspace's `worktree`, with `ports` in
    /// their variables: with its file while the worktree has it, as
    /// podman-compose needs, and by its name alone otherwise, as for a
    /// deleted worktree.
    pub(super) async fn at(
        project: &'a ContainerProject,
        worktree: Option<&'a Path>,
        ports: Vec<PortReservation>,
    ) -> Self {
        let file = match worktree {
            Some(worktree) => (off_runtime::try_exists(&worktree.join(&project.compose)).await)
                .unwrap_or(false)
                .then_some((worktree, project.compose.as_str())),
            None => None,
        };
        Self {
            runtime: project.runtime,
            project: &project.project,
            file,
            ports,
        }
    }

    /// Start the services of the file and wait until they run or report
    /// healthy.
    pub(super) async fn up(&self) -> Result<()> {
        ensure!(
            self.file.is_some(),
            "the worktree has no Compose file to start"
        );
        let mut command = self.command(true);
        command.args(["up", "--detach", "--wait"]);
        run(command, UP_TIMEOUT).await.map(drop)
    }

    /// Stop the services, keeping their containers and volumes.
    pub(super) async fn stop(&self) -> Result<()> {
        self.run_by_name_if_needed(&["stop"]).await
    }

    /// Remove the services with their volumes, and containers the file no
    /// longer names.
    pub(super) async fn down(&self) -> Result<()> {
        (self.run_by_name_if_needed(&["down", "--volumes", "--remove-orphans"])).await
    }

    /// The services with the ports the runtime published for them.
    pub(super) async fn services(&self) -> Result<Vec<ContainerService>> {
        let mut command = self.command(true);
        command.args(["ps", "--all", "--format", "json"]);
        parse_services(&run(command, TIMEOUT).await?)
    }

    /// Run `args` with the file, then by the project's name alone when that
    /// fails, as it does for a file that no longer reads, so the project
    /// still stops and goes.
    async fn run_by_name_if_needed(&self, args: &[&str]) -> Result<()> {
        let mut command = self.command(true);
        command.args(args);
        let with_file = run(command, TIMEOUT).await;
        let Err(error) = with_file.as_ref() else {
            return Ok(());
        };
        if self.file.is_none() {
            return with_file.map(drop);
        }
        let mut command = self.command(false);
        command.args(args);
        let by_name = run(command, TIMEOUT).await;
        by_name
            .map(drop)
            .with_context(|| format!("and with its file: {error:#}"))
    }

    /// `<runtime> compose --project-name <project>`, with the ports in their
    /// variables, and with `-f <file>` in the worktree when `with_file` and
    /// it has the file.
    fn command(&self, with_file: bool) -> Command {
        let mut command = Command::new(tool(self.runtime).program());
        command.args(["compose", "--project-name", self.project]);
        if let Some((worktree, file)) = self.file.filter(|_| with_file) {
            command.current_dir(worktree).args(["-f", file]);
        }
        for port in &self.ports {
            command.env(&port.env_var, port.port.to_string());
        }
        command
    }
}

async fn run(command: Command, timeout: Duration) -> Result<String> {
    let described = format!("compose {}", describe(&command));
    crate::subprocess::Run::new(command)
        .timeout(timeout)
        .output()
        .await
        .context(described)
}

/// The arguments after `compose`, which say what it was asked.
fn describe(command: &Command) -> String {
    let args: Vec<_> = command.as_std().get_args().skip(3).collect();
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

/// One container of `compose ps --format json`.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Listed {
    service: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    publishers: Vec<Publisher>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Publisher {
    target_port: u16,
    published_port: u16,
    #[serde(default)]
    protocol: String,
}

/// The services `compose ps` lists, as a JSON array or one object per line
/// as Compose releases differ, each published port once.
fn parse_services(output: &str) -> Result<Vec<ContainerService>> {
    let trimmed = output.trim();
    let listed: Vec<Listed> = if trimmed.starts_with('[') {
        serde_json::from_str(trimmed)?
    } else {
        (trimmed.lines())
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<serde_json::Result<_>>()?
    };
    Ok(listed.into_iter().map(service).collect())
}

fn service(listed: Listed) -> ContainerService {
    let mut ports: Vec<PublishedPort> = Vec::new();
    // Unpublished ports list as 0, and a port published on both address
    // families lists twice.
    for publisher in listed.publishers {
        let port = PublishedPort {
            target: publisher.target_port,
            published: publisher.published_port,
            protocol: publisher.protocol,
        };
        if port.published != 0 && !ports.contains(&port) {
            ports.push(port);
        }
    }
    ContainerService {
        service: listed.service,
        state: listed.state,
        ports,
    }
}

#[cfg(test)]
mod tests;
