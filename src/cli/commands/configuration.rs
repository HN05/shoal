use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        client,
        context::Context,
        output::{Palette, Style},
        ui,
        workspace_context::{ScopeOrder, WorkspaceContext},
    },
    config::{self, edit::Change, repo::LocalConfig},
    env,
    protocol::ConfigTarget,
};

/// `KEY VALUE` pairs as the changes they set.
pub(super) fn sets(assignments: Vec<String>) -> Result<Vec<Change>> {
    ensure!(
        assignments.len().is_multiple_of(2),
        "expected KEY VALUE pairs; {} has no value",
        assignments.last().expect("clap requires assignments")
    );
    let mut assignments = assignments.into_iter();
    Ok(std::iter::from_fn(|| {
        Some(Change {
            key: assignments.next()?,
            value: assignments.next(),
        })
    })
    .collect())
}

pub(super) fn unsets(keys: Vec<String>) -> Vec<Change> {
    keys.into_iter()
        .map(|key| Change { key, value: None })
        .collect()
}

pub(super) async fn edit(
    ctx: &Context,
    changes: Vec<Change>,
    repository: Option<String>,
) -> Result<i32> {
    if let Some(repository) = repository {
        let repository = crate::cli::ui::repository_selector(repository)?;
        let config = crate::cli::client::request::<LocalConfig>(
            &ctx.paths,
            crate::protocol::Method::EditRepositoryConfig {
                repository,
                changes,
            },
        )
        .await?;
        ctx.emit("Updated saved repository config", &config)?;
        return Ok(0);
    }
    let (path, backup) = crate::config::Config::edit(&ctx.paths, &changes)?;
    let reloaded = reload_daemon(ctx).await;
    ctx.emit(
        &reload_message(format!("Updated {}", path.display()), reloaded),
        serde_json::json!({"config": path, "backup": backup, "daemon_reloaded": reloaded}),
    )?;
    Ok(0)
}

/// Apply a saved global file to a running daemon. The file is already saved,
/// so a daemon that cannot reload it is a warning rather than a failure.
pub(super) async fn reload_daemon(ctx: &Context) -> bool {
    client::reload_config(&ctx.paths)
        .await
        .unwrap_or_else(|error| {
            eprintln!(
                "warning: the daemon keeps its previous settings: {error:#}; run `shoal daemon reload` or `shoal daemon restart`"
            );
            false
        })
}

pub(super) fn reload_message(message: String, reloaded: bool) -> String {
    if reloaded {
        format!("{message}; the daemon reloaded it")
    } else {
        message
    }
}

pub(super) async fn show(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let target = target(ctx, workspace).await?;
    let entries = config::report::load(&ctx.paths, target).await?;
    ctx.show(&entries, |entries| {
        let palette = Palette::stdout(ctx.json);
        for entry in entries {
            let value = serde_json::to_string(&entry.value).expect("serialize config value");
            println!(
                "{} = {} {}",
                entry.key,
                value,
                palette.paint(Style::Muted, format_args!("({})", entry.layer))
            );
        }
    })?;
    Ok(0)
}

async fn target(ctx: &Context, explicit: Option<String>) -> Result<ConfigTarget> {
    if let Some(workspace) = explicit {
        return Ok(ConfigTarget::Workspace(workspace));
    }
    let workspaces = client::workspaces(&ctx.paths).await?;
    let scoped = env::is_scoped();
    let cwd = if scoped {
        None
    } else {
        Some(std::fs::canonicalize(std::env::current_dir()?)?)
    };
    let context = WorkspaceContext::from_directory(&workspaces, cwd.as_deref());
    if let Some(workspace) = context.resolve(None, scoped, ScopeOrder::BeforeDirectory) {
        return Ok(ConfigTarget::Workspace(workspace.id.clone()));
    }
    let cwd = cwd.context("scoped workspace is unavailable")?;
    let repositories = client::repositories(&ctx.paths).await?;
    let repository = repositories
        .iter()
        .filter(|repository| {
            std::fs::canonicalize(&repository.path).is_ok_and(|path| cwd.starts_with(path))
        })
        .max_by_key(|repository| repository.path.components().count());
    Ok(match repository {
        Some(repository) => ConfigTarget::Repository(repository.id.clone().into()),
        None => ConfigTarget::Workspace(ui::pick_workspace(ctx, workspaces)?),
    })
}
