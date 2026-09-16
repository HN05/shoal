//! CLI workspace workflows; all state mutations go through the daemon.
use super::output;
use crate::{
    client,
    paths::Paths,
    protocol::{Body, Method},
    ui,
};
use crate::{execution, removal, shell};
use anyhow::Result;
use anyhow::ensure;
use serde_json::json;
use std::ffi::OsString;

pub(super) async fn pull(
    paths: &Paths,
    workspace: Option<String>,
    json_output: bool,
) -> Result<i32> {
    let workspace = ui::workspace(paths, workspace, true, json_output).await?;
    let Body::PulledMain(result) = client::call(paths, Method::PullMain { workspace }).await?
    else {
        anyhow::bail!("unexpected pull response");
    };
    if json_output {
        println!("{}", serde_json::to_string(&result)?);
    } else if result.updated {
        println!("Updated main to {}", result.commit);
    } else {
        println!("Main is already up to date ({})", result.commit);
    }
    Ok(0)
}

pub(super) async fn diff(
    paths: &Paths,
    workspace: Option<String>,
    json_output: bool,
) -> Result<i32> {
    let workspace = ui::workspace(paths, workspace, true, json_output).await?;
    let base = match client::call(paths, Method::DiffBase { workspace }).await? {
        Body::DiffBase(base) => base,
        _ => anyhow::bail!("unexpected diff base response"),
    };
    execution::run(
        paths,
        base.workspace_id,
        vec!["git".into(), "diff".into(), base.commit.into(), "--".into()],
    )
    .await
}

pub(super) async fn cd(paths: &Paths, workspace: Option<String>, json_output: bool) -> Result<i32> {
    if workspace.as_deref() == Some("-") {
        let destination = shell::previous_directory()?;
        if std::env::var_os("SHOAL_SCOPE_TOKEN").is_some() {
            let workspaces = ui::workspaces(paths).await?;
            ensure!(
                workspaces.iter().any(|w| std::fs::canonicalize(&w.path)
                    .is_ok_and(|root| destination.starts_with(root))),
                "workspace processes cannot navigate outside their worktree"
            );
        }
        output(
            json_output,
            &destination.display().to_string(),
            json!({"path": destination}),
        );
        shell::navigate(&destination, json_output)?;
    } else {
        let workspace = match workspace {
            Some(workspace) => workspace,
            None => ui::workspace_picker(paths, json_output).await?,
        };
        enter_workspace(paths, workspace, json_output).await?;
    }
    Ok(0)
}

pub(super) async fn add(
    paths: &Paths,
    repository: Option<String>,
    name: Option<String>,
    base: Option<String>,
    json_output: bool,
) -> Result<i32> {
    let repository = match repository {
        Some(repo) => ui::repository_selector(repo)?,
        None => ui::pick(
            "Repository> ",
            ui::repository_choices(ui::repositories(paths).await?).await?,
            json_output,
        )?,
    };
    let name = match name {
        Some(name) => name,
        None => ui::input("Workspace name", json_output)?,
    };
    match client::call(
        paths,
        Method::Add {
            repository,
            name,
            base,
        },
    )
    .await?
    {
        Body::Workspace(workspace) => {
            output(
                json_output,
                &format!("Created {} at {}", workspace.name, workspace.path.display()),
                serde_json::to_value(&workspace)?,
            );
            shell::navigate(&workspace.path, json_output)?;
        }
        _ => anyhow::bail!("unexpected workspace response"),
    }
    Ok(0)
}

pub(super) async fn list(paths: &Paths, json_output: bool) -> Result<i32> {
    let workspaces = ui::workspaces(paths).await?;
    if json_output {
        println!("{}", serde_json::to_string(&workspaces)?);
    } else {
        for w in workspaces {
            println!(
                "{}  {}  {}  {}",
                w.name,
                w.state,
                w.branch,
                w.path.display()
            );
        }
    }
    Ok(0)
}

pub(super) async fn inspect(
    paths: &Paths,
    workspace: Option<String>,
    json_output: bool,
) -> Result<i32> {
    let workspace = ui::workspace(paths, workspace, false, json_output).await?;
    match client::call(paths, Method::Inspect { workspace }).await? {
        Body::Inspection(inspection) => {
            if json_output {
                println!("{}", serde_json::to_string(&inspection)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&inspection)?);
            }
        }
        _ => anyhow::bail!("unexpected inspection response"),
    }
    Ok(0)
}

pub(super) async fn stop(
    paths: &Paths,
    workspace: Option<String>,
    json_output: bool,
) -> Result<i32> {
    let workspace = ui::workspace(paths, workspace, false, json_output).await?;
    client::call(paths, Method::Stop { workspace }).await?;
    output(
        json_output,
        "Workspace processes stopped",
        json!({"stopped": true}),
    );
    Ok(0)
}

pub(super) async fn remove(
    paths: &Paths,
    workspace: Option<String>,
    yes: bool,
    keep_branch: bool,
    delete_branch: bool,
    json_output: bool,
) -> Result<i32> {
    let workspace = ui::workspace(paths, workspace, true, json_output).await?;
    let caller_pid = std::process::id();
    let check = match client::call(
        paths,
        Method::CheckRemoval {
            workspace: workspace.clone(),
            caller_pid,
        },
    )
    .await?
    {
        Body::RemovalCheck(check) => check,
        _ => anyhow::bail!("unexpected removal check response"),
    };
    let choice = if keep_branch {
        removal::Choice::KeepBranch
    } else if delete_branch {
        removal::Choice::DeleteBranch
    } else if !check.needs_choice() {
        removal::Choice::Auto
    } else {
        ensure!(
            !yes,
            "choose --keep-branch or --delete-branch with --yes for a dirty or differing workspace"
        );
        ui::choose_removal(&check, json_output)?
    };
    let inspection = match client::call(
        paths,
        Method::Inspect {
            workspace: workspace.clone(),
        },
    )
    .await?
    {
        Body::Inspection(inspection) => inspection,
        _ => anyhow::bail!("unexpected inspection response"),
    };
    let cwd = std::env::current_dir()?;
    let inside =
        std::fs::canonicalize(&inspection.workspace.path).is_ok_and(|root| cwd.starts_with(root));
    let destination = if inside {
        ui::repositories(paths)
            .await?
            .into_iter()
            .find(|r| r.id == inspection.workspace.repository_id)
            .map(|r| r.path)
    } else {
        None
    };
    let result = client::call(
        paths,
        Method::Remove {
            workspace,
            choice,
            caller_pid,
        },
    )
    .await;
    if inside && (result.is_ok() || !cwd.exists()) {
        let destination = destination
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| paths.home.clone());
        shell::navigate(&destination, json_output)?;
    }
    let result = match result? {
        Body::RemovalResult(result) => result,
        _ => anyhow::bail!("unexpected removal response"),
    };
    let message = match (&result.branch, result.branch_deleted) {
        (Some(branch), true) => format!("Workspace and Git branch {branch} removed"),
        (Some(branch), false) => format!(
            "Workspace removed; Git branch {branch} retained ({})",
            result.branch_outcome
        ),
        (None, _) => "Workspace removed".into(),
    };
    output(json_output, &message, serde_json::to_value(&result)?);
    Ok(0)
}

pub(super) async fn exec(
    paths: &Paths,
    workspace: Option<String>,
    command: Vec<OsString>,
    json_output: bool,
) -> Result<i32> {
    let workspace = ui::workspace(paths, workspace, true, json_output).await?;
    execution::run(paths, workspace, command).await
}

pub(super) async fn claude(
    paths: &Paths,
    workspace: Option<String>,
    args: Vec<OsString>,
    json_output: bool,
) -> Result<i32> {
    let workspace = ui::workspace(paths, workspace, true, json_output).await?;
    let Body::Inspection(inspection) = client::call(paths, Method::Inspect { workspace }).await?
    else {
        anyhow::bail!("unexpected workspace response");
    };
    let command = std::iter::once("claude".into())
        .chain(args)
        .chain(["--remote-control".into(), inspection.workspace.name.into()])
        .collect();
    execution::run(paths, inspection.workspace.id, command).await
}

pub(super) async fn codex(
    paths: &Paths,
    workspace: Option<String>,
    args: Vec<OsString>,
    json_output: bool,
) -> Result<i32> {
    let workspace = ui::workspace(paths, workspace, true, json_output).await?;
    let command = std::iter::once("codex".into())
        .chain(args)
        .chain([
            "--sandbox".into(),
            "workspace-write".into(),
            "--ask-for-approval=never".into(),
        ])
        .collect();
    execution::run(paths, workspace, command).await
}

async fn enter_workspace(paths: &Paths, workspace: String, json_output: bool) -> Result<()> {
    let Body::Inspection(inspection) = client::call(paths, Method::Inspect { workspace }).await?
    else {
        anyhow::bail!("unexpected inspection response");
    };
    ensure!(
        inspection.workspace.path.is_dir(),
        "workspace directory is missing"
    );
    output(
        json_output,
        &inspection.workspace.path.display().to_string(),
        json!({"path": inspection.workspace.path}),
    );
    shell::navigate(&inspection.workspace.path, json_output)
}
