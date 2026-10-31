//! What an AI tool's launcher takes beyond its command: argument templates
//! and changes to its environment.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::Path,
};

/// What an AI tool's tracked launches change in the environment they would
/// otherwise run with: values as written, and the variables they remove.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    pub(super) set: BTreeMap<String, String>,
    pub remove: BTreeSet<String>,
}

impl Environment {
    /// The values set, with a leading `~/` naming a path under `home`.
    pub fn values(&self, home: &Path) -> BTreeMap<String, OsString> {
        self.set
            .iter()
            .map(|(variable, value)| {
                let value = match value.strip_prefix("~/") {
                    Some(relative) => home.join(relative).into_os_string(),
                    None => value.into(),
                };
                (variable.clone(), value)
            })
            .collect()
    }

    /// `configured`, the variables a launch adds to this process's, with
    /// these changes.
    pub fn layer(&self, configured: &mut BTreeMap<String, OsString>, home: &Path) {
        for variable in &self.remove {
            configured.remove(variable);
        }
        configured.extend(self.values(home));
    }

    /// Make these changes to `command`'s environment.
    pub fn apply(&self, command: &mut std::process::Command, home: &Path) {
        for variable in &self.remove {
            command.env_remove(variable);
        }
        command.envs(self.values(home));
    }
}

/// What a provider's launcher takes after its command, from `[agents.<name>]`,
/// else the built-in provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Arguments {
    pub instructions: Option<Vec<String>>,
    pub resume: Option<Vec<String>>,
    pub resume_latest: Option<Vec<String>>,
    pub fork: Option<Vec<String>>,
    pub add_dir: Option<Vec<String>>,
    pub review: Option<Vec<String>>,
}

/// What a review's arguments name.
pub struct ReviewFields<'a> {
    pub prompt: &'a str,
    /// The output schema's file.
    pub schema: &'a Path,
    /// The output schema's text.
    pub schema_json: &'a str,
    /// The file the result goes to when the arguments name it.
    pub output: &'a Path,
}

impl Arguments {
    /// The arguments that give the agent `instructions`; `None` when it takes
    /// them only in its prompt. Empty instructions need no arguments.
    pub fn instruction_args(&self, instructions: &str) -> Option<Vec<OsString>> {
        let template = self.instructions.as_ref()?;
        if instructions.is_empty() {
            return Some(Vec::new());
        }
        let toml = toml::Value::String(instructions.to_owned()).to_string();
        Some(substitute(
            template,
            &[
                ("{instructions}", instructions),
                ("{instructions_toml}", &toml),
            ],
        ))
    }

    /// The arguments that resume `session`, or the workspace's latest session
    /// when none was reported; `None` when the agent cannot resume.
    pub fn resume_args(&self, session: Option<&str>) -> Option<Vec<OsString>> {
        match session {
            Some(session) => self
                .resume
                .as_ref()
                .map(|template| substitute(template, &[("{session}", session)])),
            None => self
                .resume_latest
                .as_ref()
                .map(|template| substitute(template, &[])),
        }
    }

    /// The arguments that start a new session in `directory` from a fork
    /// of `session`; `None` when the agent cannot fork one.
    pub fn fork_args(&self, session: &str, directory: &str) -> Option<Vec<OsString>> {
        self.fork.as_ref().map(|template| {
            substitute(
                template,
                &[("{session}", session), ("{directory}", directory)],
            )
        })
    }

    /// The arguments of a one-shot review with `fields`; `None` when the
    /// tool has no review template.
    pub fn review_args(&self, fields: &ReviewFields) -> Option<Vec<OsString>> {
        let template = self.review.as_ref()?;
        let (schema, output) = (
            fields.schema.to_string_lossy(),
            fields.output.to_string_lossy(),
        );
        Some(substitute(
            template,
            &[
                ("{prompt}", fields.prompt),
                ("{schema}", &schema),
                ("{schema_json}", fields.schema_json),
                ("{output}", &output),
            ],
        ))
    }

    /// Whether the review's result goes to `{output}` rather than standard
    /// output.
    pub fn review_writes_output(&self) -> bool {
        self.review
            .iter()
            .flatten()
            .any(|arg| arg.contains("{output}"))
    }

    /// The arguments that give the agent `directory` as another working
    /// directory; none when it takes no such arguments.
    pub fn add_dir_args(&self, directory: &Path) -> Vec<OsString> {
        let Some(template) = &self.add_dir else {
            return Vec::new();
        };
        // Placeholders render into text, which a path need not be.
        let Some(directory) = directory.to_str() else {
            return Vec::new();
        };
        substitute(template, &[("{directory}", directory)])
    }
}

/// Each argument with every placeholder in `fields` replaced once.
fn substitute(template: &[String], fields: &[(&str, &str)]) -> Vec<OsString> {
    template
        .iter()
        .map(|arg| crate::config::templates::render(arg, fields).into())
        .collect()
}

/// A review's command: the launcher's words before `{args}`, or all of them
/// when it has none, followed by the review's arguments.
pub fn review_command(launcher: &[String], review: Vec<OsString>) -> Vec<OsString> {
    launcher
        .iter()
        .take_while(|word| word.as_str() != "{args}")
        .map(OsString::from)
        .chain(review)
        .collect()
}
