//! `shoal trigger`.
use std::path::Path;

use anyhow::{Context as _, Result};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::triggers::{Fired, RunOutcome, TriggerRun, TriggerStatus, WebhookRequest};

use super::items::{current_context, named_repository};
use crate::{
    cli::{
        args::sessions::TriggerCommand,
        context::Context,
        output::{Palette, Style},
        ui,
    },
    model::Repository,
    protocol::ItemContext,
    time::UnixTime,
};

pub(in crate::cli::commands) async fn run(ctx: &Context, command: TriggerCommand) -> Result<i32> {
    match command {
        TriggerCommand::Ls { repository } => list(ctx, repository).await,
        TriggerCommand::Pause { name, repository } => set_paused(ctx, name, repository, true).await,
        TriggerCommand::Resume { name, repository } => {
            set_paused(ctx, name, repository, false).await
        }
        TriggerCommand::Run {
            name,
            repository,
            payload,
            headers,
        } => {
            let sample = sample_request(payload.as_deref(), &headers)?;
            fire(ctx, name, repository, sample).await
        }
    }
}

async fn list(ctx: &Context, repository: Option<String>) -> Result<i32> {
    let context = trigger_context(ctx, repository).await?;
    let triggers = crate::client::triggers::list(&ctx.paths, context).await?;
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let rows = trigger_rows(&triggers, &repositories, Palette::stdout(ctx.json));
    ctx.show(&triggers, |_| {
        for row in &rows {
            outln!("{row}");
        }
        if rows.is_empty() {
            outln!("No triggers");
        }
    })?;
    Ok(0)
}

async fn set_paused(
    ctx: &Context,
    name: String,
    repository: Option<String>,
    paused: bool,
) -> Result<i32> {
    let context = trigger_context(ctx, repository).await?;
    let status = crate::client::triggers::set_paused(&ctx.paths, context, name, paused).await?;
    let message = match status.next_at {
        _ if status.paused => format!("Paused trigger {}", status.name),
        Some(next) => format!(
            "Resumed trigger {}; it fires next at {}",
            status.name,
            next.local_minutes()
        ),
        None => format!("Resumed trigger {}", status.name),
    };
    ctx.emit(&message, &status)?;
    Ok(0)
}

/// A failed run exits 1.
async fn fire(
    ctx: &Context,
    name: String,
    repository: Option<String>,
    sample: Option<WebhookRequest>,
) -> Result<i32> {
    let context = trigger_context(ctx, repository).await?;
    let fired = crate::client::triggers::run(&ctx.paths, context, name.clone(), sample).await?;
    let message = match &fired {
        Fired::Ran { run } => format!("Trigger {name}: {}", run_text(run)),
        Fired::Dropped { condition } => {
            format!(
                "Trigger {name}: its filter dropped the firing, since {condition} does not hold"
            )
        }
    };
    ctx.emit(&message, &fired)?;
    let failed = matches!(fired, Fired::Ran { run } if run.outcome == RunOutcome::Failed);
    Ok(i32::from(failed))
}

fn sample_request(payload: Option<&Path>, headers: &[String]) -> Result<Option<WebhookRequest>> {
    if payload.is_none() && headers.is_empty() {
        return Ok(None);
    }
    let body = match payload {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("read the payload {}", path.display()))?,
        None => String::new(),
    };
    let headers = (headers.iter())
        .map(|header| match header.split_once('=') {
            Some((name, value)) if !name.trim().is_empty() => {
                Ok((name.trim().to_owned(), value.to_owned()))
            }
            _ => anyhow::bail!("--header {header}: expected NAME=VALUE"),
        })
        .collect::<Result<_>>()?;
    Ok(Some(WebhookRequest {
        headers,
        query: Vec::new(),
        body,
    }))
}

async fn trigger_context(ctx: &Context, repository: Option<String>) -> Result<Option<ItemContext>> {
    match repository {
        Some(repository) => {
            let repository = named_repository(ctx, &repository).await?;
            Ok(Some(ItemContext::Checkout(repository.id)))
        }
        None => current_context(ctx).await,
    }
}

fn trigger_rows(
    triggers: &[TriggerStatus],
    repositories: &[Repository],
    palette: Palette,
) -> Vec<String> {
    let now = UnixTime::now();
    let rows = (triggers.iter())
        .map(|trigger| {
            let (state, style) = match trigger.paused {
                true => ("paused", Style::Warning),
                false => ("active", Style::Success),
            };
            let next = (trigger.next_at)
                .map_or_else(String::new, |next| format!("next {}", next.local_minutes()));
            let last = (trigger.last_run.as_ref()).map_or_else(String::new, |run| {
                format!("last {} {}", run.at.local_ago(now), run_text(run))
            });
            [
                (trigger.name.clone(), Some(Style::Heading)),
                (
                    configured_in(trigger.repository.as_ref(), repositories),
                    None,
                ),
                (trigger.source.to_string(), Some(Style::Muted)),
                (state.to_owned(), Some(style)),
                (next, None),
                (last, Some(Style::Muted)),
            ]
        })
        .collect();
    ui::aligned(rows, palette)
}

fn configured_in(repository: Option<&RepositoryId>, repositories: &[Repository]) -> String {
    let Some(id) = repository else {
        return "global".to_owned();
    };
    (repositories.iter())
        .find(|repository| repository.id == *id)
        .map_or_else(
            || id.to_string(),
            |repository| repository.display_name().to_owned(),
        )
}

fn run_text(run: &TriggerRun) -> String {
    let launched = (run.space_id.as_ref().map(|space| format!("space {space}")))
        .or_else(|| (run.queue_entry.as_ref()).map(|entry| format!("queued launch {entry}")));
    let detail = match (launched, &run.reason) {
        (_, Some(reason)) => format!(": {reason}"),
        (Some(launched), None) if run.outcome == RunOutcome::Queued => {
            format!(", waiting for {launched}")
        }
        (Some(launched), None) => format!(" {launched}"),
        (None, None) => String::new(),
    };
    format!("{}{detail}", run.outcome)
}

#[cfg(test)]
mod tests;
