//! The actions a surface offers, each the agent event of one of its
//! buttons, with the inputs whose values it takes, and how an answer's
//! values resolve its context.
use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Map, Value};

use super::{Component, Surface, binding};

/// An action a surface offers.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceAction {
    /// Its id: the name of the event its button dispatches, which an
    /// answer names.
    pub id: String,
    /// What its button says.
    pub label: String,
    /// Whether the surface recommends it, as its primary button.
    pub recommended: bool,
    /// The values it takes, one for each key of its context that binds an
    /// input's data, in the order their inputs render.
    pub inputs: Vec<Input>,
}

/// A value an action takes, by its context's key.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub key: String,
    pub kind: InputKind,
    /// The value the surface starts with, from its data model.
    pub initial: Option<Value>,
}

/// What an input takes, as the component bound to its data enters it.
#[derive(Debug, Clone, PartialEq)]
pub enum InputKind {
    Text,
    Number {
        min: Option<f64>,
        max: Option<f64>,
    },
    Boolean,
    /// One of the options' values, or with `multiple` several; each
    /// option is its value and its label.
    Options {
        options: Vec<(String, String)>,
        multiple: bool,
    },
    DateTime,
    /// Data no input component enters, taken as given.
    Any,
}

impl Surface {
    /// Its actions, in the order its components render from the root.
    pub fn actions(&self) -> Vec<SurfaceAction> {
        (self.rendered().into_iter())
            .filter_map(|component| self.action_of(component))
            .collect()
    }

    /// Its action `id`.
    pub fn action(&self, id: &str) -> Option<SurfaceAction> {
        self.actions().into_iter().find(|action| action.id == id)
    }

    /// The recommended action's label.
    pub fn recommendation(&self) -> Option<String> {
        (self.actions().into_iter())
            .find(|action| action.recommended)
            .map(|action| action.label)
    }

    /// The context of the action `id` with `values` entered for its
    /// inputs, as the event the answer dispatches carries it: literal
    /// entries as written, and bound ones with the value given, else the
    /// data model's. A value is refused unless the action takes it and
    /// its input accepts it.
    pub fn resolve(&self, id: &str, mut values: Map<String, Value>) -> Result<Map<String, Value>> {
        let Some(action) = self.action(id) else {
            let ids: Vec<String> = self.actions().into_iter().map(|action| action.id).collect();
            match &ids[..] {
                [] => bail!("the surface offers no actions"),
                ids => bail!("the surface has no action {id}; it has {}", ids.join(", ")),
            }
        };
        if let Some(key) = values.keys().find(|key| !action.takes(key)) {
            match action.inputs.len() {
                0 => bail!("action {id} takes no values"),
                _ => bail!(
                    "action {id} takes no value {key}; it takes {}",
                    action.keys()
                ),
            }
        }
        let mut resolved = Map::new();
        for (key, entry) in self.context(id) {
            let value = match (binding(entry), values.remove(key)) {
                (Some(_), Some(given)) => action.input(key).kind.accept(key, given)?,
                (Some(path), None) => self.data(path).cloned().unwrap_or(Value::Null),
                (None, _) => entry.clone(),
            };
            resolved.insert(key.clone(), value);
        }
        Ok(resolved)
    }

    /// The action of `component`, a button that dispatches an agent event.
    fn action_of(&self, component: Component<'_>) -> Option<SurfaceAction> {
        let event = component.event()?;
        let id = event.get("name")?.as_str()?.to_owned();
        let mut bound: Vec<(&String, &str)> = (self.context(&id).into_iter())
            .filter_map(|(key, value)| Some((key, binding(value)?)))
            .collect();
        let rendered = self.rendered();
        let shown = |path: &str| {
            let entering = |component: &Component<'_>| entering(*component, path);
            rendered.iter().position(entering).unwrap_or(usize::MAX)
        };
        bound.sort_by_key(|(_, path)| shown(path));
        let inputs = (bound.into_iter())
            .map(|(key, path)| self.input(key, path))
            .collect();
        Some(SurfaceAction {
            label: (component.literal("child"))
                .and_then(|child| self.label(child, ""))
                .unwrap_or_else(|| id.clone()),
            recommended: component.literal("variant") == Some("primary"),
            id,
            inputs,
        })
    }

    /// The context the event `id` carries, by key.
    fn context(&self, id: &str) -> Vec<(&String, &Value)> {
        let event = (self.components())
            .filter_map(|component| component.event())
            .find(|event| event.get("name").and_then(Value::as_str) == Some(id));
        (event
            .and_then(|event| event.get("context")?.as_object())
            .into_iter())
        .flatten()
        .collect()
    }

    /// The input `key` of an action, bound to the data at `path`: what
    /// the component bound there enters, and the data it starts with.
    fn input(&self, key: &str, path: &str) -> Input {
        let entering = (self.components()).find(|component| entering(*component, path));
        Input {
            key: key.to_owned(),
            kind: entering.and_then(entry).unwrap_or(InputKind::Any),
            initial: self.data(path).cloned(),
        }
    }

    /// What the component `id` says, as a button's label: a `Text`'s
    /// text, its data read under `base`, or an `Icon`'s name.
    pub(super) fn label(&self, id: &str, base: &str) -> Option<String> {
        let component = self.component(id)?;
        match component.kind {
            "Text" => self.text(component.get("text")?, base),
            "Icon" => component.literal("name").map(str::to_owned),
            _ => None,
        }
    }
}

impl SurfaceAction {
    fn takes(&self, key: &str) -> bool {
        self.inputs.iter().any(|input| input.key == key)
    }

    /// Its input `key`, which it takes.
    fn input(&self, key: &str) -> &Input {
        (self.inputs.iter())
            .find(|input| input.key == key)
            .expect("the action takes the value")
    }

    /// The keys of its inputs, as a refusal lists them.
    fn keys(&self) -> String {
        let keys: Vec<&str> = self.inputs.iter().map(|input| input.key.as_str()).collect();
        keys.join(", ")
    }
}

/// Whether `component` is an input that enters the data at `path`.
fn entering(component: Component<'_>, path: &str) -> bool {
    component.binding("value") == Some(path) && entry(component).is_some()
}

/// What `component` enters into the data its value binds, for an input.
fn entry(component: Component<'_>) -> Option<InputKind> {
    let number = |field| component.get(field).and_then(Value::as_f64);
    Some(match component.kind {
        "TextField" if component.literal("variant") == Some("number") => InputKind::Number {
            min: None,
            max: None,
        },
        "TextField" => InputKind::Text,
        "CheckBox" => InputKind::Boolean,
        "Slider" => InputKind::Number {
            min: number("min").or(Some(0.0)),
            max: number("max"),
        },
        "DateTimeInput" => InputKind::DateTime,
        "ChoicePicker" => InputKind::Options {
            options: (component
                .get("options")
                .and_then(Value::as_array)
                .into_iter())
            .flatten()
            .filter_map(|option| {
                let value = option.get("value")?.as_str()?;
                let label = option.get("label").and_then(Value::as_str).unwrap_or(value);
                Some((value.to_owned(), label.to_owned()))
            })
            .collect(),
            multiple: component.literal("variant") == Some("multipleSelection"),
        },
        _ => return None,
    })
}

impl InputKind {
    /// `given` as the value of the input `key`, which takes text the way
    /// `--value KEY=VALUE` passes it as well as typed JSON.
    fn accept(&self, key: &str, given: Value) -> Result<Value> {
        match (self, given) {
            (Self::Any, given) => Ok(given),
            (Self::Text | Self::DateTime, Value::String(text)) => Ok(Value::String(text)),
            (Self::Number { min, max }, given) => number(key, &given, *min, *max),
            (Self::Boolean, Value::Bool(value)) => Ok(Value::Bool(value)),
            (Self::Boolean, Value::String(text)) => match text.as_str() {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => bail!("{key} takes true or false, not {text}"),
            },
            (Self::Options { options, multiple }, given) => {
                selection(key, given, options, *multiple)
            }
            (kind, given) => bail!("{key} takes {}, not {given}", kind.describe()),
        }
    }

    /// What it takes, in a few words.
    pub fn describe(&self) -> String {
        match self {
            Self::Text => "text".into(),
            Self::Number {
                min: Some(min),
                max: Some(max),
            } => format!("a number from {min} to {max}"),
            Self::Number { .. } => "a number".into(),
            Self::Boolean => "true or false".into(),
            Self::Options {
                options,
                multiple: false,
            } => format!("one of {}", values(options)),
            Self::Options {
                options,
                multiple: true,
            } => format!("any of {}, repeated", values(options)),
            Self::DateTime => "an ISO 8601 date or time".into(),
            Self::Any => "a value".into(),
        }
    }
}

/// `given`, a number or its text, as the number of the input `key`, within
/// `min` and `max`: an integer stays one, as a renderer sends it.
fn number(key: &str, given: &Value, min: Option<f64>, max: Option<f64>) -> Result<Value> {
    let number = match given {
        Value::Number(number) => Some(number.clone()),
        Value::String(text) => parsed(text.trim()),
        _ => None,
    }
    .with_context(|| format!("{key} takes a number, not {given}"))?;
    let value = number.as_f64().unwrap_or(f64::NAN);
    ensure!(
        min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max),
        "{key} takes {}, not {number}",
        InputKind::Number { min, max }.describe()
    );
    Ok(Value::Number(number))
}

/// The number `text` spells, an integer where it is one.
fn parsed(text: &str) -> Option<serde_json::Number> {
    match text.parse::<i64>() {
        Ok(integer) => Some(integer.into()),
        Err(_) => serde_json::Number::from_f64(text.parse().ok()?),
    }
}

/// `given`, one option's value or a list of them, as the selection of the
/// input `key`, which offers `options` and takes several when `multiple`.
fn selection(
    key: &str,
    given: Value,
    options: &[(String, String)],
    multiple: bool,
) -> Result<Value> {
    let picked: Vec<Value> = match given {
        Value::Array(picked) => picked,
        single => vec![single],
    };
    ensure!(
        multiple || picked.len() <= 1,
        "{key} takes one of {}",
        values(options)
    );
    for value in &picked {
        let offered = |value: &str| options.iter().any(|(offered, _)| offered == value);
        ensure!(
            value.as_str().is_some_and(offered),
            "{key} takes {}, not {value}",
            values(options)
        );
    }
    Ok(Value::Array(picked))
}

/// The values of `options`, as `--value` takes them.
fn values(options: &[(String, String)]) -> String {
    let values: Vec<&str> = options.iter().map(|(value, _)| value.as_str()).collect();
    values.join("|")
}
