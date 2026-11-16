//! Triggers: a `[triggers.NAME]` table pairs a source of events with a
//! launch. A source produces firings, each with a JSON input document; the
//! filter, the templates, the key and the launch read only that document,
//! whatever its source.
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use shoal_protocol::triggers::SourceKind;
use shoal_states::states;

use crate::{config::span::Span, time::UnixTime};

pub mod filter;
pub mod input;
pub mod launch;
pub mod path;
pub mod template;
pub mod timer;

use filter::Condition;
use input::Input;
use launch::LaunchKeys;
use template::Template;
use timer::{ClockTime, Day, Timer};

/// How long a key keeps a repeat from running when the trigger sets none.
pub const KEY_WINDOW: Span = Span::from_secs(10 * 60);

/// One `[triggers.NAME]` table, whose keys config writes flat: the
/// source's beside the launch's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Written", into = "Written")]
pub struct Trigger {
    pub source: Source,
    pub launch: LaunchKeys,
    pub overlap: Overlap,
    pub filter: Vec<Condition>,
    /// What a firing is about, whose repeat within `key_window` is dropped.
    pub key: Option<Template>,
    pub key_window: Span,
}

/// Where a trigger's firings come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Timer(Timer),
}

impl Source {
    pub fn kind(&self) -> SourceKind {
        match self {
            Self::Timer(_) => SourceKind::Timer,
        }
    }

    /// When it fires next, counting from `counted_from`, for a trigger that
    /// last fired at `last_fired`.
    pub fn next(&self, counted_from: UnixTime, last_fired: Option<UnixTime>) -> UnixTime {
        match self {
            Self::Timer(timer) => timer.next(counted_from, last_fired),
        }
    }

    /// The input of the firing of the trigger `name` that `shoal trigger
    /// run` makes at `now`, after the firing at `previous`.
    pub fn manual_input(&self, name: &str, now: UnixTime, previous: UnixTime) -> Input {
        match self {
            Self::Timer(timer) => timer.input(name, now, now, Some(previous)).into(),
        }
    }
}

states!(
    #[derive(Default)]
    /// What a firing that would start a new space does while the space of
    /// the trigger's last run still has its agent running or queued.
    Overlap {
        /// It is dropped, and recorded as skipped.
        #[default]
        Skip => "skip",
        /// It launches once that agent stops.
        Queue => "queue",
        /// It launches at once.
        Parallel => "parallel",
    }
);

/// Every trigger of a layer, by name.
pub type Triggers = BTreeMap<String, Trigger>;

/// Refuse a trigger name `shoal trigger` could not take.
pub fn validate(triggers: &Triggers) -> Result<()> {
    for name in triggers.keys() {
        crate::validate::lowercase_name("trigger", name)
            .with_context(|| format!("invalid trigger name: {name}"))?;
    }
    Ok(())
}

/// A `[triggers.NAME]` table as config writes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Written {
    on: SourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    every: Option<Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    at: Option<ClockTimes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    days: Option<Vec<Day>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    catch_up: Option<Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repository: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effort: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    branch: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    issue: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pr: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<Template>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    permission: BTreeMap<String, Template>,
    #[serde(default, rename = "in", skip_serializing_if = "Option::is_none")]
    in_space: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task: Option<Template>,
    #[serde(default)]
    overlap: Overlap,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    filter: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<Template>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key_window: Option<Span>,
}

/// `at`: one time of day or a list of them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ClockTimes {
    One(ClockTime),
    Many(Vec<ClockTime>),
}

impl TryFrom<Written> for Trigger {
    type Error = anyhow::Error;

    fn try_from(written: Written) -> Result<Self> {
        let source = match written.on {
            SourceKind::Timer => Source::Timer(Timer::new(
                written.every,
                written.at.map(|at| match at {
                    ClockTimes::One(time) => vec![time],
                    ClockTimes::Many(times) => times,
                }),
                written.days,
                written.timezone,
                written.catch_up,
            )?),
        };
        let launch = LaunchKeys {
            repository: written.repository,
            agent: written.agent,
            mode: written.mode,
            model: written.model,
            effort: written.effort,
            branch: written.branch,
            base: written.base,
            issue: written.issue,
            pr: written.pr,
            name: written.name,
            priority: written.priority,
            permission: written.permission,
            in_space: written.in_space,
            task: written.task,
        };
        launch.validate()?;
        for condition in &written.filter {
            condition.validate()?;
        }
        Ok(Self {
            source,
            launch,
            overlap: written.overlap,
            filter: written.filter,
            key: written.key,
            key_window: written.key_window.unwrap_or(KEY_WINDOW),
        })
    }
}

impl From<Trigger> for Written {
    fn from(trigger: Trigger) -> Self {
        let Source::Timer(timer) = trigger.source;
        let (every, at, days) = match timer.schedule {
            timer::Schedule::Every(every) => (Some(every), None, None),
            timer::Schedule::At { times, days } => {
                let days = (days != Day::ALL).then_some(days);
                (None, Some(ClockTimes::Many(times)), days)
            }
        };
        let launch = trigger.launch;
        Self {
            on: SourceKind::Timer,
            every,
            at,
            days,
            timezone: timer.timezone,
            catch_up: Some(timer.catch_up),
            repository: launch.repository,
            agent: launch.agent,
            mode: launch.mode,
            model: launch.model,
            effort: launch.effort,
            branch: launch.branch,
            base: launch.base,
            issue: launch.issue,
            pr: launch.pr,
            name: launch.name,
            priority: launch.priority,
            permission: launch.permission,
            in_space: launch.in_space,
            task: launch.task,
            overlap: trigger.overlap,
            filter: trigger.filter,
            key: trigger.key,
            key_window: Some(trigger.key_window),
        }
    }
}

#[cfg(test)]
mod tests;
