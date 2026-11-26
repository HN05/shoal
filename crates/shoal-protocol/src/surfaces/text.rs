//! A surface as text, which the terminal and agents read: its components
//! drawn from the root, layout as indented blocks, then one line per action
//! with the command that answers it.
use std::collections::BTreeSet;

use serde_json::Value;

use super::{Component, Surface, SurfaceAction};

/// How far a nested block is indented.
const INDENT: usize = 2;

/// Most components one drawing visits. A component may be named more than
/// once, and a list repeats its template for each item, so without a bound
/// a small surface could draw without end.
pub(super) const MAX_DRAWN: usize = 2000;

impl Surface {
    /// The surface as lines of text, then, with `answer`, the command that
    /// answers it, such as `shoal send 3.2`, its [`action_lines`].
    ///
    /// [`action_lines`]: Surface::action_lines
    pub fn text_lines(&self, answer: Option<&str>) -> Vec<String> {
        let mut lines = self.layout_lines();
        lines.extend(answer.map_or_else(Vec::new, |answer| self.action_lines(answer)));
        lines
    }

    /// Its components drawn from the root, layout as indented blocks.
    /// It stops after [`MAX_DRAWN`] components with a line that says so.
    pub fn layout_lines(&self) -> Vec<String> {
        let mut drawing = Drawing {
            surface: self,
            lines: Vec::new(),
            drawn: BTreeSet::new(),
            visits: 0,
        };
        drawing.draw("root", 0, "");
        if drawing.visits > MAX_DRAWN {
            drawing
                .lines
                .push("… (`--json` gives the whole surface)".to_owned());
        }
        drawing.lines
    }

    /// A line for each action, under a heading, naming the values it takes
    /// and the command that answers it, which starts with `answer`; none
    /// without actions.
    pub fn action_lines(&self, answer: &str) -> Vec<String> {
        let actions = self.actions();
        if actions.is_empty() {
            return Vec::new();
        }
        let commands = actions.iter().map(|action| action_line(action, answer));
        std::iter::once("Actions:".to_owned())
            .chain(commands.map(|line| format!("{:INDENT$}{line}", "")))
            .collect()
    }
}

/// The lines drawn so far, and the components drawn on the path to the
/// one being drawn, which a reference back to them would repeat forever.
struct Drawing<'a> {
    surface: &'a Surface,
    lines: Vec<String>,
    drawn: BTreeSet<&'a str>,
    /// How many components it visited, which [`MAX_DRAWN`] bounds.
    visits: usize,
}

impl<'a> Drawing<'a> {
    /// Draw the component `id` at `indent`, its relative data paths read
    /// under `base`, the data of a list's item.
    fn draw(&mut self, id: &'a str, indent: usize, base: &str) {
        self.visits += 1;
        if self.visits > MAX_DRAWN {
            return;
        }
        let Some(component) = self.surface.component(id) else {
            return;
        };
        if !self.drawn.insert(id) {
            return;
        }
        match component.kind {
            "Column" | "Row" => self.children(component, indent, base),
            "List" => self.children(component, indent + INDENT, base),
            "Card" => self.child(component, "child", indent + INDENT, base),
            "Tabs" => self.tabs(component, indent, base),
            "Modal" => {
                self.child(component, "trigger", indent, base);
                self.child(component, "content", indent + INDENT, base);
            }
            _ => {
                let lines = self.leaf(component, base);
                self.push(indent, lines);
            }
        }
        self.drawn.remove(id);
    }

    fn children(&mut self, component: Component<'a>, indent: usize, base: &str) {
        match component.get("children") {
            Some(Value::Array(ids)) => {
                for id in ids.iter().filter_map(Value::as_str) {
                    self.draw(id, indent, base);
                }
            }
            Some(Value::Object(template)) => self.repeat(template, indent, base),
            _ => {}
        }
    }

    /// Draw a list's template once for each item of the data it repeats
    /// over.
    fn repeat(&mut self, template: &'a serde_json::Map<String, Value>, indent: usize, base: &str) {
        let (Some(id), Some(path)) = (
            template.get("componentId").and_then(Value::as_str),
            template.get("path").and_then(Value::as_str),
        ) else {
            return;
        };
        let path = super::scoped(base, path);
        let items = self
            .surface
            .data(&path)
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        for item in 0..items {
            self.draw(id, indent, &format!("{path}/{item}"));
        }
    }

    fn child(&mut self, component: Component<'a>, field: &str, indent: usize, base: &str) {
        if let Some(id) = component.literal(field) {
            self.draw(id, indent, base);
        }
    }

    fn tabs(&mut self, component: Component<'a>, indent: usize, base: &str) {
        let tabs = component.get("tabs").and_then(Value::as_array);
        for tab in tabs.into_iter().flatten() {
            let title = tab.get("title").and_then(|title| self.text(title, base));
            self.push(indent, vec![format!("{}:", title.unwrap_or_default())]);
            if let Some(id) = tab.get("child").and_then(Value::as_str) {
                self.draw(id, indent + INDENT, base);
            }
        }
    }

    /// The lines of a component that holds no others.
    fn leaf(&self, component: Component<'a>, base: &str) -> Vec<String> {
        let text = |field| (component.get(field)).and_then(|value| self.text(value, base));
        let label = text("label").unwrap_or_default();
        match component.kind {
            "Text" => markdown(&text("text").unwrap_or_default()),
            "Image" => vec![image(
                &text("description").unwrap_or_default(),
                &text("url").unwrap_or_default(),
            )],
            "Divider" => vec!["─".repeat(20)],
            "Button" => vec![format!("[{}]", self.button_label(component, base))],
            "TextField" | "DateTimeInput" => vec![field(&label, text("value"))],
            "CheckBox" => vec![checkbox(&label, self.value(component, "value", base))],
            "Slider" => vec![self.slider(component, &label, base)],
            "ChoicePicker" => self.choice_picker(component, &label, base),
            kind => vec![format!("[{kind}]")],
        }
    }

    /// A slider's label, its value and its range.
    fn slider(&self, component: Component<'a>, label: &str, base: &str) -> String {
        let shown = |value: Option<&Value>, none: &str| value.map_or(none.into(), Value::to_string);
        format!(
            "{label}: {} ({} to {})",
            shown(self.value(component, "value", base), "…"),
            shown(component.get("min"), "0"),
            shown(component.get("max"), "…"),
        )
    }

    /// A choice picker's label, then each option, marked when selected.
    fn choice_picker(&self, component: Component<'a>, label: &str, base: &str) -> Vec<String> {
        let selected = self
            .value(component, "value", base)
            .and_then(Value::as_array);
        let is_selected = |value: &Value| selected.is_some_and(|selected| selected.contains(value));
        let multiple = component.literal("variant") == Some("multipleSelection");
        let options = (component
            .get("options")
            .and_then(Value::as_array)
            .into_iter())
        .flatten()
        .map(|option| {
            let mark = match (multiple, option.get("value").is_some_and(is_selected)) {
                (true, true) => "[x]",
                (true, false) => "[ ]",
                (false, true) => "(•)",
                (false, false) => "( )",
            };
            let name = option.get("label").and_then(|label| self.text(label, base));
            format!("{:INDENT$}{mark} {}", "", name.unwrap_or_default())
        });
        std::iter::once(format!("{label}:"))
            .chain(options)
            .collect()
    }

    /// What a button says: its child's text, else its action's id.
    fn button_label(&self, component: Component<'a>, base: &str) -> String {
        let label = (component.literal("child")).and_then(|id| self.surface.label(id, base));
        let event = (component.event()).and_then(|event| event.get("name")?.as_str());
        label.or(event.map(str::to_owned)).unwrap_or_default()
    }

    /// The data the property `field` binds under `base`, or its literal.
    fn value(&self, component: Component<'a>, field: &str, base: &str) -> Option<&'a Value> {
        self.surface.value(component.get(field)?, base)
    }

    fn text(&self, value: &Value, base: &str) -> Option<String> {
        self.surface.text(value, base)
    }

    fn push(&mut self, indent: usize, lines: Vec<String>) {
        let indented = lines.into_iter().map(|line| match line.is_empty() {
            true => line,
            false => format!("{:indent$}{line}", ""),
        });
        self.lines.extend(indented);
    }
}

/// Markdown text as lines, each image shown as its alt text and file.
fn markdown(text: &str) -> Vec<String> {
    let mut shown = text.to_owned();
    for found in super::markdown::images(text) {
        shown = shown.replace(found.source, &image(&found.alt, &found.target));
    }
    (shown.lines())
        .map(|line| line.trim_end().to_owned())
        .collect()
}

/// A text field's or date's label and value, an ellipsis while empty.
fn field(label: &str, value: Option<String>) -> String {
    let value = value.filter(|value| !value.is_empty());
    format!("{label}: {}", value.unwrap_or_else(|| "…".into()))
}

/// A checkbox, checked when its value is `true`.
fn checkbox(label: &str, value: Option<&Value>) -> String {
    let mark = if value == Some(&Value::Bool(true)) {
        "x"
    } else {
        " "
    };
    format!("[{mark}] {label}")
}

/// An image as text: what it shows and the attached file that holds it.
fn image(alt: &str, file: &str) -> String {
    match alt {
        "" => format!("[image: attached file {file}]"),
        alt => format!("[image: {alt}, attached file {file}]"),
    }
}

/// An action's line: its id, its label unless that is its id, whether it
/// is recommended, the values it takes, and the command that answers it.
fn action_line(action: &SurfaceAction, answer: &str) -> String {
    let mut line = action.id.clone();
    if action.label != action.id {
        line.push_str(&format!(" {}", action.label));
    }
    if action.recommended {
        line.push_str(" (recommended)");
    }
    let takes: Vec<String> = (action.inputs.iter())
        .map(|input| format!("{} ({})", input.key, input.kind.describe()))
        .collect();
    if !takes.is_empty() {
        line.push_str(&format!(", takes {}", takes.join(", ")));
    }
    let values: String = (action.inputs.iter())
        .map(|input| format!(" --value {}=…", quoted(&input.key)))
        .collect();
    format!("{line}: {answer} --action {}{values}", quoted(&action.id))
}

/// `word` as a shell reads it back: as it is when it holds nothing a shell
/// treats specially, else in single quotes.
fn quoted(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_.,:/@%+=".contains(c);
    match !word.is_empty() && word.chars().all(plain) {
        true => word.to_owned(),
        false => format!("'{}'", word.replace('\'', "'\\''")),
    }
}
