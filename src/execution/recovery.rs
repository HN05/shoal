//! Restore commands omit the original task and remain available after wrapper exit.
use std::{ffi::OsString, path::PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    cli::client,
    config::named_commands,
    fsutil::{Permissions, ReplaceOptions},
    model::Workspace,
    paths::Paths,
    protocol::ConfigTarget,
};

#[derive(Debug)]
pub(crate) struct Recovery {
    pub agent: String,
    pub command: Vec<OsString>,
    pub automatic: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Record {
    pub agent: String,
}

impl Recovery {
    pub(super) async fn for_launch(paths: &Paths, workspace_id: &str, agent: &str) -> Self {
        let resolved = async {
            let workspace = client::inspect(paths, workspace_id.to_owned())
                .await?
                .workspace;
            Self::resolve(paths, &workspace, agent).await
        }
        .await;
        resolved.unwrap_or_else(|error| {
            eprintln!("warning: automatic agent recovery unavailable: {error:#}");
            Self {
                agent: agent.replace(' ', "-"),
                command: vec![],
                automatic: false,
            }
        })
    }

    pub(crate) async fn resolve(paths: &Paths, workspace: &Workspace, agent: &str) -> Result<Self> {
        let settings =
            client::settings(paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
        let name = agent.replace(' ', "-");
        let automatic = settings.agent_resume.contains_key(&name);
        let command = if automatic {
            named_commands::expand(paths, &settings.agent_resume, &name, workspace, vec![]).await?
        } else {
            match name.as_str() {
                "codex" => {
                    named_commands::expand(
                        paths,
                        &settings.commands,
                        &name,
                        workspace,
                        vec!["resume".into()],
                    )
                    .await?
                }
                "claude" => {
                    named_commands::expand(
                        paths,
                        &settings.commands,
                        &name,
                        workspace,
                        vec!["--resume".into()],
                    )
                    .await?
                }
                _ => vec![],
            }
        };
        Ok(Self {
            agent: name,
            command,
            automatic,
        })
    }

    pub(super) fn save(&self, paths: &Paths, workspace_id: &str, id: &str) -> Result<PathBuf> {
        let directory = paths.workspace_state(workspace_id);
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{id}.recovery.json"));
        crate::fsutil::replace_atomically(
            &path,
            &serde_json::to_vec(&Record {
                agent: self.agent.clone(),
            })?,
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: true,
            },
        )?;
        Ok(path)
    }
}

pub(super) async fn wait(
    stream: &mut tokio::net::UnixStream,
) -> Result<Option<Vec<crate::model::PortReservation>>> {
    use crate::protocol::{self, Control};
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut quit = signal(SignalKind::quit())?;
    tokio::select! {
        control = protocol::read::<Control>(stream) => match control? {
            Control::Resume { ports } => Ok(Some(ports)),
            Control::Stop => Ok(None),
            _ => anyhow::bail!("unexpected recovery control"),
        },
        _ = interrupt.recv() => Ok(None),
        _ = terminate.recv() => Ok(None),
        _ = quit.recv() => Ok(None),
    }
}

/// Pending recovery is unfinished work even when its Git tree is clean.
pub fn pending(paths: &Paths, workspace_id: &str) -> Result<bool> {
    let entries = match std::fs::read_dir(paths.workspace_state(workspace_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .ends_with(".recovery.json")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn consume(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
