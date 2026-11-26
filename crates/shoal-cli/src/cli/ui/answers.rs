//! Answering a post's surface: one of its actions, picked with the
//! recommended one first, then each value that action takes, or a reply in
//! words instead.
use anyhow::{Context as _, Result};
use serde_json::{Map, Value};
use shoal_protocol::surfaces::{Input, InputKind, SurfaceAction};

use super::{
    Entries, SelectionCanceled, input, pick, pick_choice, pick_many_selected, pick_with_header,
    read_input,
};
use crate::cli::context::Context;

/// The prefix of an action's picker id, before its place among the
/// actions: an action's own id could hold what the picker trims or splits.
const ACTION: &str = "action:";
/// The picker id of the reply in words.
const WORDS: &str = "words";
/// The prefix of an option's picker id, before its place among the
/// options, which keeps it apart from picking none whatever its value.
const OPTION: &str = "option:";
/// The picker id of picking no option.
const NO_OPTION: &str = "none";

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
    let Some(place) = picked.strip_prefix(ACTION) else {
        return Ok(Answering::Words(input(ctx, "Reply")?));
    };
    let action = (place.parse::<usize>().ok())
        .and_then(|place| actions.get(place))
        .context("picker returned an unknown action")?;
    let mut values = Map::new();
    for input in &action.inputs {
        values.insert(input.key.clone(), value(ctx, input)?);
    }
    Ok(Answering::Action {
        id: action.id.clone(),
        values,
    })
}

/// The picker's rows: each action, the recommended one first, then the
/// reply in words.
fn entries(actions: &[SurfaceAction]) -> Entries {
    let mut ordered: Vec<(usize, &SurfaceAction)> = actions.iter().enumerate().collect();
    ordered.sort_by_key(|(_, action)| !action.recommended);
    let rows = ordered.into_iter().map(|(place, action)| {
        let recommended = if action.recommended {
            " (recommended)"
        } else {
            ""
        };
        let label = format!("{}{recommended}  --action {}", action.label, action.id);
        (format!("{ACTION}{place}"), label)
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
        InputKind::Options { options, multiple } => picked(ctx, &prompt, input, options, *multiple),
        InputKind::Boolean => boolean(ctx, &prompt, input),
        _ => typed(ctx, &prompt, input),
    }
}

/// The values of the options picked among `options`, by their labels,
/// several when `multiple`, or none, starting from those `input` holds;
/// picking none clears the rest.
fn picked(
    ctx: &Context,
    prompt: &str,
    input: &Input,
    options: &[(String, String)],
    multiple: bool,
) -> Result<Value> {
    let (entries, selected) = option_entries(options, &selected_values(input), multiple);
    let prompt = format!("{prompt}> ");
    let picked = match multiple {
        true => pick_many_selected(ctx, &prompt, entries, &selected)?,
        false => vec![pick(ctx, &prompt, entries)?],
    };
    Ok(picked_values(&picked, options))
}

/// The values of the options the picker rows `picked` name; none where
/// they pick no option, whatever else they select.
fn picked_values(picked: &[String], options: &[(String, String)]) -> Value {
    if picked.iter().any(|id| id == NO_OPTION) {
        return Value::Array(Vec::new());
    }
    let places = (picked.iter()).filter_map(|id| id.strip_prefix(OPTION)?.parse::<usize>().ok());
    let values = places.filter_map(|place| options.get(place));
    Value::Array(
        values
            .map(|(value, _)| Value::String(value.clone()))
            .collect(),
    )
}

/// The rows offering `options` and none, and the ids of those to start
/// selected: for one option, the one `current` names leads, or picking
/// none where it names none; for several, those it names are selected,
/// with picking none leading where it names none, so Enter alone keeps
/// the selection the surface starts with.
fn option_entries(
    options: &[(String, String)],
    current: &[String],
    multiple: bool,
) -> (Entries, Vec<String>) {
    let mut rows: Vec<(String, String)> = (options.iter().enumerate())
        .map(|(place, (_, label))| (format!("{OPTION}{place}"), label.clone()))
        .collect();
    let chosen: Vec<String> = (options.iter().enumerate())
        .filter(|(_, (value, _))| current.contains(value))
        .map(|(place, _)| format!("{OPTION}{place}"))
        .collect();
    let none = (NO_OPTION.to_owned(), "No option".to_owned());
    match (chosen.first(), multiple) {
        (None, _) => rows.insert(0, none),
        (Some(first), false) => {
            let at = rows.iter().position(|(id, _)| id == first).unwrap_or(0);
            let lead = rows.remove(at);
            rows.insert(0, lead);
            rows.push(none);
        }
        (Some(_), true) => rows.push(none),
    }
    let selected = if multiple { chosen } else { Vec::new() };
    (rows, selected)
}

/// The option values `input` starts with: its text, or each text of its
/// list.
fn selected_values(input: &Input) -> Vec<String> {
    match &input.initial {
        Some(Value::String(value)) => vec![value.clone()],
        Some(Value::Array(values)) => (values.iter())
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
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

/// Typed text, blank text an answer too; where the input has a value,
/// Enter alone keeps it and a lone `-` clears it.
fn typed(ctx: &Context, prompt: &str, input: &Input) -> Result<Value> {
    let Some(initial) = initial_text(input) else {
        let text = read_input(ctx, prompt)?.ok_or(SelectionCanceled)?;
        return Ok(Value::String(text));
    };
    let prompt = format!("{prompt} [{initial}; - clears it]");
    let text = read_input(ctx, &prompt)?.ok_or(SelectionCanceled)?;
    Ok(Value::String(match text.as_str() {
        "" => initial,
        CLEAR => String::new(),
        _ => text,
    }))
}

/// What clears a typed input's value.
const CLEAR: &str = "-";

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

#[cfg(test)]
mod tests;
