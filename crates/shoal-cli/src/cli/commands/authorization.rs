//! What this process's permissions let it take: the daemon decides each
//! action a permission key governs, and records the approval request a
//! `propose` asks for.
use anyhow::{Result, ensure};

use crate::{
    cli::{Command, context::Context},
    config::permissions::{Acting, PermissionKey},
    env,
};

/// Refuse a scoped process `command` where its space's permissions do not
/// let it take what it does, asking the daemon, which records the approval
/// request a permission asks for. An approval lets the requests the command
/// sends take what it governs.
pub(super) async fn authorize(ctx: &Context, command: &Command) -> Result<()> {
    ensure!(
        !(command.rewrites_config() && env::inherits_scope(&ctx.paths.state)),
        "a scoped process changes Shoal's config only key by key, which it may check gives only the permissions it holds; ask the user to run this command"
    );
    match command.permission() {
        Some(key) => authorize_in(ctx, key, command.repository()).await,
        None => Ok(()),
    }
}

/// Refuse a scoped process what the permission `key` governs, as
/// [`authorize`] refuses a command, once the command knows it takes it.
pub(in crate::cli::commands) async fn authorize_key(
    ctx: &Context,
    key: PermissionKey,
) -> Result<()> {
    authorize_in(ctx, key, None).await
}

/// [`authorize_key`] for an action in the registered repository
/// `repository` names, in whose config a chat without one resolves it.
async fn authorize_in(ctx: &Context, key: PermissionKey, repository: Option<&str>) -> Result<()> {
    if !env::inherits_scope(&ctx.paths.state) {
        return Ok(());
    }
    // Without its scope token, the process would act as the user.
    ensure!(
        env::is_scoped(),
        "this process runs below a scoped one without its scope; run it where the agent runs"
    );
    // The daemon decides the permission, so none can when no daemon of this
    // Shoal answers, as before a start or an install after an upgrade.
    if key == PermissionKey::Daemon {
        let answers = matches!(crate::cli::client::status(&ctx.paths).await, Ok(Some(_)));
        ensure!(
            answers,
            "no daemon of this Shoal answers to decide this process's daemon permission; ask the user to run this command"
        );
    }
    let repository = (repository.map(str::to_owned))
        .map(crate::cli::ui::repository_selector)
        .transpose()?;
    let action = command_line();
    let granted = crate::client::approvals::authorize(&ctx.paths, key, action, repository).await?;
    if let Some(token) = granted {
        env::act_under_grant(token);
    }
    Ok(())
}

/// Whether this process may take what the permission `key` governs, at once
/// or after approval: always for the user.
pub(in crate::cli::commands) async fn permits(ctx: &Context, key: PermissionKey) -> Result<bool> {
    if !env::inherits_scope(&ctx.paths.state) {
        return Ok(true);
    }
    let held = crate::client::workspaces::held_permissions(&ctx.paths, None).await?;
    Ok(held.acting(key) != Acting::Deny)
}

/// This command as the user typed it, which an approval of it names.
fn command_line() -> String {
    let words: Vec<String> = std::iter::once("shoal".to_owned())
        .chain(std::env::args().skip(1))
        .collect();
    crate::shell::quote(&words)
}
