//! Answering a post's surface: one of its actions, picked with the
//! recommended one first, then each value that action takes, or a reply in
//! words instead.
use anyhow::{Context as _, Result};
use serde_json::{Map, Value};
use shoal_protocol::surfaces::{Input, InputKind, SurfaceAction};

use super::{
    Entries, SelectionCanceled, input, input_with_default, pick, pick_choice, pick_many,
    pick_with_header, read_input,
};
use crate::cli::context::Context;

/// The prefix of an action's picker id, which keeps it apart from the
/// reply in words whatever the action's id.
const ACTION: &str = "action:";
/// The picker id of the reply in words.
const WORDS: &str = "words";

/// How the user answers a post's surface.
pub enum Answering {
    /// Its action `id`, with the values entered for its inputs.
    Action {
        id: String,
        values: Map<String, Value>,
    },
    /// A reply in words, as a plain post.
    Words(String),
}

/// Pick one of `actions`, the recommended one first, where the picker
/// starts, or a reply in words, under `asked`, the post they answer; then
/// ask for each value the action takes.
pub fn pick_answer(ctx: &Context, asked: &str, actions: &[SurfaceAction]) -> Result<Answering> {
    let picked = pick_with_header(ctx, "Answer> ", entries(actions), Some(asked))?;
    let Some(id) = picked.strip_prefix(ACTION) else {
        return Ok(Answering::Words(input(ctx, "Reply")?));
    };
    let action = (actions.iter())
        .find(|action| action.id == id)
        .context("picker returned an unknown action")?;
    let mut values = Map::new();
    for input in &action.inputs {
        values.insert(input.key.clone(), value(ctx, input)?);
    }
    Ok(Answering::Action {
        id: id.to_owned(),
        values,
    })
}

/// The picker's rows: each action, the recommended one first, then the
/// reply in words.
fn entries(actions: &[SurfaceAction]) -> Entries {
    let mut ordered: Vec<&SurfaceAction> = actions.iter().collect();
    ordered.sort_by_key(|action| !action.recommended);
    let rows = ordered.into_iter().map(|action| {
        let recommended = if action.recommended {
            " (recommended)"
        } else {
            ""
        };
        let label = format!("{}{recommended}  --action {}", action.label, action.id);
        (format!("{ACTION}{}", action.id), label)
    });
    let words = (WORDS.to_owned(), "Reply in words instead".to_owned());
    rows.chain(std::iter::once(words)).collect()
}

/// The value the user gives the input `input`: picked among its options
/// or `true` and `false`, or typed, offering the value the surface starts
/// with.
fn value(ctx: &Context, input: &Input) -> Result<Value> {
    let prompt = format!("{} ({})", input.key, input.kind.describe());
    match &input.kind {
        InputKind::Options { options, multiple } => picked(ctx, &prompt, options, *multiple),
        InputKind::Boolean => boolean(ctx, &prompt, input),
        _ => typed(ctx, &prompt, input),
    }
}

/// The values of the options picked among `options`, by their labels,
/// several when `multiple`.
fn picked(
    ctx: &Context,
    prompt: &str,
    options: &[(String, String)],
    multiple: bool,
) -> Result<Value> {
    let entries: Entries = options.to_vec();
    let prompt = format!("{prompt}> ");
    let picked = match multiple {
        true => pick_many(ctx, &prompt, entries)?,
        false => vec![pick(ctx, &prompt, entries)?],
    };
    Ok(Value::Array(
        picked.into_iter().map(Value::String).collect(),
    ))
}

/// `true` or `false`, picked with the input's value first.
fn boolean(ctx: &Context, prompt: &str, input: &Input) -> Result<Value> {
    let initial = (input.initial.as_ref())
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let choices = [(initial, label(initial)), (!initial, label(!initial))];
    Ok(Value::Bool(pick_choice(
        ctx,
        &format!("{prompt}> "),
        &choices,
    )?))
}

/// Typed text, offering the input's value when it has one; blank text is
/// an answer too.
fn typed(ctx: &Context, prompt: &str, input: &Input) -> Result<Value> {
    let text = match initial_text(input) {
        Some(initial) => input_with_default(ctx, prompt, &initial)?,
        None => read_input(ctx, prompt)?.ok_or(SelectionCanceled)?,
    };
    Ok(Value::String(text))
}

fn label(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// The value a typed input starts with, unless it starts empty.
fn initial_text(input: &Input) -> Option<String> {
    match input.initial.as_ref()? {
        Value::String(text) if text.is_empty() => None,
        Value::String(text) => Some(text.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}
