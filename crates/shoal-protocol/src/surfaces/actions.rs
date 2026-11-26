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
    /// entries as written, and bound ones with the value given, else the
    /// data model's. A value is refused unless the action takes it and
    /// its input accepts it.
    pub fn resolve(&self, id: &str, mut values: Map<String, Value>) -> Result<Map<String, Value>> {
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
        let mut resolved = Map::new();
        for (key, entry) in self.context(&action.event) {
            let value = match (binding(entry), values.remove(key)) {
                (Some(_), Some(given)) => action.input(key).kind.accept(key, given)?,
                (Some(path), None) => {
                    (self.data(&scoped(&action.scope, path)).cloned()).unwrap_or(Value::Null)
                }
                (None, _) => evaluated(entry, &action.scope),
            };
            resolved.insert(key.clone(), value);
        }
        Ok(resolved)
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
        let mut entered: Vec<(usize, Input)> = (self.context(event).into_iter())
            .filter_map(|(key, value)| {
                let path = scoped(&button.scope, binding(value)?);
                let (at, input) =
                    (self.tree_order().into_iter().enumerate()).find_map(|(at, component)| {
                        let scope = self.scope_toward(component, &path)?;
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
                };
                Some((at, input))
            })
            .collect();
        entered.sort_by_key(|(at, _)| *at);
        entered.into_iter().map(|(_, input)| input).collect()
    }

    /// The scope `component` is placed in where its data could be at
    /// `path`: for each list that repeats it, the item `path` names in
    /// that list's data; none when `path` lies in no item of one.
    fn scope_toward(&self, component: Component<'_>, path: &str) -> Option<String> {
        let mut scope = String::new();
        for list in self.lists_above(component) {
            let items = scoped(&scope, list.template()?.1);
            let rest = path.strip_prefix(&items)?.strip_prefix('/')?;
            let item = rest.split('/').next()?;
            item.parse::<usize>().ok()?;
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

/// A literal context `entry` of an action placed in `scope` as its event
/// carries it: the item's index for a call of `@index`, which counts the
/// innermost list's items from 0 plus its `offset`; any other entry as
/// written, another function's call too, which only a renderer runs.
fn evaluated(entry: &Value, scope: &str) -> Value {
    if entry.get("@call").and_then(Value::as_str) != Some("@index") {
        return entry.clone();
    }
    let item = (scope.rsplit('/').next()).and_then(|item| item.parse::<i64>().ok());
    let offset = (entry.get("args").and_then(|args| args.get("offset")))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    item.map_or_else(|| entry.clone(), |item| Value::from(item + offset))
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
            Self::DateTime(dates) => dates.describe(),
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
