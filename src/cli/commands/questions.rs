//! Record questions and let the user inspect, answer or withdraw them.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use crate::client::output::workspaces::InChat;
use crate::{
    cli::{
        args::coordination::QuestionsArgs,
        commands::messages::hanging_indent,
        context::Context,
        installed::{arguments, replace_option, without_option},
        output::{Palette, Style},
        ui::{self, Cell, Entries, Fallback, WidthLimit},
    },
    client::item_numbers::ItemNumber,
    client::questions::{self, Ask, Question},
    installed::Executable,
};

pub(super) async fn run(ctx: &Context, args: QuestionsArgs) -> Result<i32> {
    match args.number {
        Some(number) if args.withdraw => withdraw(ctx, number).await,
        Some(number) if args.answer.is_some() => {
            answer(ctx, number, args.answer.unwrap_or_default(), None).await
        }
        Some(number) => show(ctx, numbered(ctx, number).await?).await,
        None => list(ctx, args.space).await,
    }
}

/// The question `number`.
async fn numbered(ctx: &Context, number: ItemNumber) -> Result<Question> {
    questions::show(&ctx.paths, number).await
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
    space: Option<String>,
    ask: Ask,
    attach: Vec<String>,
    wait: bool,
) -> Result<i32> {
    let attachments = super::messages::attachments(&attach)?;
    crate::client::files::check_sent(&attachments)?;
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let question = questions::ask(&ctx.paths, space, ask, attachments, crate::env::chat()).await?;
    if !wait && question.is_open() {
        ctx.emit(
            &format!(
                "Question {} recorded; the answer arrives as a message.",
                question.number
            ),
            &question,
        )?;
        return Ok(0);
    }
    // The space's ID stays valid if the space is renamed meanwhile.
    let space = question.space_id.clone();
    let question = if question.is_open() {
        await_answer(ctx, &space, &question, false).await?
    } else {
        question
    };
    print_answer(ctx, space, &question).await?;
    Ok(0)
}

/// Mark the caller's agent's turn as asking the user in its conversation.
pub(super) async fn ask_in_chat(ctx: &Context, space: Option<String>) -> Result<i32> {
    let space = ui::select_space(ctx, space, Fallback::OwnWorkspace).await?;
    questions::ask_in_chat(&ctx.paths, space).await?;
    ctx.emit(
        "Ask your question as your final message and end your turn; it also reaches the user's questions.",
        InChat { in_chat: true },
    )?;
    Ok(0)
}

/// Continue an earlier `ask --wait` for question `item` of `space`, which
/// asks `ask`.
pub(super) async fn resume(ctx: &Context, space: String, item: u32, ask: Ask) -> Result<i32> {
    let question = await_answer(ctx, &space, Asked { item, ask }, true).await?;
    print_answer(ctx, space, &question).await?;
    Ok(0)
}

/// Printed here, the answer's message need not reach the agent again.
async fn print_answer(ctx: &Context, space: String, question: &Question) -> Result<()> {
    ctx.emit(question.answer.as_deref().unwrap_or_default(), question)?;
    crate::stdout::flush()?;
    let Some(id) = question.message_id else {
        return Ok(());
    };
    crate::client::agent_messages::mark_delivered(&ctx.paths, space, vec![id]).await
}

/// The question a wait follows: its number within its space and what it
/// asks, since a later question may reuse the number.
struct Asked {
    item: u32,
    ask: Ask,
}

impl From<&Question> for Asked {
    fn from(question: &Question) -> Self {
        Self {
            item: question.number.item,
            ask: question.ask.clone(),
        }
    }
}

/// Wait across daemon restarts; after an upgrade, the installed release
/// resumes the wait for the same question.
async fn await_answer(
    ctx: &Context,
    space: &str,
    asked: impl Into<Asked>,
    restarted: bool,
) -> Result<Question> {
    let asked = asked.into();
    let executable = Executable::current();
    questions::await_answer(&ctx.paths, space, asked.item, &asked.ask, restarted)
        .await
        .map_err(|error| executable.resume_or(error, || resume_arguments(space, asked.item)))
}

/// This command's arguments, which state the question, resuming the wait for
/// its number `item` in `space` without asking again. A wait an earlier
/// release handed over names its workspace the earlier way, which the space
/// replaces.
fn resume_arguments(space: &str, item: u32) -> Vec<OsString> {
    let arguments = without_option(arguments(), "--workspace");
    let arguments = replace_option(arguments, "--space", space);
    replace_option(arguments, "--resume", &item.to_string())
}

pub(super) async fn list(ctx: &Context, space: Option<String>) -> Result<i32> {
    let listing = questions::list(&ctx.paths, space).await?;
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
    let number: ItemNumber = ui::pick(ctx, "Question> ", picker_rows(&questions))?.parse()?;
    questions
        .into_iter()
        .find(|question| question.number == number)
        .context("picker returned an unknown question")
}

/// One aligned row per question, as the inbox shows it: its number, the
/// space, the question cut to fit the picker, and the recommendation.
fn picker_rows(questions: &[Question]) -> Entries {
    let cells = questions
        .iter()
        .map(|question| {
            [
                (question.number.to_string(), Some(Style::Muted)),
                (question.space.clone(), None),
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
        .map(|question| question.number.to_string())
        .zip(labels)
        .collect()
}

/// Withdraw the question `number`, which its space's agent no longer needs
/// answered.
pub(super) async fn withdraw(ctx: &Context, number: ItemNumber) -> Result<i32> {
    let shown = numbered(ctx, number).await?;
    let question = questions::withdraw(&ctx.paths, shown).await?;
    ctx.emit(
        &format!(
            "Question {} withdrawn: {}",
            question.number, question.ask.question
        ),
        &question,
    )?;
    Ok(0)
}

async fn answer(
    ctx: &Context,
    number: ItemNumber,
    answer: String,
    shown: Option<Question>,
) -> Result<i32> {
    ensure_answering_scope(ctx)?;
    let question = questions::answer(&ctx.paths, number, answer, shown).await?;
    ctx.emit(&answered(&question), &question)?;
    Ok(0)
}

fn ensure_answering_scope(ctx: &Context) -> Result<()> {
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot answer questions without their scope token"
    );
    Ok(())
}

/// Names the question by its text too: a stale notification may name a
/// number that a later question reuses.
fn answered(question: &Question) -> String {
    format!(
        "Question {} answered: {}\n  Answer: {}",
        question.number,
        question.ask.question,
        question.answer.as_deref().unwrap_or_default()
    )
}

/// Show the question and read an answer or choice number.
async fn prompt_answer(ctx: &Context, question: Question) -> Result<i32> {
    match read_question_action(ctx, &question)? {
        QuestionAction::Recommend => {
            ensure_answering_scope(ctx)?;
            let question = questions::accept_recommendation(&ctx.paths, question).await?;
            ctx.emit(&answered(&question), &question)?;
            Ok(0)
        }
        QuestionAction::Answer(response) => {
            answer(ctx, question.number, response, Some(question)).await
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

/// One line with what the user needs to choose a question: its space, the
/// decision, the recommendation and the choices.
pub(in crate::cli) fn summary(question: &Question) -> String {
    let mut line = format!(
        "{}  {}: {}",
        question.number,
        question.space,
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
/// settled, space, recommendation, reason, numbered choices and answer.
fn details(question: &Question, palette: Palette) -> String {
    // A question asked in the chat is the agent's whole message, which
    // follows the heading.
    let (heading, message) = if question.in_chat {
        (
            format!("Question {}, asked in the chat", question.number),
            Some(&question.ask.question),
        )
    } else {
        (
            format!("Question {}: {}", question.number, question.ask.question),
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
    row("Space", &question.space);
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
    for (index, file) in question.files.iter().enumerate() {
        let label = if index == 0 { "Files" } else { "" };
        row(label, &file.path.display().to_string());
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
        palette.paint(Style::Heading, question.number),
        palette.paint(Style::Muted, format_args!("[{}]", status(question))),
        question.space,
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
    for file in &question.files {
        lines.push(format!("  Attached file: {}", file.path.display()));
    }
    if let Some(answer) = &question.answer {
        lines.push(format!("  Answer: {answer}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests;
