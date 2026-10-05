//! Herdr terminal handoffs; workspace ownership stays with the daemon.
use std::{ffi::OsString, path::Path, time::Duration};

use anyhow::{Context as _, Result, ensure};
use serde::Deserialize;
use tokio::process::Command;

use super::{
    client,
    commands::workspaces::{AddPlan, execute_add},
    context::Context,
    internal::{InternalCommand, internal_command},
};
use crate::{config::Config, env, protocol::ConfigTarget, subprocess::Run};

pub struct Tab {
    id: String,
    close_when_done: bool,
}

pub(super) async fn handoff(ctx: &Context, plan: &AddPlan, here: bool) -> Result<bool> {
    if here
        || ctx.herdr_tab.is_some()
        || !ctx.interactive()
        || std::env::var("HERDR_ENV").as_deref() != Ok("1")
    {
        return Ok(false);
    }
    let settings = client::settings(
        &ctx.paths,
        ConfigTarget::Repository(plan.repository.clone()),
    )
    .await?;
    if !settings.herdr.new_tab {
        return Ok(false);
    }
    ensure!(
        !env::is_scoped(),
        "workspace processes cannot allocate workspaces"
    );
    let payload = serde_json::to_string(plan)?;
    let created = create_tab(ctx, plan, settings.herdr.focus).await?;
    submit_worker(ctx, created, settings.herdr.close_when_done, &payload).await?;
    Ok(true)
}

async fn create_tab(ctx: &Context, plan: &AddPlan, focus: bool) -> Result<CreatedResult> {
    let workspace = std::env::var_os("HERDR_WORKSPACE_ID")
        .filter(|id| !id.is_empty())
        .context("Herdr did not provide HERDR_WORKSPACE_ID")?;
    let mut create = Command::new("herdr");
    create
        .args(["tab", "create", "--workspace"])
        .arg(workspace)
        .arg("--cwd")
        .arg(std::env::current_dir()?)
        .arg("--label")
        .arg(plan.label())
        .arg(if focus { "--focus" } else { "--no-focus" });
    // Preserve this CLI's config location, independently of the Herdr server's environment.
    let config = Config::path(&ctx.paths);
    let config_home = config
        .parent()
        .and_then(Path::parent)
        .context("config has no parent")?;
    create
        .arg("--env")
        .arg(format!("HOME={}", ctx.paths.home.display()));
    create
        .arg("--env")
        .arg(format!("XDG_CONFIG_HOME={}", config_home.display()));
    let output = Run::new(create).checked().await?;
    let created: Created =
        serde_json::from_slice(&output.stdout).context("invalid Herdr tab creation response")?;
    Ok(created.result)
}

async fn submit_worker(
    ctx: &Context,
    created: CreatedResult,
    close_when_done: bool,
    payload: &str,
) -> Result<()> {
    let argv = internal_command(
        &ctx.paths,
        false,
        InternalCommand::Herdr {
            tab: &created.tab.tab_id,
            close_when_done,
            plan: payload,
        },
    )?;
    let mut run = Command::new("herdr");
    run.args(["pane", "run"])
        .arg(&created.root_pane.pane_id)
        .arg(shell_command(&argv)?);
    Run::new(run)
        .checked()
        .await
        .context("run Shoal in the new Herdr tab (tab retained)")?;
    Ok(())
}

#[derive(Deserialize)]
struct Created {
    result: CreatedResult,
}
#[derive(Deserialize)]
struct CreatedResult {
    tab: CreatedTab,
    root_pane: CreatedPane,
}
#[derive(Deserialize)]
struct CreatedTab {
    tab_id: String,
}
#[derive(Deserialize)]
struct CreatedPane {
    pane_id: String,
}

// Herdr's pane API sends shell text. Quote every word, including the JSON payload.
fn shell_command(argv: &[OsString]) -> Result<String> {
    argv.iter()
        .map(|arg| {
            let arg = arg.to_str().context("Herdr command path is not UTF-8")?;
            Ok(format!("'{}'", arg.replace('\'', "'\\''")))
        })
        .collect::<Result<Vec<_>>>()
        .map(|words| words.join(" "))
}

pub async fn worker(
    mut ctx: Context,
    id: String,
    close_when_done: bool,
    payload: &str,
) -> Result<i32> {
    let plan = serde_json::from_str(payload).context("invalid Herdr workspace plan")?;
    ctx.herdr_tab = Some(Tab {
        id,
        close_when_done,
    });
    execute_add(&ctx, plan).await
}

impl Tab {
    pub async fn rename(&self, branch: &str) {
        let mut rename = Command::new("herdr");
        rename.args(["tab", "rename"]).arg(&self.id).arg(branch);
        if let Err(error) = Run::new(rename).checked().await {
            eprintln!("warning: cannot rename Herdr tab {}: {error:#}", self.id);
        }
    }

    pub async fn close(&self) {
        if !self.close_when_done {
            return;
        }
        let mut close = Command::new("herdr");
        close.args(["tab", "close"]).arg(&self.id);
        if let Err(error) = Run::new(close).checked().await {
            eprintln!("warning: cannot close Herdr tab {}: {error:#}", self.id);
        }
    }

    pub async fn shell(&self, cwd: &Path) -> Result<i32> {
        let shell = std::env::var_os("SHELL")
            .filter(|shell| !shell.is_empty())
            .unwrap_or_else(|| "/bin/sh".into());
        let status = Command::new(shell)
            .arg("-i")
            .current_dir(cwd)
            .env_remove(env::SHELL_DIRECTIVE)
            .status()
            .await
            .context("open workspace shell")?;
        Ok(crate::execution::exit_code(status))
    }

    pub async fn wait_for_detached(
        &self,
        ctx: &Context,
        workspace: &str,
        execution: &str,
    ) -> Result<()> {
        if !self.close_when_done {
            return Ok(());
        }
        loop {
            let active = match client::inspect(&ctx.paths, workspace.into()).await {
                Ok(inspection) => inspection
                    .executions
                    .iter()
                    .any(|item| item.id == execution),
                Err(error) => {
                    // Removal may finish between polls. Other failures do not prove exit.
                    if client::workspaces(&ctx.paths)
                        .await?
                        .iter()
                        .any(|item| item.id == workspace)
                    {
                        return Err(error);
                    }
                    false
                }
            };
            if !active {
                self.close().await;
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}
