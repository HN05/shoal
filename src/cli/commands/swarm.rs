//! Swarms: one task attempted in several workspaces, of which one is kept.
use std::{collections::HashSet, ffi::OsString};

use anyhow::{Context as _, Result, ensure};

use super::workspaces::{self, AddPlan, AgentLaunch, Creation};
use crate::{
    agent::Agent,
    cli::{
        SwarmCommand, agents, client,
        context::Context,
        herdr,
        ui::{self, Fallback},
    },
    forge::IssueInput,
    git,
    model::Workspace,
    protocol::ConfigTarget,
    validate,
};

pub(super) async fn run(ctx: &Context, command: SwarmCommand) -> Result<i32> {
    match command {
        SwarmCommand::Add {
            target,
            task,
            repository,
            issue,
            agents,
            count,
            base,
            git_profile,
            args,
        } => {
            let input = SwarmInput {
                target,
                repository,
                task,
                issue,
                agents,
                count: count.into(),
                base,
                git_profile,
            };
            add(ctx, input, args).await
        }
        SwarmCommand::Pick {
            workspace,
            confirmation,
            keep_branch,
            delete_branch,
        } => {
            let removal = Removal {
                yes: confirmation.yes,
                keep_branch,
                delete_branch,
            };
            pick(ctx, workspace, removal).await
        }
    }
}

/// What `shoal swarm add` was asked to create.
struct SwarmInput {
    /// A registered repository, or an issue number or URL.
    target: Option<String>,
    repository: Option<String>,
    task: Option<String>,
    issue: Option<String>,
    agents: Vec<Agent>,
    count: usize,
    base: Option<String>,
    git_profile: Option<String>,
}

/// Create one workspace per attempt from the same base and prompt, each on a
/// branch named after the task and the attempt's agent.
async fn add(ctx: &Context, input: SwarmInput, args: Vec<OsString>) -> Result<i32> {
    let (repository, issue) = split_target(input.target, input.repository, input.issue)?;
    let target = workspaces::resolve_add_target(
        ctx,
        repository,
        issue.as_deref(),
        &AgentLaunch::IssueDefault(None),
    )
    .await?;
    let settings = client::settings(
        &ctx.paths,
        ConfigTarget::Repository(target.selector.clone()),
    )
    .await?;
    let agents = if input.agents.is_empty() {
        let agent =
            agents::default_agent(ctx, ConfigTarget::Repository(target.selector.clone()), None)
                .await?;
        vec![agent]
    } else {
        for agent in &input.agents {
            agents::ensure_installed(agent, &settings.commands)?;
        }
        input.agents.into_iter().map(Some).collect()
    };
    let attempts: Vec<_> = agents
        .iter()
        .flat_map(|agent| std::iter::repeat_n(agent.clone(), input.count))
        .collect();
    ensure!(
        attempts.len() >= 2,
        "a swarm needs at least two attempts; pass several agents with --agents, or --count"
    );
    let tabs = herdr::opens_tabs(ctx, &settings);
    ensure!(
        tabs || attempts.iter().flatten().all(runs_detached),
        "CLI agents in a swarm each need a Herdr tab; run shoal swarm inside Herdr, or use happy-claude or happy-codex"
    );
    let issue = match issue {
        Some(input) => Some(super::issues::load(target.repository(ctx).await?, &input).await?),
        None => None,
    };
    let task = match (input.task, &issue) {
        (Some(task), _) => task,
        (None, Some(issue)) => issue.branch_name(),
        (None, None) => ui::input(ctx, "Task name")?,
    };
    git::check_branch_name(None, &task).await?;
    let taken = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .map(|workspace| workspace.name)
        .collect();
    let branches = attempt_branches(&task, &attempts, taken);
    let repo = target.repository(ctx).await?;
    for (agent, branch) in attempts.into_iter().zip(branches) {
        let tab_label = issue
            .as_ref()
            .map(|issue| attempt_label(issue.tab_label(repo), agent.as_ref()));
        let creation = Creation {
            path: None,
            branch: Some(branch),
            existing: None,
            base: input.base.clone(),
            git_profile: input.git_profile.clone(),
            pr: None,
            swarm: Some(task.clone()),
        };
        let mut plan = AddPlan::attempt(
            target.selector.clone(),
            creation,
            tab_label,
            issue.as_ref().map(|issue| issue.url.clone()),
            agent,
            args.clone(),
        );
        if tabs && herdr::handoff(ctx, &mut plan, issue.as_ref(), false).await? {
            continue;
        }
        let code = workspaces::execute_add(ctx, plan).await?;
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}

/// The branch choice and confirmation for each removed workspace, as for `rm`.
struct Removal {
    yes: bool,
    keep_branch: bool,
    delete_branch: bool,
}

/// Keep the workspace and remove the swarm's other workspaces through the
/// normal removal path. Their linked PRs are reported, not closed.
async fn pick(ctx: &Context, workspace: Option<String>, removal: Removal) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let kept = client::inspect(&ctx.paths, workspace).await?.workspace;
    let swarm = kept
        .swarm
        .with_context(|| format!("workspace {} is not in a swarm", kept.name))?;
    let mut prs = Vec::new();
    let mut removed = serde_json::Map::new();
    for other in &swarm.workspaces {
        let linked = client::inspect(&ctx.paths, other.id.clone()).await?;
        prs.extend(linked.workspace.links.prs);
        let result = workspaces::remove_workspace(
            ctx,
            other.id.clone(),
            removal.yes,
            removal.keep_branch,
            removal.delete_branch,
        )
        .await?;
        if !ctx.json {
            println!("{}: {}", other.name, result.message());
        }
        removed.insert(other.name.clone(), serde_json::to_value(result)?);
    }
    let mut text = format!("Picked {}", kept.name);
    if !prs.is_empty() {
        text.push_str(&format!("\nPRs of removed workspaces: {}", prs.join(", ")));
    }
    ctx.emit(
        &text,
        serde_json::json!({"workspace": kept.name, "removed": removed, "prs": prs}),
    )?;
    Ok(0)
}

/// The swarm's task and other workspaces, when the workspace is in one.
pub(super) fn render(workspace: &Workspace) {
    if let Some(swarm) = &workspace.swarm {
        println!("Swarm: {}", swarm.task);
        let others: Vec<_> = swarm.workspaces.iter().map(super::base::describe).collect();
        println!("Swarm workspaces: {}", others.join(", "));
    }
}

/// The repository and issue a swarm's target names: an issue number or URL
/// is the issue, anything else the repository.
fn split_target(
    target: Option<String>,
    repository: Option<String>,
    issue: Option<String>,
) -> Result<(Option<String>, Option<String>)> {
    let Some(target) = target else {
        return Ok((repository, issue));
    };
    if issue.is_none() && IssueInput::parse(&target) != IssueInput::Invalid {
        return Ok((repository, Some(target)));
    }
    ensure!(
        repository.is_none(),
        "a positional repository cannot be combined with --repo"
    );
    Ok((Some(target), issue))
}

/// Happy sessions run detached; other agents run in the terminal they start in.
fn runs_detached(agent: &Agent) -> bool {
    matches!(agent, Agent::Happy(_))
}

fn attempt_label(label: String, agent: Option<&Agent>) -> String {
    match agent {
        Some(agent) => format!("{label} {}", String::from(agent.clone())),
        None => label,
    }
}

/// Branches named after the task and each attempt's agent, suffixed with
/// `-2`, `-3`, etc. until their workspace names are unused. The task is
/// shortened so that the names stay distinct within the name length limit.
fn attempt_branches(
    task: &str,
    attempts: &[Option<Agent>],
    mut taken: HashSet<String>,
) -> Vec<String> {
    let labels: Vec<_> = attempts
        .iter()
        .map(|agent| agent.clone().map_or_else(|| "attempt".into(), String::from))
        .collect();
    let longest = labels.iter().map(String::len).max().unwrap_or(0);
    // Room for "-<label>-<n>" with up to three digits.
    let room = validate::MAX_NAME_LEN.saturating_sub(longest + 5);
    let task: String = task.chars().take(room).collect();
    let task = task.trim_end_matches(['-', '_', '/', '.']);
    labels
        .into_iter()
        .map(|label| {
            let base = format!("{task}-{label}");
            let branch = std::iter::once(base.clone())
                .chain((2..).map(|n| format!("{base}-{n}")))
                .find(|branch| !taken.contains(&validate::workspace_name(branch)))
                .expect("an unused suffix exists");
            taken.insert(validate::workspace_name(&branch));
            branch
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::BuiltinAgent;

    #[test]
    fn attempts_name_their_agent_and_avoid_used_workspace_names() {
        let attempts = [
            Some(Agent::Codex),
            Some(Agent::Claude),
            Some(Agent::Claude),
            Some(Agent::Happy(BuiltinAgent::Codex)),
        ];
        let taken = HashSet::from(["fix-login-codex".to_owned()]);
        assert_eq!(
            attempt_branches("fix/login", &attempts, taken),
            [
                "fix/login-codex-2",
                "fix/login-claude",
                "fix/login-claude-2",
                "fix/login-happy-codex"
            ]
        );
        assert_eq!(
            attempt_branches("fix", &[None, None], HashSet::new()),
            ["fix-attempt", "fix-attempt-2"]
        );
    }

    #[test]
    fn long_tasks_keep_attempt_names_distinct() {
        let task = format!("issue-563-{}", "word-".repeat(20));
        let branches = attempt_branches(
            &task,
            &[Some(Agent::Claude), Some(Agent::Claude)],
            HashSet::new(),
        );
        let names: HashSet<_> = branches
            .iter()
            .map(|branch| validate::workspace_name(branch))
            .collect();
        assert_eq!(names.len(), 2);
        assert!(
            branches
                .iter()
                .all(|branch| branch.len() <= validate::MAX_NAME_LEN)
        );
        assert!(branches[0].ends_with("-claude"), "{}", branches[0]);
        assert!(branches[1].ends_with("-claude-2"), "{}", branches[1]);
    }

    #[test]
    fn issue_numbers_and_links_are_issues_and_other_targets_repositories() {
        let split = |target: &str, repository: Option<&str>| {
            split_target(Some(target.into()), repository.map(Into::into), None).unwrap()
        };
        assert_eq!(
            split("563", Some("shoal")),
            (Some("shoal".into()), Some("563".into()))
        );
        let url = "https://forge.example/team/repo/issues/7";
        assert_eq!(split(url, None), (None, Some(url.into())));
        assert_eq!(split("shoal", None), (Some("shoal".into()), None));
        assert!(split_target(Some("shoal".into()), Some("other".into()), None).is_err());
    }
}
