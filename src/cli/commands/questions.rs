//! Record questions and let the user inspect, answer or withdraw them.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        args::coordination::QuestionsArgs,
        client::{self, request},
        commands::messages::hanging_indent,
        context::Context,
        installed::{Executable, arguments, replace_option},
        output::{Palette, Style},
        ui::{self, Cell, Entries, Fallback, WidthLimit},
    },
    daemon::questions::{Ask, Listing, Question},
    protocol::{AgentMethod, QuestionMethod},
};

pub(super) async fn run(ctx: &Context, args: QuestionsArgs) -> Result<i32> {
    match args.id {
        Some(id) if args.withdraw => withdraw(ctx, None, id).await,
        Some(id) if args.answer.is_some() => {
            answer(ctx, id, args.answer.unwrap_or_default(), None).await
        }
        Some(id) => {
            let question =
                request::<Question>(&ctx.paths, QuestionMethod::ShowQuestion { id }).await?;
            show(ctx, question).await
        }
        None => list(ctx, args.workspace).await,
    }
}

async fn show(ctx: &Context, question: Question) -> Result<i32> {
    if ctx.interactive_output() && question.is_open() {
        return prompt_answer(ctx, question).await;
    }
    ctx.show(&question, |question| {
        outln!("{}", details(question, Palette::stdout(ctx.json)))
    })?;
    Ok(0)
}

pub(super) async fn ask(
    ctx: &Context,
    workspace: Option<String>,
    ask: Ask,
    wait: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let question: Question = request(
        &ctx.paths,
        QuestionMethod::AskQuestion {
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

/// Mark the caller's agent's turn as asking the user in its chat.
pub(super) async fn ask_in_chat(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::OwnWorkspace).await?;
    request::<()>(&ctx.paths, QuestionMethod::AskInChat { workspace }).await?;
    ctx.emit(
        "Ask your question as your final message and end your turn; it also reaches the user's questions.",
        serde_json::json!({ "in_chat": true }),
    )?;
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
    crate::stdout::flush()?;
    let Some(id) = question.message_id else {
        return Ok(());
    };
    request::<()>(
        &ctx.paths,
        AgentMethod::MarkAgentMessagesDelivered {
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
    let method = || QuestionMethod::AwaitAnswer {
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
    let listing =
        request::<Listing>(&ctx.paths, QuestionMethod::ListQuestions { workspace }).await?;
    if ctx.interactive_output() && !listing.questions.is_empty() {
        let question = pick_question(ctx, listing.questions)?;
        return show(ctx, question).await;
    }
    ctx.show(&listing, |listing| {
        let palette = Palette::stdout(ctx.json);
        for question in &listing.questions {
            outln!("{}", render(question, palette));
        }
        if listing.omitted > 0 {
            outln!("{} older questions not shown", listing.omitted);
        } else if listing.questions.is_empty() {
            outln!("No open questions");
        }
    })?;
    Ok(0)
}

fn pick_question(ctx: &Context, questions: Vec<Question>) -> Result<Question> {
    let id: i64 = ui::pick(ctx, "Question> ", picker_rows(&questions))?.parse()?;
    questions
        .into_iter()
        .find(|question| question.id == id)
        .context("picker returned an unknown question")
}

/// One aligned row per question, as the inbox shows it: its number, the
/// workspace, the question cut to fit the picker, and the recommendation.
fn picker_rows(questions: &[Question]) -> Entries {
    let cells = questions
        .iter()
        .map(|question| {
            [
                (question.id.to_string(), Some(Style::Muted)),
                (question.workspace.clone(), None),
                (question.one_line(), None),
                (
                    question
                        .recommendation()
                        .map(|recommendation| format!("recommends {recommendation}"))
                        .unwrap_or_default(),
                    Some(Style::Muted),
                ),
            ]
        })
        .collect();
    let limit = ui::picker_width().map(|width| WidthLimit { column: 2, width });
    let labels = ui::aligned_to(cells, Palette::picker(), limit);
    questions
        .iter()
        .map(|question| question.id.to_string())
        .zip(labels)
        .collect()
}

pub(super) async fn withdraw(ctx: &Context, workspace: Option<String>, id: i64) -> Result<i32> {
    let question = request::<Question>(&ctx.paths, QuestionMethod::ShowQuestion { id }).await?;
    let workspace = workspace.unwrap_or_else(|| question.workspace_id.clone());
    withdraw_shown(ctx, workspace, question).await
}

async fn withdraw_shown(ctx: &Context, workspace: String, shown: Question) -> Result<i32> {
    let id = shown.id;
    let question = request::<Question>(
        &ctx.paths,
        QuestionMethod::WithdrawQuestion {
            workspace,
            id,
            shown: Some(shown),
        },
    )
    .await?;
    ctx.emit(
        &format!(
            "Question {} withdrawn: {}",
            question.id, question.ask.question
        ),
        &question,
    )?;
    Ok(0)
}

async fn answer(ctx: &Context, id: i64, answer: String, shown: Option<Question>) -> Result<i32> {
    submit_answer(ctx, QuestionMethod::AnswerQuestion { id, answer, shown }).await
}

async fn submit_answer(ctx: &Context, method: QuestionMethod) -> Result<i32> {
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot answer questions without their scope token"
    );
    let question = request::<Question>(&ctx.paths, method).await?;
    ctx.emit(&answered(&question), &question)?;
    Ok(0)
}

/// Names the question by its text too: a stale notification may name a
/// number that a later question reuses.
fn answered(question: &Question) -> String {
    format!(
        "Question {} answered: {}\n  Answer: {}",
        question.id,
        question.ask.question,
        question.answer.as_deref().unwrap_or_default()
    )
}

/// Show the question and read an answer or choice number.
async fn prompt_answer(ctx: &Context, question: Question) -> Result<i32> {
    match read_question_action(ctx, &question)? {
        QuestionAction::Recommend => {
            submit_answer(
                ctx,
                QuestionMethod::AcceptQuestionRecommendation {
                    id: question.id,
                    shown: Some(question),
                },
            )
            .await
        }
        QuestionAction::Answer(response) => {
            answer(ctx, question.id, response, Some(question)).await
        }
    }
}

enum QuestionAction {
    Recommend,
    Answer(String),
}

/// Enter alone accepts the recommendation the prompt shows, and cancels
/// without one; Ctrl-C or end of input cancels.
fn read_question_action(ctx: &Context, question: &Question) -> Result<QuestionAction> {
    eprintln!("{}", details(question, Palette::stderr(ctx.json)));
    let prompt = match question.recommendation() {
        Some(recommendation) => format!("Answer [{recommendation}]"),
        None => "Answer".to_owned(),
    };
    let response = ui::read_input(ctx, &prompt)?.ok_or(ui::SelectionCanceled)?;
    Ok(match response.as_str() {
        "" if question.recommendation().is_some() => QuestionAction::Recommend,
        "" => return Err(ui::SelectionCanceled.into()),
        _ => QuestionAction::Answer(response),
    })
}

/// One line with what the user needs to choose a question: its workspace,
/// the decision, the recommendation and the choices.
pub(in crate::cli) fn summary(question: &Question) -> String {
    let mut line = format!(
        "{}  {}: {}",
        question.id,
        question.workspace,
        question.one_line()
    );
    if let Some(recommendation) = question.recommendation() {
        line.push_str(&format!("  Recommended: {recommendation}"));
    }
    if !question.ask.choices.is_empty() {
        let choices: Vec<_> = (1..)
            .zip(&question.ask.choices)
            .map(|(number, choice)| format!("{number}. {choice}"))
            .collect();
        line.push_str(&format!("  Choices: {}", choices.join(", ")));
    }
    line
}

/// The question's number and text, then labeled rows with its status once
/// settled, workspace, recommendation, reason, numbered choices and answer.
fn details(question: &Question, palette: Palette) -> String {
    // A question asked in the chat is the agent's whole message, which
    // follows the heading.
    let (heading, message) = if question.in_chat {
        (
            format!("Question {}, asked in the chat", question.id),
            Some(&question.ask.question),
        )
    } else {
        (
            format!("Question {}: {}", question.id, question.ask.question),
            None,
        )
    };
    let mut rows: Vec<[Cell; 2]> = Vec::new();
    let mut row = |label: &str, value: &str| {
        rows.push([
            (label.to_owned(), Some(Style::Muted)),
            (value.to_owned(), None),
        ]);
    };
    if !question.is_open() {
        row("Status", status(question));
    }
    row("Workspace", &question.workspace);
    if let Some(recommendation) = question.recommendation() {
        row("Recommended", recommendation);
    }
    if let Some(reason) = &question.ask.reason {
        row("Reason", reason);
    }
    for (number, choice) in (1..).zip(&question.ask.choices) {
        let label = if number == 1 { "Choices" } else { "" };
        row(label, &format!("{number}. {choice}"));
    }
    if let Some(answer) = &question.answer {
        row("Answer", answer);
    }
    let mut lines = vec![palette.paint(Style::Heading, heading)];
    lines.extend(
        message
            .into_iter()
            .flat_map(|message| message.lines())
            .map(|line| format!("  {line}").trim_end().to_owned()),
    );
    lines.extend(
        ui::aligned(rows, palette)
            .into_iter()
            .map(|line| format!("  {line}")),
    );
    lines.join("\n")
}

fn status(question: &Question) -> &'static str {
    if question.withdrawn {
        "withdrawn"
    } else if question.closed_at.is_some() {
        "closed"
    } else if question.is_open() {
        "open"
    } else {
        "answered"
    }
}

fn render(question: &Question, palette: Palette) -> String {
    let mut lines = vec![format!(
        "{} {} {}: {}",
        palette.paint(Style::Heading, question.id),
        palette.paint(Style::Muted, format_args!("[{}]", status(question))),
        question.workspace,
        hanging_indent(&question.ask.question, "  ")
    )];
    if let Some(recommendation) = question.recommendation() {
        lines.push(format!("  Recommended: {recommendation}"));
    }
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

#[cfg(test)]
mod tests;
