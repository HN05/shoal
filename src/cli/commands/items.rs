//! `shoal pr`, `shoal pr ci`, `shoal ci` and `shoal issue`: ask the daemon to
//! list, open or change items of the workspace's repository, or start CI runs
//! there, and report them as the forge now does.
use std::path::Path;

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        AttachArgs, BodyArgs, CiCommand, EditArgs, IssueCommand, ItemSelector, ListArgs, PrCommand,
        WorkflowCommand, client, context::Context, ui,
    },
    daemon::links::proposals::{OpenOutcome, Proposed},
    forge::{
        ForgeRepo,
        capability::Capability,
        ci::{CiActed, CiAction, run::StartedRun},
        items::{
            action::{Action, Edit, ThreadChange},
            attach::{self, Attachment},
            create::{NewIssue, PullOptions},
            item::{Item, ItemState, Opened},
            link::{ItemKind, Selection},
            list::Search,
        },
    },
    git,
    model::{Selected, Workspace},
    protocol::Method,
};

pub(super) async fn pr(ctx: &Context, command: PrCommand) -> Result<i32> {
    let (item, action, outcome) = match command {
        PrCommand::Ci { command } => return ci(ctx, command).await,
        PrCommand::List(args) => return list(ctx, ItemKind::Pr, args).await,
        PrCommand::Open {
            workspace,
            title,
            body,
            files,
            base,
            draft,
            no_draft,
            labels,
            reviewers,
        } => {
            let (body, attachments) = read_content(body, files)?;
            let options = PullOptions {
                title,
                body,
                base,
                draft: (draft || no_draft).then_some(draft),
                labels,
                reviewers,
                attachments,
            };
            return open_pr(ctx, workspace, options).await;
        }
        PrCommand::Edit {
            item,
            edit,
            base,
            add_reviewers,
            remove_reviewers,
            draft,
            no_draft,
        } => {
            let edit = Edit {
                base,
                add_reviewers,
                remove_reviewers,
                draft: (draft || no_draft).then_some(draft),
                ..edit_fields(edit)?
            };
            (item, Action::Edit(edit), "PR updated")
        }
        PrCommand::Comment { item, body, files } => {
            (item, comment(body, files)?, "Commented on PR")
        }
        PrCommand::Reply {
            comment,
            item,
            body,
        } => {
            let reply = read_body(body)?.context("a reply needs --body or --body-file")?;
            (
                item,
                thread(comment, Some(reply), None),
                "Replied in thread",
            )
        }
        PrCommand::Resolve {
            comment,
            item,
            body,
            confirmation,
        } => {
            let reply = read_body(body)?;
            if reply.is_none() && !confirmation.yes {
                confirm_unanswered(ctx, &item, comment).await?;
            }
            (item, thread(comment, reply, Some(true)), "Thread resolved")
        }
        PrCommand::Unresolve { comment, item } => (
            item,
            thread(comment, None, Some(false)),
            "Thread unresolved",
        ),
        PrCommand::Close { item } => (item, Action::Close, "PR closed"),
        PrCommand::Reopen { item } => (item, Action::Reopen, "PR reopened"),
        PrCommand::Merge {
            item,
            method,
            delete_branch,
        } => (
            item,
            Action::Merge {
                method,
                delete_branch,
            },
            "PR merged",
        ),
    };
    act(ctx, item, ItemKind::Pr, action, outcome).await
}

pub(super) async fn ci(ctx: &Context, command: CiCommand) -> Result<i32> {
    let (action, checks) = match command {
        CiCommand::Rerun { checks } => (CiAction::Rerun, checks),
        CiCommand::Cancel { checks } => (CiAction::Cancel, checks),
    };
    let workspace =
        ui::select_workspace(ctx, checks.item.workspace, ui::Fallback::CurrentDirectory).await?;
    let acted: CiActed = client::request(
        &ctx.paths,
        Method::CiAction {
            workspace,
            item: checks.item.item,
            action,
            checks: checks.checks,
        },
    )
    .await?;
    let outcome = match action {
        CiAction::Rerun => "Rerun started",
        CiAction::Cancel => "Cancelled",
    };
    ctx.emit(
        &format!("{outcome} for {}: {}", acted.checks.join(", "), acted.url),
        &acted,
    )?;
    Ok(0)
}

pub(super) async fn workflow(ctx: &Context, command: WorkflowCommand) -> Result<i32> {
    let WorkflowCommand::Run {
        workflow,
        workspace,
        branch,
        inputs,
    } = command;
    let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory).await?;
    let started: StartedRun = client::request(
        &ctx.paths,
        Method::StartCiRun {
            workspace,
            workflow,
            branch,
            inputs: inputs.into_iter().collect(),
        },
    )
    .await?;
    let run = match &started.workflow {
        Some(workflow) => format!("{workflow} on {}", started.branch),
        None => started.branch.clone(),
    };
    let text = match &started.url {
        Some(url) => format!("Run started for {run}: {url}"),
        None => format!("Run started for {run}"),
    };
    ctx.emit(&text, &started)?;
    Ok(0)
}

pub(super) async fn issue(ctx: &Context, command: IssueCommand) -> Result<i32> {
    let (item, action, outcome) = match command {
        IssueCommand::List(args) => return list(ctx, ItemKind::Issue, args).await,
        IssueCommand::Open {
            workspace,
            repo,
            title,
            body,
            files,
            labels,
            link,
        } => {
            let (body, attachments) = read_content(body, files)?;
            let issue = NewIssue {
                title,
                body: body.unwrap_or_default(),
                labels,
                attachments,
            };
            return open_issue(ctx, workspace, issue, repo, link).await;
        }
        IssueCommand::Edit { item, edit } => {
            (item, Action::Edit(edit_fields(edit)?), "Issue updated")
        }
        IssueCommand::Comment { item, body, files } => {
            (item, comment(body, files)?, "Commented on issue")
        }
        IssueCommand::Close { item } => (item, Action::Close, "Issue closed"),
        IssueCommand::Reopen { item } => (item, Action::Reopen, "Issue reopened"),
    };
    act(ctx, item, ItemKind::Issue, action, outcome).await
}

async fn list(ctx: &Context, kind: ItemKind, args: ListArgs) -> Result<i32> {
    let query = args.query.join(" ");
    let search = Search {
        state: args.state,
        labels: args.labels,
        author: args.author,
        text: (!query.is_empty()).then_some(query),
        limit: args.limit.into(),
    };
    // Refuse a bad filter before the workspace picker asks for anything.
    search.check()?;
    let workspace =
        ui::select_workspace(ctx, args.workspace, ui::Fallback::CurrentDirectory).await?;
    let method = Method::ListItems {
        workspace,
        kind,
        search,
    };
    let items = ctx
        .progress("Searching", client::pages::<Item>(&ctx.paths, method))
        .await?;
    ctx.show(&items, |items| render_list(kind, items))?;
    Ok(0)
}

/// One line per item: its number, state, title and labels.
fn render_list(kind: ItemKind, items: &[Item]) {
    if items.is_empty() {
        match kind {
            ItemKind::Issue => outln!("No issues"),
            ItemKind::Pr => outln!("No PRs"),
        }
        return;
    }
    let width = items
        .iter()
        .map(|item| item.number.to_string().len())
        .max()
        .unwrap_or(0);
    for item in items {
        let state = match &item.pr {
            Some(pr) if pr.draft && item.state == ItemState::Open => "draft",
            _ => item.state.as_str(),
        };
        let mut line = format!("#{:<width$}  {state:<6}  {}", item.number, item.title);
        if !item.labels.is_empty() {
            line += &format!("  [{}]", item.labels.join(", "));
        }
        outln!("{line}");
    }
}

async fn act(
    ctx: &Context,
    selector: ItemSelector,
    kind: ItemKind,
    action: Action,
    outcome: &str,
) -> Result<i32> {
    action.validate(kind)?;
    let workspace =
        ui::select_workspace(ctx, selector.workspace, ui::Fallback::CurrentDirectory).await?;
    let item: Item = client::request(
        &ctx.paths,
        Method::ItemAction {
            workspace,
            kind,
            item: selector.item,
            action,
        },
    )
    .await?;
    ctx.emit(&format!("{outcome}: {}", item.url), &item)?;
    Ok(0)
}

/// Verify the worktree before anything leaves it, push without forcing, then
/// let the daemon find or open the PR and link it.
async fn open_pr(ctx: &Context, workspace: Option<String>, options: PullOptions) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory).await?;
    let workspace: Workspace = client::request(
        &ctx.paths,
        Method::VerifyWorkspace {
            workspace,
            on_branch: true,
        },
    )
    .await?;
    push(ctx, &workspace).await?;
    let outcome: OpenOutcome = client::request(
        &ctx.paths,
        Method::OpenPull {
            workspace: workspace.id,
            options,
        },
    )
    .await?;
    let opened = match outcome {
        OpenOutcome::Opened(opened) => opened,
        OpenOutcome::Proposed(proposed) => return report_proposed(ctx, "PR", &proposed),
    };
    print_warnings(&opened);
    let outcome = if opened.created {
        "PR opened"
    } else {
        "Open PR found"
    };
    let message = if opened.linked {
        format!(
            "{outcome} and linked: {}\nRun shoal watch pr for updates and shoal done once the assignment is finished.",
            opened.item.url
        )
    } else {
        format!("{outcome}: {}", opened.item.url)
    };
    ctx.emit(&message, &opened)?;
    Ok(0)
}

/// Push to the branch's push remote as a tracked command under the agent
/// account, with the user's Git configuration and hooks.
async fn push(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let remote = git::push_remote(&workspace.path, &workspace.branch).await?;
    let code = crate::execution::publish(
        &ctx.paths,
        workspace.id.clone(),
        git::push_command(&remote, &workspace.branch),
    )
    .await?;
    ensure!(
        code == 0,
        "git push of {} to {remote} failed (exit {code})",
        workspace.branch
    );
    Ok(())
}

async fn open_issue(
    ctx: &Context,
    workspace: Option<String>,
    issue: NewIssue,
    repository: Option<String>,
    link: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory).await?;
    let outcome: OpenOutcome = client::request(
        &ctx.paths,
        Method::OpenIssue {
            workspace,
            issue,
            repository,
            link,
        },
    )
    .await?;
    let opened = match outcome {
        OpenOutcome::Opened(opened) => opened,
        OpenOutcome::Proposed(proposed) => return report_proposed(ctx, "Issue", &proposed),
    };
    print_warnings(&opened);
    let linked = if opened.linked { " and linked" } else { "" };
    ctx.emit(
        &format!("Issue opened{linked}: {}", opened.item.url),
        &opened,
    )?;
    Ok(0)
}

/// The repository requires approval: the agent continues its work and
/// hears the decision as a message.
fn report_proposed(ctx: &Context, kind: &str, proposed: &Proposed) -> Result<i32> {
    ctx.emit(
        &format!(
            "{kind} proposed for approval {}: the user decides whether Shoal opens it, and the decision arrives as a message.",
            proposed.proposed.id
        ),
        proposed,
    )?;
    Ok(0)
}

fn print_warnings(opened: &Opened) {
    for warning in &opened.warnings {
        eprintln!("{warning}");
    }
}

fn edit_fields(edit: EditArgs) -> Result<Edit> {
    let (body, attachments) = read_content(edit.body, edit.files)?;
    Ok(Edit {
        title: edit.title,
        body,
        add_labels: edit.add_labels,
        remove_labels: edit.remove_labels,
        attachments,
        ..Edit::default()
    })
}

fn comment(body: BodyArgs, files: AttachArgs) -> Result<Action> {
    let (body, attachments) = read_content(body, files)?;
    ensure!(
        body.is_some() || !attachments.is_empty(),
        "a comment needs --body, --body-file or --attach"
    );
    Ok(Action::Comment {
        body: body.unwrap_or_default(),
        attachments,
    })
}

fn thread(comment: u64, reply: Option<String>, resolve: Option<bool>) -> Action {
    Action::Thread(ThreadChange {
        comment,
        reply,
        resolve,
    })
}

/// Ask before resolving a thread without a reply, which leaves the reviewer
/// no reason. Without exactly one selected PR the action itself fails, so
/// there is nothing to ask.
async fn confirm_unanswered(ctx: &Context, selector: &ItemSelector, comment: u64) -> Result<()> {
    let workspace = ui::select_workspace(
        ctx,
        selector.workspace.clone(),
        ui::Fallback::CurrentDirectory,
    )
    .await?;
    let selection = Selection {
        kind: Some(ItemKind::Pr),
        input: selector.item.clone(),
    };
    let selected: Selected = client::request(
        &ctx.paths,
        Method::SelectItems {
            workspace,
            selection,
        },
    )
    .await?;
    let [item] = selected.items.as_slice() else {
        return Ok(());
    };
    let forge = ForgeRepo::resolve_pull_url(&item.url, &ctx.forge_hosts()?).await?;
    forge.require(Capability::ResolveThreads)?;
    let (number, _) = forge.pull(&item.url)?;
    let thread = ctx
        .progress(
            "Reading thread",
            forge.thread(&ctx.forge_account()?, number, comment),
        )
        .await?;
    if thread.replies == 0 {
        ensure!(
            ui::confirm(
                ctx,
                &format!("Thread {} has no reply.", thread.first),
                "--body-file with the reason, or --yes"
            )?,
            "thread resolution canceled"
        );
    }
    Ok(())
}

/// The body and the files to attach, checked before anything is sent, with
/// the body's references to them pointing at their paths.
fn read_content(body: BodyArgs, files: AttachArgs) -> Result<(Option<String>, Vec<Attachment>)> {
    // Only attachments resolve paths, which a deleted working directory cannot.
    if files.attach.is_empty() {
        return Ok((read_body(body)?, Vec::new()));
    }
    let cwd = std::env::current_dir().context("read the working directory")?;
    let attachments: Vec<_> = files
        .attach
        .iter()
        .map(|arg| Attachment::parse(arg, &cwd))
        .collect();
    attach::check_all(&attachments)?;
    let body_dir = body
        .body_file
        .as_ref()
        .and_then(|path| cwd.join(path).parent().map(Path::to_path_buf));
    let text = read_body(body)?.map(|text| {
        let dirs: Vec<&Path> = body_dir.as_deref().into_iter().chain([&*cwd]).collect();
        attach::resolve_references(&text, &attachments, &dirs)
    });
    Ok((text, attachments))
}

pub(super) fn read_body(body: BodyArgs) -> Result<Option<String>> {
    match body.body_file {
        Some(path) => std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))
            .map(Some),
        None => Ok(body.body),
    }
}
