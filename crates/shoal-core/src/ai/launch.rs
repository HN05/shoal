//! What an AI tool's launcher takes beyond its command: argument templates
//! and changes to its environment.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::Path,
};

use crate::agent::ModelChoice;
use shoal_ids::agent_session_ids::AgentSessionId;

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

    /// These changes with `variable` also set to `effort`, when both are
    /// given.
    pub fn with_effort(&self, variable: Option<&str>, effort: Option<&str>) -> Self {
        let mut environment = self.clone();
        if let (Some(variable), Some(effort)) = (variable, effort) {
            environment.remove.remove(variable);
            environment
                .set
                .insert(variable.to_owned(), effort.to_owned());
        }
        environment
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
    pub model_args: Option<Vec<String>>,
    pub effort_args: Option<Vec<String>>,
    pub review: Option<Vec<String>>,
    pub review_resume: Option<Vec<String>>,
    pub gate: Option<Vec<String>>,
    pub gate_resume: Option<Vec<String>>,
}

/// What a one-shot run's arguments name.
pub struct ReviewFields<'a> {
    pub prompt: &'a str,
    /// The output schema of a run that returns its result in one, which
    /// only gate templates name.
    pub schema: Option<Schema<'a>>,
    /// The file the result goes to when the arguments name it.
    pub output: &'a Path,
}

/// An output schema, as a file and as its text.
#[derive(Clone, Copy)]
pub struct Schema<'a> {
    pub file: &'a Path,
    pub json: &'a str,
}

impl Arguments {
    /// The arguments that select `choice`'s model and effort; the error names
    /// the template missing for a value it sets.
    pub fn model_args(&self, choice: &ModelChoice) -> Result<Vec<OsString>, &'static str> {
        let mut args = Vec::new();
        for (value, template, name, field) in [
            (&choice.model, &self.model_args, "model_args", "{model}"),
            (&choice.effort, &self.effort_args, "effort_args", "{effort}"),
        ] {
            if let Some(value) = value {
                let template = template.as_ref().ok_or(name)?;
                args.extend(substitute(template, &[(field, value)]));
            }
        }
        Ok(args)
    }

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
    pub fn resume_args(&self, session: Option<&AgentSessionId>) -> Option<Vec<OsString>> {
        match session {
            Some(session) => self
                .resume
                .as_ref()
                .map(|template| substitute(template, &[("{session}", session.as_str())])),
            None => self
                .resume_latest
                .as_ref()
                .map(|template| substitute(template, &[])),
        }
    }

    /// The arguments that start a new session in `directory` from a fork
    /// of `session`; `None` when the agent cannot fork one.
    pub fn fork_args(&self, session: &AgentSessionId, directory: &str) -> Option<Vec<OsString>> {
        self.fork.as_ref().map(|template| {
            substitute(
                template,
                &[("{session}", session.as_str()), ("{directory}", directory)],
            )
        })
    }

    /// The arguments of a one-shot review with `fields`; `None` when the
    /// tool has no review template.
    pub fn review_args(&self, fields: &ReviewFields) -> Option<Vec<OsString>> {
        Some(oneshot_args(self.review.as_ref()?, fields, None))
    }

    /// The arguments of a one-shot review with `fields` that resumes
    /// `session`; `None` when the tool cannot resume one.
    pub fn review_resume_args(
        &self,
        fields: &ReviewFields,
        session: &AgentSessionId,
    ) -> Option<Vec<OsString>> {
        Some(oneshot_args(
            self.review_resume.as_ref()?,
            fields,
            Some(session),
        ))
    }

    /// The arguments of a gate run with `fields`; `None` when the tool has
    /// no gate template.
    pub fn gate_args(&self, fields: &ReviewFields) -> Option<Vec<OsString>> {
        Some(oneshot_args(self.gate.as_ref()?, fields, None))
    }

    /// The arguments of a gate run with `fields` that resumes `session`;
    /// `None` when the tool cannot resume one.
    pub fn gate_resume_args(
        &self,
        fields: &ReviewFields,
        session: &AgentSessionId,
    ) -> Option<Vec<OsString>> {
        Some(oneshot_args(
            self.gate_resume.as_ref()?,
            fields,
            Some(session),
        ))
    }

    /// Whether the review's result goes to `{output}` rather than standard
    /// output.
    pub fn review_writes_output(&self) -> bool {
        names_output(self.review.as_deref())
    }

    /// Whether a resumed review's result goes to `{output}` rather than
    /// standard output.
    pub fn review_resume_writes_output(&self) -> bool {
        names_output(self.review_resume.as_deref())
    }

    /// Whether a gate run's result goes to `{output}` rather than standard
    /// output.
    pub fn gate_writes_output(&self) -> bool {
        names_output(self.gate.as_deref())
    }

    /// Whether a resumed gate run's result goes to `{output}` rather than
    /// standard output.
    pub fn gate_resume_writes_output(&self) -> bool {
        names_output(self.gate_resume.as_deref())
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

/// Whether `template` names the output file `{output}`.
fn names_output(template: Option<&[String]>) -> bool {
    template
        .into_iter()
        .flatten()
        .any(|arg| arg.contains("{output}"))
}

/// A one-shot run's arguments from `template`, which names `fields` and the
/// `session` it resumes.
fn oneshot_args(
    template: &[String],
    fields: &ReviewFields,
    session: Option<&AgentSessionId>,
) -> Vec<OsString> {
    let output = fields.output.to_string_lossy();
    let mut named = vec![
        ("{prompt}", fields.prompt),
        ("{output}", &output),
        ("{session}", session.map_or("", AgentSessionId::as_str)),
    ];
    let schema = fields.schema.map(|schema| schema.file.to_string_lossy());
    if let (Some(file), Some(schema)) = (&schema, fields.schema) {
        named.extend([("{schema}", &**file), ("{schema_json}", schema.json)]);
    }
    substitute(template, &named)
}

/// Each argument with every placeholder in `fields` replaced once.
fn substitute(template: &[String], fields: &[(&str, &str)]) -> Vec<OsString> {
    template
        .iter()
        .map(|arg| crate::config::templates::render(arg, fields).into())
        .collect()
}

/// A one-shot run's command: the launcher's words before `{args}`, or all of
/// them when it has none, followed by the run's arguments.
pub fn oneshot_command(launcher: &[String], args: Vec<OsString>) -> Vec<OsString> {
    launcher
        .iter()
        .take_while(|word| word.as_str() != "{args}")
        .map(OsString::from)
        .chain(args)
        .collect()
}
