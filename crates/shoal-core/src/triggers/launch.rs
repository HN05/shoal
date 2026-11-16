//! What a trigger's firing launches: the keys of `shoal start`'s flags,
//! with `task` for the text after `--`. Each string is a template the
//! firing's input renders, then reads as its flag's value would be read.
use std::{collections::BTreeMap, str::FromStr};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use shoal_protocol::launches::{IssueChoice, Launch, SpaceChoice, StartTarget};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_schedule::priority::Priority;

use super::template::Template;
use crate::{
    agent::{Agent, ModelChoice},
    config::{modes::ModeName, permissions::PermissionsConfig},
    forge::items::{link::LinkKind, reference::ItemInput},
};

/// The launch keys of a `[triggers.NAME]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchKeys {
    /// The registered repository a global trigger starts in; a
    /// repository's trigger starts in its own.
    pub repository: Option<Template>,
    pub agent: Option<Template>,
    pub mode: Option<Template>,
    pub model: Option<Template>,
    pub effort: Option<Template>,
    pub branch: Option<Template>,
    pub base: Option<Template>,
    pub issue: Option<Template>,
    pub pr: Option<Template>,
    pub name: Option<Template>,
    pub priority: Option<Template>,
    /// `--permission KEY=VALUE`, by key.
    pub permission: BTreeMap<String, Template>,
    /// The existing workspace, by ID, whose agent takes the task, as
    /// `--in` names it.
    pub in_space: Option<Template>,
    pub task: Option<Template>,
}

impl LaunchKeys {
    /// Refuse a value without lookups that its flag would refuse, and a
    /// combination no firing can launch, before any firing.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.issue.is_none() || self.pr.is_none(),
            "a launch starts on an issue or a PR, not both"
        );
        self.read(&Value::Null, true).map(drop)
    }

    /// The launch a firing whose input is `input` asks for, in the
    /// repository `repository` names; fails for a lookup that reaches no
    /// value or a value its flag refuses.
    pub fn launch(&self, input: &Value, repository: Option<String>) -> Result<Launch> {
        let mut launch = self.read(input, false)?;
        if launch.repository.is_none() {
            launch.repository = repository;
        }
        Ok(launch)
    }

    /// The launch these keys give for `input`; with `only_static`, the
    /// values that look nothing up, the rest left out.
    fn read(&self, input: &Value, only_static: bool) -> Result<Launch> {
        let render = |key: &str, template: &Option<Template>| -> Result<Option<String>> {
            match template {
                Some(template) if !only_static || template.is_static() => {
                    let text = template.render(input).with_context(|| key.to_owned())?;
                    ensure!(!text.trim().is_empty(), "{key} renders empty");
                    Ok(Some(text))
                }
                _ => Ok(None),
            }
        };
        let agent = (render("agent", &self.agent)?)
            .map(|text| Agent::from_str(&text).with_context(|| format!("agent {text}")))
            .transpose()?;
        let mode = (render("mode", &self.mode)?)
            .map(|text| ModeName::from_str(&text).with_context(|| format!("mode {text}")))
            .transpose()?;
        let priority = (render("priority", &self.priority)?)
            .map(|text| Priority::from_str(&text).with_context(|| format!("priority {text}")))
            .transpose()?;
        let name = (render("name", &self.name)?)
            .map(|text| SpaceName::from_str(&text).with_context(|| format!("name {text}")))
            .transpose()?;
        let issue = (render("issue", &self.issue)?)
            .map(|text| issue(&text))
            .transpose()?;
        let target = (render("pr", &self.pr)?)
            .map(|text| pull(&text))
            .transpose()?;
        let in_workspace = (render("in", &self.in_space)?)
            .map(|text| space(&text))
            .transpose()?;
        let mut assignments = Vec::new();
        for (key, value) in &self.permission {
            if let Some(value) = render(&format!("permission.{key}"), &Some(value.clone()))? {
                assignments.push(format!("{key}={value}"));
            }
        }
        let permissions = PermissionsConfig::assigned(&assignments)?;
        let model = ModelChoice {
            model: render("model", &self.model)?,
            effort: render("effort", &self.effort)?,
        };
        let branch = render("branch", &self.branch)?;
        if let Some(branch) = &branch {
            ensure!(
                !branch.contains(char::is_whitespace),
                "branch {branch:?} is not a branch name"
            );
        }
        Ok(Launch {
            target,
            repository: render("repository", &self.repository)?,
            branch,
            base: render("base", &self.base)?,
            issue: issue.map(IssueChoice::Named),
            in_workspace: in_workspace.map(SpaceChoice::Space),
            mode,
            agent,
            model,
            name,
            priority,
            permissions,
            task: render("task", &self.task)?.into_iter().collect(),
            ..Launch::default()
        })
    }
}

/// An issue as `--issue` reads it: its number or URL.
fn issue(text: &str) -> Result<ItemInput> {
    let issue = ItemInput::from_str(text).with_context(|| format!("issue {text}"))?;
    if let Some(kind) = issue.kind() {
        ensure!(
            kind == LinkKind::Issue,
            "issue {text} is a {} URL",
            kind.noun()
        );
    }
    Ok(issue)
}

/// A PR by its URL, as `shoal start` takes it.
fn pull(text: &str) -> Result<StartTarget> {
    match StartTarget::from_str(text).with_context(|| format!("pr {text}"))? {
        target @ StartTarget::Pull(_) => Ok(target),
        _ => bail!("pr {text} is not a PR URL"),
    }
}

/// A space by its ID, as `--in` names it.
fn space(text: &str) -> Result<SpaceSelector> {
    match SpaceSelector::parse(text).with_context(|| format!("in {text}"))? {
        selector @ SpaceSelector::Id(_) => Ok(selector),
        _ => bail!("in {text}: name the space by its ID, which stays the same"),
    }
}
