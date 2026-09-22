use anyhow::{Context as _, Result, ensure};

use crate::{
    client, config_report,
    context::Context,
    env,
    model::Workspace,
    output::{Palette, Style},
    protocol::ConfigTarget,
};

pub(super) async fn edit(
    ctx: &Context,
    key: String,
    value: Option<String>,
    repository: Option<String>,
) -> Result<i32> {
    if let Some(repository) = repository {
        let config = crate::client::request!(
            &ctx.paths,
            crate::protocol::Method::EditRepositoryConfig {
                repository,
                key,
                value
            },
            RepositoryConfig
        );
        ctx.emit("Updated saved repository config", &config)?;
        return Ok(0);
    }
    let (path, backup) = crate::config::Config::edit(&ctx.paths, &key, value.as_deref())?;
    ctx.emit(
        &format!("Updated {}", path.display()),
        serde_json::json!({"config": path, "backup": backup}),
    )?;
    Ok(0)
}

pub(super) async fn show(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let target = target(ctx, workspace).await?;
    let entries = config_report::load(&ctx.paths, target).await?;
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
    if env::is_scoped() {
        return workspaces
            .first()
            .map(|workspace| ConfigTarget::Workspace(workspace.id.clone()))
            .context("scoped workspace is unavailable");
    }
    let cwd = std::fs::canonicalize(std::env::current_dir()?)?;
    if let Some(workspace) = Workspace::innermost(&workspaces, &cwd) {
        return Ok(ConfigTarget::Workspace(workspace.id.clone()));
    }
    let repositories = client::repositories(&ctx.paths).await?;
    let repository = repositories
        .iter()
        .filter(|repository| {
            std::fs::canonicalize(&repository.path).is_ok_and(|path| cwd.starts_with(path))
        })
        .max_by_key(|repository| repository.path.components().count());
    ensure!(
        repository.is_some(),
        "no current workspace or registered checkout; pass an explicit workspace"
    );
    Ok(ConfigTarget::Repository(
        repository.expect("checked above").id.clone(),
    ))
}
