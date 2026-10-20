//! `shoal ask` records a question for the user, `shoal questions` lists them
//! and `shoal answer` sends the user's answer to the asking agents.
use std::ffi::OsString;

use anyhow::{Result, ensure};

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
        await_answer(ctx, &workspace, question.id, false).await?
    } else {
        question
    };
    print_answer(ctx, workspace, &question).await?;
    Ok(0)
}

/// Continue an earlier `ask --wait` for question `id` of `workspace`.
pub(super) async fn resume(ctx: &Context, workspace: String, id: i64) -> Result<i32> {
    let question = await_answer(ctx, &workspace, id, true).await?;
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

/// Wait across daemon restarts; after an upgrade, the installed release
/// resumes the wait for the same question.
async fn await_answer(
    ctx: &Context,
    workspace: &str,
    id: i64,
    restarted: bool,
) -> Result<Question> {
    let executable = Executable::current();
    let method = || Method::AwaitAnswer {
        workspace: workspace.to_owned(),
        id,
    };
    match client::await_reply(&ctx.paths, method, restarted).await {
        Ok(body) => Question::try_from(body),
        Err(error) => Err(executable.resume_or(error, || resume_arguments(workspace, id))),
    }
}

/// This command's arguments, resuming the wait for question `id` of `workspace`
/// without asking again.
fn resume_arguments(workspace: &str, id: i64) -> Vec<OsString> {
    let arguments = replace_option(arguments(), "--workspace", workspace);
    replace_option(arguments, "--resume", &id.to_string())
}

pub(super) async fn list(ctx: &Context, workspace: Option<String>, all: bool) -> Result<i32> {
    let listing = request::<Listing>(&ctx.paths, Method::ListQuestions { workspace, all }).await?;
    ctx.show(&listing, |listing| {
        let palette = Palette::stdout(ctx.json);
        for question in &listing.questions {
            println!("{}", render(question, palette));
        }
        if listing.omitted > 0 {
            println!("{} older questions not shown", listing.omitted);
        } else if listing.questions.is_empty() {
            println!("No {}questions", if all { "" } else { "open " });
        }
    })?;
    Ok(0)
}

pub(super) async fn answer(ctx: &Context, id: Option<i64>, answer: Option<String>) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot answer questions; ask the user with shoal ask"
    );
    let (id, answer) = match (id, answer) {
        (Some(id), Some(answer)) => (id, answer),
        (id, _) => prompt_answer(ctx, id).await?,
    };
    let question = request::<Question>(&ctx.paths, Method::AnswerQuestion { id, answer }).await?;
    ctx.emit(
        &format!(
            "Answer sent to workspace {} for question {}.",
            question.workspace, question.id
        ),
        &question,
    )?;
    Ok(0)
}

/// Show the open question `id`, or one the user picks, and read the answer.
async fn prompt_answer(ctx: &Context, id: Option<i64>) -> Result<(i64, String)> {
    let question = select_open(ctx, id).await?;
    eprintln!("{}", render(&question, Palette::stderr(ctx.json)));
    Ok((question.id, ui::input(ctx, "Answer")?))
}

/// The open question `id`, or one the user picks.
async fn select_open(ctx: &Context, id: Option<i64>) -> Result<Question> {
    let id = match id {
        Some(id) => id,
        None => pick_open(ctx).await?,
    };
    let question = request::<Question>(&ctx.paths, Method::ShowQuestion { id }).await?;
    ensure!(question.is_open(), "question {id} is already answered");
    Ok(question)
}

/// Choose among the newest open questions.
async fn pick_open(ctx: &Context) -> Result<i64> {
    let questions = request::<Listing>(
        &ctx.paths,
        Method::ListQuestions {
            workspace: None,
            all: false,
        },
    )
    .await?
    .questions;
    ensure!(!questions.is_empty(), "no open questions");
    let entries = questions
        .iter()
        .map(|question| {
            (
                question.id.to_string(),
                format!(
                    "{}  {}: {}",
                    question.id, question.workspace, question.ask.question
                ),
            )
        })
        .collect();
    Ok(ui::pick(ctx, "Question> ", entries)?.parse()?)
}

fn render(question: &Question, palette: Palette) -> String {
    let status = if question.is_open() {
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
