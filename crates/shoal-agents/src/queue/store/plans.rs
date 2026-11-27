//! How a launch's row keeps the start it plans, in the agent queue and among
//! failed launches alike: the launch's agent, mode, permissions and source,
//! and its issue's title and labels, in columns the queue reads, and the
//! rest as the JSON of a [`StoredStart`].
use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::Row;
use serde::{Deserialize, Serialize};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_store::json_column;

use crate::agent::{Agent, ModelChoice};
use crate::config::modes::ModeName;
use crate::forge::items::reference::{ItemInput, ItemRef};
use crate::model::QueuedStart;
use crate::protocol::operations::{LaunchIssue, QueuedLaunch};

/// The columns of a launch's row that hold the start it plans.
pub struct PlannedColumns {
    pub agent: Agent,
    pub mode: Option<ModeName>,
    pub narrowing: Option<String>,
    pub issue_title: Option<String>,
    pub issue_labels: String,
    pub source: Option<String>,
    pub plan: String,
}

impl PlannedColumns {
    /// The columns that keep `start`.
    pub fn of(start: &QueuedStart) -> Result<Self> {
        let QueuedStart { launch, pr } = start;
        let QueuedLaunch {
            agent,
            prompt,
            mode,
            narrowing,
            model,
            args,
            fork,
            issue,
            run,
            env,
            now,
            here,
            replace,
            source,
        } = launch;
        let stored = StoredStart {
            launch: StoredLaunch {
                prompt: prompt.clone(),
                model: model.clone(),
                args: args.clone(),
                fork: fork.clone(),
                issue: issue.as_ref().map(|issue| issue.url.clone()),
                run: run.clone(),
                env: env.clone(),
                now: *now,
                here: *here,
                replace: *replace,
            },
            pr: pr.clone(),
        };
        let labels = issue.as_ref().map_or(&[][..], |issue| &issue.labels);
        Ok(Self {
            agent: agent.clone(),
            mode: mode.clone(),
            narrowing: shoal_store::narrowing_column(narrowing)?,
            issue_title: issue.as_ref().map(|issue| issue.title.clone()),
            issue_labels: serde_json::to_string(labels)?,
            source: source.as_ref().map(serde_json::to_string).transpose()?,
            plan: serde_json::to_string(&stored)?,
        })
    }
}

/// The start the launch's `row` plans, from its columns and its plan,
/// which a restore's or a job's lacks.
pub fn planned_start(row: &Row<'_>) -> rusqlite::Result<QueuedStart> {
    let stored: StoredStart = json_column(row, "plan")?.ok_or_else(|| missing(row, "plan"))?;
    let StoredLaunch {
        prompt,
        model,
        args,
        fork,
        issue,
        run,
        env,
        now,
        here,
        replace,
    } = stored.launch;
    let labels: Vec<String> = json_column(row, "issue_labels")?.unwrap_or_default();
    let issue = match issue {
        Some(url) => Some(LaunchIssue {
            url,
            title: (row.get::<_, Option<String>>("issue_title")?)
                .ok_or_else(|| missing(row, "issue_title"))?,
            labels,
        }),
        None => None,
    };
    let launch = QueuedLaunch {
        agent: row.get("agent")?,
        prompt,
        mode: row.get("mode")?,
        narrowing: json_column(row, "narrowing")?.unwrap_or_default(),
        model,
        args,
        fork,
        issue,
        run,
        env,
        now,
        here,
        replace,
        source: json_column(row, "source")?,
    };
    Ok(QueuedStart {
        launch,
        pr: stored.pr,
    })
}

/// The error of `row`'s empty `column`, which the start it plans needs.
fn missing(row: &Row<'_>, column: &str) -> rusqlite::Error {
    let index = row.as_ref().column_index(column).unwrap_or_default();
    rusqlite::Error::InvalidColumnType(index, column.into(), rusqlite::types::Type::Null)
}

/// What a launch's row keeps as JSON of the start it plans.
#[derive(Serialize, Deserialize)]
struct StoredStart {
    launch: StoredLaunch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pr: Option<ItemInput>,
}

/// What a launch's row keeps as JSON of its [`QueuedLaunch`].
#[derive(Serialize, Deserialize)]
struct StoredLaunch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(default, skip_serializing_if = "ModelChoice::is_empty")]
    model: ModelChoice,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fork: Option<AgentSessionId>,
    /// The URL of the issue it works on, whose title and labels the row's
    /// columns hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    issue: Option<ItemRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run: Option<ItemRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    env: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    now: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    here: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    replace: bool,
}

#[cfg(test)]
mod tests;
