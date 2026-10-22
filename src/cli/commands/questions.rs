//! `shoal ask` records a question for the user, `shoal questions` lists them
//! and `shoal answer` sends the user's answer to the asking agents.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        installed::{Executable, arguments, replace_option},
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::questions::{Ask, Listing, Question},
    protocol::Method,
};

pub(super) async fn ask(
    ctx: &Context,
    workspace: Option<String>,
    ask: Ask,
    wait: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let question: Question = request(
        &ctx.paths,
        Method::AskQuestion {
            workspace: workspace.clone(),
            ask,
        },
    )
    .await?;
    if !wait && question.is_open() {
        ctx.emit(
            &format!(
                "Question {} recorded; the answer arrives as a message.",
                question.id
            ),
            &question,
        )?;
        return Ok(0);
    }
    // The workspace ID stays valid if the workspace is renamed meanwhile.
    let workspace = question.workspace_id.clone();
    let question = if question.is_open() {
        await_answer(ctx, &workspace, &question, false).await?
    } else {
        question
    };
    print_answer(ctx, workspace, &question).await?;
    Ok(0)
}

/// Continue an earlier `ask --wait` for question `id` of `workspace`, which
/// asks `ask`.
pub(super) async fn resume(ctx: &Context, workspace: String, id: i64, ask: Ask) -> Result<i32> {
    let question = await_answer(ctx, &workspace, Asked { id, ask }, true).await?;
    print_answer(ctx, workspace, &question).await?;
    Ok(0)
}

/// Printed here, the answer's message need not reach the agent again.
async fn print_answer(ctx: &Context, workspace: String, question: &Question) -> Result<()> {
    ctx.emit(question.answer.as_deref().unwrap_or_default(), question)?;
    std::io::Write::flush(&mut std::io::stdout())?;
    let Some(id) = question.message_id else {
        return Ok(());
    };
    request::<()>(
        &ctx.paths,
        Method::MarkAgentMessagesDelivered {
            workspace,
            ids: vec![id],
        },
    )
    .await
}

/// The question a wait follows: its number and what it asks, since a later
/// question may reuse the number.
struct Asked {
    id: i64,
    ask: Ask,
}

impl From<&Question> for Asked {
    fn from(question: &Question) -> Self {
        Self {
            id: question.id,
            ask: question.ask.clone(),
        }
    }
}

/// Wait across daemon restarts; after an upgrade, the installed release
/// resumes the wait for the same question.
async fn await_answer(
    ctx: &Context,
    workspace: &str,
    asked: impl Into<Asked>,
    restarted: bool,
) -> Result<Question> {
    let asked = asked.into();
    let executable = Executable::current();
    let method = || Method::AwaitAnswer {
        workspace: workspace.to_owned(),
        id: asked.id,
        ask: asked.ask.clone(),
    };
    match client::await_reply(&ctx.paths, method, restarted).await {
        Ok(body) => Question::try_from(body),
        Err(error) => Err(executable.resume_or(error, || resume_arguments(workspace, asked.id))),
    }
}

/// This command's arguments, which state the question, resuming the wait for
/// its number `id` in `workspace` without asking again.
fn resume_arguments(workspace: &str, id: i64) -> Vec<OsString> {
    let arguments = replace_option(arguments(), "--workspace", workspace);
    replace_option(arguments, "--resume", &id.to_string())
}

pub(super) async fn list(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let listing = request::<Listing>(&ctx.paths, Method::ListQuestions { workspace }).await?;
    ctx.show(&listing, |listing| {
        let palette = Palette::stdout(ctx.json);
        for question in &listing.questions {
            println!("{}", render(question, palette));
        }
        if listing.omitted > 0 {
            println!("{} older questions not shown", listing.omitted);
        } else if listing.questions.is_empty() {
            println!("No open questions");
        }
    })?;
    Ok(0)
}

pub(super) async fn answer(ctx: &Context, id: Option<i64>, answer: Option<String>) -> Result<i32> {
    let (id, answer, shown) = match (id, answer) {
        (Some(id), Some(answer)) => (id, answer, None),
        // Workspace processes answer only the questions of the workspaces
        // below theirs, by number.
        _ if crate::env::inherits_scope(&ctx.paths.state) => {
            anyhow::bail!("workspace processes answer with shoal answer <number> <answer>")
        }
        (id, _) => {
            let (question, answer) = prompt_answer(ctx, id).await?;
            (question.id, answer, Some(question))
        }
    };
    let question =
        request::<Question>(&ctx.paths, Method::AnswerQuestion { id, answer, shown }).await?;
    ctx.emit(&answered(&question), &question)?;
    Ok(0)
}

/// Names the question by its text too: a stale notification may name a
/// number that a later question reuses.
fn answered(question: &Question) -> String {
    format!(
        "Answered question {} from {} ({}): {}",
        question.id,
        question.workspace,
        question.ask.question,
        question.answer.as_deref().unwrap_or_default()
    )
}

/// Show the open question `id`, or one the user picks, and read the answer.
async fn prompt_answer(ctx: &Context, id: Option<i64>) -> Result<(Question, String)> {
    let question = select_open(ctx, id).await?;
    eprintln!("{}", render(&question, Palette::stderr(ctx.json)));
    let answer = ui::input(ctx, "Answer")?;
    Ok((question, answer))
}

/// The open question `id`, or one the user picks.
async fn select_open(ctx: &Context, id: Option<i64>) -> Result<Question> {
    let Some(id) = id else {
        return pick_open(ctx).await;
    };
    let question = request::<Question>(&ctx.paths, Method::ShowQuestion { id }).await?;
    question.ensure_open()?;
    Ok(question)
}

/// Choose among the newest open questions.
async fn pick_open(ctx: &Context) -> Result<Question> {
    let questions = request::<Listing>(&ctx.paths, Method::ListQuestions { workspace: None })
        .await?
        .questions;
    ensure!(!questions.is_empty(), "no open questions");
    let entries = questions
        .iter()
        .map(|question| (question.id.to_string(), summary(question)))
        .collect();
    let id: i64 = ui::pick(ctx, "Question> ", entries)?.parse()?;
    questions
        .into_iter()
        .find(|question| question.id == id)
        .context("picker returned an unknown question")
}

/// One line with what the user needs to choose a question: its workspace,
/// the decision, the recommendation and the choices.
pub(in crate::cli) fn summary(question: &Question) -> String {
    let mut line = format!(
        "{}  {}: {}  Recommended: {}",
        question.id, question.workspace, question.ask.question, question.ask.recommendation
    );
    if !question.ask.choices.is_empty() {
        let choices: Vec<_> = (1..)
            .zip(&question.ask.choices)
            .map(|(number, choice)| format!("{number}. {choice}"))
            .collect();
        line.push_str(&format!("  Choices: {}", choices.join(", ")));
    }
    line
}

fn render(question: &Question, palette: Palette) -> String {
    let status = if question.closed_at.is_some() {
        "closed"
    } else if question.is_open() {
        "open"
    } else {
        "answered"
    };
    let mut lines = vec![format!(
        "{} {} {}: {}",
        palette.paint(Style::Heading, question.id),
        palette.paint(Style::Muted, format_args!("[{status}]")),
        question.workspace,
        question.ask.question
    )];
    lines.push(format!("  Recommended: {}", question.ask.recommendation));
    if let Some(reason) = &question.ask.reason {
        lines.push(format!("  Reason: {reason}"));
    }
    for (number, choice) in question.ask.choices.iter().enumerate() {
        lines.push(format!("  {}. {choice}", number + 1));
    }
    if let Some(answer) = &question.answer {
        lines.push(format!("  Answer: {answer}"));
    }
    lines.join("\n")
}
