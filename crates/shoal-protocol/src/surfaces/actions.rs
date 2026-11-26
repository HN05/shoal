//! The actions a surface offers, each the agent event of one of its
//! buttons, with the inputs whose values it takes, and how an answer's
//! values resolve its context.
use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Map, Value};

use super::{Component, Placed, Surface, binding, dates::Dates, scoped};

/// An action a surface offers.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceAction {
    /// Its id, which an answer names: the name of the event its button
    /// dispatches, and in a list's item, `#` and the item's number from 1,
    /// each list's joined by `.` in nested lists, such as `done#2.1`.
    pub id: String,
    /// The name of the event its button dispatches.
    pub event: String,
    /// The data path its context's relative bindings read under: its list
    /// item's, or the root's.
    pub scope: String,
    /// What its button says.
    pub label: String,
    /// Whether the surface recommends it, as its primary button.
    pub recommended: bool,
    /// The values it takes, one for each key of its context that binds the
    /// data of an input component, in the order those components render.
    pub inputs: Vec<Input>,
}

/// A value an action takes, by its context's key.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub key: String,
    pub kind: InputKind,
    /// The value the surface starts with, from its data model.
    pub initial: Option<Value>,
    /// The absolute data path its component enters.
    pub path: String,
}

/// What an input takes, as the component bound to its data enters it.
#[derive(Debug, Clone, PartialEq)]
pub enum InputKind {
    Text,
    /// A number field's text: a number, or left empty, as its field may be.
    NumberText,
    /// A slider's number, from `min` to `max`, and on a `step` from `min`
    /// where the slider snaps to its divisions.
    Number {
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
    },
    Boolean,
    /// One of the options' values, or with `multiple` several; each
    /// option is its value and its label.
    Options {
        options: Vec<(String, String)>,
        multiple: bool,
    },
    DateTime(Dates),
}

impl Surface {
    /// Its actions, in the order its components render from the root.
    pub fn actions(&self) -> Vec<SurfaceAction> {
        (self.placed().iter())
            .filter_map(|placed| self.action_of(placed))
            .collect()
    }

    /// Its action `id`, found from the id itself rather than by placing
    /// every component, so an action past the bound placing keeps stays
    /// answerable: the button that dispatches the event, under the lists
    /// that repeat it, in the items the id numbers.
    pub fn action(&self, id: &str) -> Option<SurfaceAction> {
        let (event, numbers) = match id.split_once('#') {
            Some((event, numbers)) => (event, numbers.split('.').collect()),
            None => (id, Vec::new()),
        };
        let button = (self.components()).find(|component| component.event_name() == Some(event))?;
        let lists = self.lists_above(button);
        if lists.len() != numbers.len() {
            return None;
        }
        let mut placed = Placed {
            component: button,
            scope: String::new(),
            items: Vec::new(),
        };
        for (list, number) in lists.into_iter().zip(numbers) {
            let item = number.parse::<usize>().ok()?.checked_sub(1)?;
            let (_, path) = list.template()?;
            let items = scoped(&placed.scope, path);
            let count = self
                .data(&items)
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            if item >= count {
                return None;
            }
            placed.scope = format!("{items}/{item}");
            placed.items.push(item);
        }
        self.action_of(&placed).filter(|action| action.id == id)
    }

    /// The lists whose templates hold `component`, the outermost first.
    /// It climbs at most once per component, so a stored surface whose
    /// components name each other in a loop ends the climb.
    fn lists_above(&self, component: Component<'_>) -> Vec<Component<'_>> {
        let parent =
            |id: &str| (self.components()).find(|parent| parent.references().contains(&id));
        let mut lists = Vec::new();
        let mut current = component;
        for _ in 0..self.components().count() {
            let Some(above) = (current.id != "root").then(|| parent(current.id)).flatten() else {
                break;
            };
            if (above.template()).is_some_and(|(template, _)| template == current.id) {
                lists.push(above);
            }
            current = above;
        }
        lists.reverse();
        lists
    }

    /// The recommended action's label.
    pub fn recommendation(&self) -> Option<String> {
        (self.actions().into_iter())
            .find(|action| action.recommended)
            .map(|action| action.label)
    }

    /// The context of the action `id` with `values` entered for its
    /// inputs, as the event the answer dispatches carries it: literal
    /// entries as written, and bound ones read from the data model with
    /// the values entered. A value is refused unless the action takes it
    /// and its input accepts it, with the bounds the other values give it,
    /// such as an end date's earliest, which a start date binds.
    pub fn resolve(&self, id: &str, values: Map<String, Value>) -> Result<Map<String, Value>> {
        ensure!(
            super::checks::plain(id, false),
            "an action's id holds no control characters"
        );
        super::checks::check_data(&Value::Object(values.clone()))?;
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
        let mut entered = self.entered(&action, &values);
        // Entered data can reshape a list; keep the action as offered then.
        let action = (entered.action(id))
            .filter(|entered| values.keys().all(|key| entered.takes(key)))
            .unwrap_or(action);
        for (key, given) in values {
            let input = action.input(&key);
            let accepted = input.kind.accept(&key, given)?;
            entered.enter(&input.path, accepted)?;
        }
        Ok(entered.dispatched(&action))
    }

    /// The surface with `values` entered into the data of `action`'s
    /// inputs, each as its input reads it where it can, so bounds bound to
    /// that data read what the answer gives.
    fn entered(&self, action: &SurfaceAction, values: &Map<String, Value>) -> Surface {
        let mut entered = self.clone();
        for (key, given) in values {
            let input = action.input(key);
            let value = (input.kind.accept(key, given.clone())).unwrap_or_else(|_| given.clone());
            let _ = entered.enter(&input.path, value);
        }
        entered
    }

    /// The context `action`'s event carries: each bound entry read from
    /// the data model, each literal one as [`Self::evaluated`] gives it.
    fn dispatched(&self, action: &SurfaceAction) -> Map<String, Value> {
        (self.context(&action.event).into_iter())
            .map(|(key, entry)| {
                let value = match binding(entry) {
                    Some(path) => {
                        (self.data(&scoped(&action.scope, path)).cloned()).unwrap_or(Value::Null)
                    }
                    None => self.evaluated(entry, &action.scope),
                };
                (key.clone(), value)
            })
            .collect()
    }

    /// Write `value` into the data model at the JSON pointer `path`, as a
    /// renderer's input does: in place, creating the model and what holds
    /// the value where it is missing or null, an array's item by its index
    /// and else an object's field, and an array's next item. Refused where
    /// other data stands in the way, such as text where the path names a
    /// field of it, or an item past an array's next.
    fn enter(&mut self, path: &str, value: Value) -> Result<()> {
        let body = (self.0.get_mut("createSurface"))
            .and_then(Value::as_object_mut)
            .context("a surface holds a createSurface object")?;
        let mut at = (body.entry("dataModel")).or_insert(Value::Null);
        for field in path.split('/').skip(1) {
            let field = field.replace("~1", "/").replace("~0", "~");
            let index = field.parse::<usize>().ok();
            if at.is_null() {
                *at = match index {
                    Some(_) => Value::Array(Vec::new()),
                    None => Value::Object(Map::new()),
                };
            }
            at = match (at, index) {
                (Value::Array(items), Some(index)) if index <= items.len() => {
                    if index == items.len() {
                        items.push(Value::Null);
                    }
                    &mut items[index]
                }
                (Value::Object(fields), _) => fields.entry(field).or_insert(Value::Null),
                _ => bail!("the data at {path} cannot hold the value entered"),
            };
        }
        *at = value;
        Ok(())
    }

    /// The action of `placed`, a button that dispatches an agent event.
    fn action_of(&self, placed: &Placed<'_>) -> Option<SurfaceAction> {
        let component = placed.component;
        let event = component.event()?.get("name")?.as_str()?.to_owned();
        let id = match &placed.items[..] {
            [] => event.clone(),
            items => {
                let numbers: Vec<String> =
                    items.iter().map(|item| (item + 1).to_string()).collect();
                format!("{event}#{}", numbers.join("."))
            }
        };
        Some(SurfaceAction {
            label: (component.literal("child"))
                .and_then(|child| self.label(child, &placed.scope))
                .unwrap_or_else(|| id.clone()),
            recommended: component.literal("variant") == Some("primary"),
            inputs: self.inputs(&event, placed),
            id,
            event,
            scope: placed.scope.clone(),
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

    /// The inputs of `button`'s event: the keys of its context that bind
    /// data an input component enters, wherever the input is placed, in the
    /// order those components render. A key that binds data no component
    /// enters takes the data as it is.
    fn inputs(&self, event: &str, button: &Placed<'_>) -> Vec<Input> {
        let around: Vec<(&str, usize)> = (self.lists_above(button.component).into_iter())
            .map(|list| list.id)
            .zip(button.items.iter().copied())
            .collect();
        let mut entered: Vec<(usize, Input)> = (self.context(event).into_iter())
            .filter_map(|(key, value)| {
                let path = scoped(&button.scope, binding(value)?);
                let (at, input) =
                    (self.tree_order().into_iter().enumerate()).find_map(|(at, component)| {
                        let scope = self.scope_toward(component, &path, &around)?;
                        let placed = Placed {
                            component,
                            scope,
                            items: Vec::new(),
                        };
                        entering(&placed, &path).then_some((at, placed))
                    })?;
                let input = Input {
                    key: key.clone(),
                    kind: self.entry(&input)?,
                    initial: self.data(&path).cloned(),
                    path,
                };
                Some((at, input))
            })
            .collect();
        entered.sort_by_key(|(at, _)| *at);
        entered.into_iter().map(|(_, input)| input).collect()
    }

    /// The scope `component` is placed in where its data could be at
    /// `path`: for each list that repeats it, the item `path` names in
    /// that list's data; none when `path` lies in no item of one. An input
    /// that binds absolute data reads it in every item, so it takes the
    /// item the button is in where the list is one of `around`, the lists
    /// around the button with the item of each, else the first, for what
    /// else it reads in its item, such as its bounds.
    fn scope_toward(
        &self,
        component: Component<'_>,
        path: &str,
        around: &[(&str, usize)],
    ) -> Option<String> {
        let absolute = component
            .binding("value")
            .is_some_and(|bound| bound.starts_with('/'));
        let lists = self.lists_above(component);
        // A list over absolute data repeats the same items in every item
        // of the lists around it, so the scope starts afresh there.
        let fresh = (lists.iter())
            .rposition(|list| {
                list.template()
                    .is_some_and(|(_, path)| path.starts_with('/'))
            })
            .unwrap_or(0);
        let mut scope = String::new();
        for (at, list) in lists.iter().enumerate().skip(fresh) {
            let items = scoped(&scope, list.template()?.1);
            let within = |path: &str| {
                let item = path
                    .strip_prefix(&items)?
                    .strip_prefix('/')?
                    .split('/')
                    .next()?;
                item.parse::<usize>().ok().map(|_| item.to_owned())
            };
            let item = match absolute {
                true => (around.get(at))
                    .filter(|(id, _)| *id == list.id)
                    .map_or(0, |(_, item)| *item)
                    .to_string(),
                false => within(path)?,
            };
            scope = format!("{items}/{item}");
        }
        Some(scope)
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

/// The component types that enter data, which their `value` binds.
const INPUTS: &[&str] = &[
    "TextField",
    "CheckBox",
    "Slider",
    "DateTimeInput",
    "ChoicePicker",
];

/// Whether `placed` is an input that enters the data at `path`, an
/// absolute one.
fn entering(placed: &Placed<'_>, path: &str) -> bool {
    let component = placed.component;
    let binds = |bound: &str| scoped(&placed.scope, bound) == path;
    component.binding("value").is_some_and(binds) && INPUTS.contains(&component.kind)
}

impl Surface {
    /// What `placed`, one of its inputs, enters into the data its value
    /// binds.
    fn entry(&self, placed: &Placed<'_>) -> Option<InputKind> {
        let component = placed.component;
        let number = |field| component.get(field).and_then(Value::as_f64);
        Some(match component.kind {
            "TextField" if component.literal("variant") == Some("number") => InputKind::NumberText,
            "TextField" => InputKind::Text,
            "CheckBox" => InputKind::Boolean,
            "Slider" => {
                let min = number("min").unwrap_or(0.0);
                let max = number("max");
                let steps = number("steps").filter(|steps| *steps >= 1.0);
                InputKind::Number {
                    min: Some(min),
                    max,
                    step: (max.zip(steps).map(|(max, steps)| (max - min) / steps))
                        .filter(|step| *step > 0.0),
                }
            }
            "DateTimeInput" => InputKind::DateTime(Dates::of(self, component, &placed.scope)),
            "ChoicePicker" => InputKind::Options {
                options: (component
                    .get("options")
                    .and_then(Value::as_array)
                    .into_iter())
                .flatten()
                .filter_map(|option| {
                    let value = option.get("value")?.as_str()?;
                    let label = (option.get("label"))
                        .and_then(|label| self.text(label, &placed.scope))
                        .unwrap_or_else(|| value.to_owned());
                    Some((value.to_owned(), label))
                })
                .collect(),
                multiple: component.literal("variant") == Some("multipleSelection"),
            },
            _ => return None,
        })
    }
}

impl InputKind {
    /// `given` as the value of the input `key`, which takes text the way
    /// `--value KEY=VALUE` passes it as well as typed JSON.
    fn accept(&self, key: &str, given: Value) -> Result<Value> {
        match (self, given) {
            (Self::Text, Value::String(text)) => Ok(Value::String(text)),
            (Self::DateTime(dates), Value::String(text)) => dates.accept(key, &text),
            (Self::NumberText, Value::String(text)) if text.trim().is_empty() => {
                Ok(Value::String(String::new()))
            }
            (Self::NumberText, given) => number(key, &given).map(Value::Number),
            (Self::Number { min, max, step }, given) => {
                let number = number(key, &given)?;
                let value = number.as_f64().unwrap_or(f64::NAN);
                ensure!(
                    min.is_none_or(|min| value >= min)
                        && max.is_none_or(|max| value <= max)
                        && step.is_none_or(|step| on_step(value, min.unwrap_or(0.0), step)),
                    "{key} takes {}, not {number}",
                    self.describe()
                );
                Ok(Value::Number(number))
            }
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
                step: Some(step),
            } => format!("a number from {min} to {max} in steps of {step}"),
            Self::Number {
                min: Some(min),
                max: Some(max),
                ..
            } => format!("a number from {min} to {max}"),
            Self::Number { .. } => "a number".into(),
            Self::NumberText => "a number, or empty to leave it unset".into(),
            Self::Boolean => "true or false".into(),
            Self::Options {
                options,
                multiple: false,
            } => format!("one of {}", values(options)),
            Self::Options {
                options,
                multiple: true,
            } => format!("any of {}, repeated", values(options)),
            Self::DateTime(dates) => dates.describe(),
        }
    }
}

/// `given`, a number or its text, as the number of the input `key`: an
/// integer stays one, as a renderer sends it.
fn number(key: &str, given: &Value) -> Result<serde_json::Number> {
    match given {
        Value::Number(number) => Some(number.clone()),
        Value::String(text) => parsed(text.trim()),
        _ => None,
    }
    .with_context(|| format!("{key} takes a number, not {given}"))
}

/// Whether `value` lies a whole number of `step`s from `min`, within the
/// rounding of a division.
fn on_step(value: f64, min: f64, step: f64) -> bool {
    let steps = (value - min) / step;
    (steps - steps.round()).abs() < 1e-9
}

/// The number `text` spells, an integer where it is one.
fn parsed(text: &str) -> Option<serde_json::Number> {
    match text.parse::<i64>() {
        Ok(integer) => Some(integer.into()),
        Err(_) => serde_json::Number::from_f64(text.parse().ok()?),
    }
}

/// `given`, one option's value or a list of them, as the selection of the
/// input `key`, which offers `options` and takes several when `multiple`;
/// empty text, as `--value KEY=` gives it, selects none.
fn selection(
    key: &str,
    given: Value,
    options: &[(String, String)],
    multiple: bool,
) -> Result<Value> {
    let picked: Vec<Value> = match given {
        Value::Array(picked) => picked,
        Value::String(text) if text.is_empty() => Vec::new(),
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
