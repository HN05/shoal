//! Secrets that user-configured commands fetch for tracked agents. Shoal keeps
//! the commands, never the values.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Stdio,
};

use crate::{env, paths::Paths};

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Secret {
    /// Argument array whose standard output is the value.
    pub command: Vec<String>,
    /// Programs that receive the value each time they run; empty gives it to
    /// the agent's environment instead.
    pub tools: Vec<String>,
}

/// Configured secrets keyed by the variable that carries each value.
pub type Secrets = BTreeMap<String, Secret>;

/// The tool-scoped secrets a launcher directory serves, beside its launchers.
/// Tool names cannot start with a dot, so no launcher can take its name.
const MANIFEST: &str = ".secrets.json";

pub fn validate(secrets: &Secrets) -> Result<()> {
    for (name, secret) in secrets {
        env::validate_configured_name("[secrets]", name)?;
        ensure!(
            !env::PROTECTED.contains(&name.as_str()),
            "[secrets] cannot set {name}"
        );
        ensure!(
            secret
                .command
                .first()
                .is_some_and(|program| !program.is_empty()),
            "secrets.{name}.command must name a program"
        );
        ensure!(
            secret.command.iter().all(|word| !word.contains('\0')),
            "secrets.{name}.command contains NUL"
        );
        for tool in &secret.tools {
            ensure!(
                !tool.is_empty() && !tool.starts_with('.') && !tool.contains(['/', '\0']),
                "secrets.{name}.tools: {tool:?} is not a program name"
            );
        }
    }
    Ok(())
}

/// The secret variables an agent launch adds to its child's environment, and
/// the launchers that give tool-scoped secrets to their tools.
#[derive(Debug, Default)]
pub struct Launch {
    environment: Vec<(String, OsString)>,
    launchers: Option<tempfile::TempDir>,
}

impl Launch {
    /// Fetch every environment secret now, so a failing command stops the
    /// launch before the agent starts. Tool-scoped secrets are fetched each
    /// time their tool runs.
    pub async fn prepare(secrets: &Secrets, paths: &Paths) -> Result<Self> {
        validate(secrets)?;
        let (scoped, unscoped): (Secrets, Secrets) = secrets
            .clone()
            .into_iter()
            .partition(|(_, secret)| !secret.tools.is_empty());
        let mut environment = Vec::new();
        for (name, secret) in &unscoped {
            environment.push((name.clone(), fetch(name, secret, None).await?));
        }
        let launchers = if scoped.is_empty() {
            None
        } else {
            Some(write_launchers(&scoped, paths)?)
        };
        Ok(Self {
            environment,
            launchers,
        })
    }

    /// Put the launchers first on the child's PATH, ahead of directories an
    /// earlier step prepended.
    pub fn apply(&self, command: &mut tokio::process::Command) {
        if !self.environment.is_empty() {
            for (name, value) in &self.environment {
                command.env(name, value);
            }
            command.env(
                env::SECRET_ENV,
                exported_names(self.environment.iter().map(|(name, _)| name)),
            );
        }
        if let Some(launchers) = &self.launchers {
            let inherited = command
                .as_std()
                .get_envs()
                .find(|(name, _)| *name == "PATH")
                .and_then(|(_, value)| value.map(OsStr::to_owned))
                .or_else(|| std::env::var_os("PATH"))
                .unwrap_or_default();
            let prepend = |list: &OsStr| {
                std::env::join_paths(std::iter::once(launchers.path().to_owned()).chain(
                    std::env::split_paths(list).filter(|entry| !entry.as_os_str().is_empty()),
                ))
            };
            // `prepare` rejects a launcher directory that cannot join a PATH.
            if let (Ok(path), Ok(directories)) = (
                prepend(&inherited),
                prepend(&std::env::var_os(env::SECRET_TOOLS).unwrap_or_default()),
            ) {
                command
                    .env("PATH", path)
                    .env(env::SECRET_TOOLS, directories);
            }
        }
    }
}

/// One executable per tool that hands its arguments to the secret tool
/// worker, beside the manifest it reads.
fn write_launchers(secrets: &Secrets, paths: &Paths) -> Result<tempfile::TempDir> {
    let directory = paths.secret_tools_directory()?;
    std::env::join_paths([directory.path()]).with_context(|| {
        format!(
            "secret tool launchers cannot go on PATH from {}",
            directory.path().display()
        )
    })?;
    std::fs::write(
        directory.path().join(MANIFEST),
        serde_json::to_vec(secrets)?,
    )?;
    let tools: BTreeSet<_> = secrets.values().flat_map(|secret| &secret.tools).collect();
    for tool in tools {
        let words = crate::cli::internal::internal_command(
            paths,
            false,
            crate::cli::internal::Worker::SecretTool {
                launchers: directory.path(),
                tool,
            },
        )?
        .into_iter()
        .map(|word| {
            word.into_string()
                .map_err(|word| anyhow::anyhow!("{} is not UTF-8", word.to_string_lossy()))
        })
        .collect::<Result<Vec<_>>>()?;
        let invocation = crate::shell::quote(&words);
        let path = directory.path().join(tool);
        std::fs::write(&path, format!("#!/bin/sh\nexec {invocation} \"$@\"\n"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(directory)
}

/// Run `tool` with the secrets configured for it in place of the launcher
/// in `launchers`: the next `tool` on PATH, which may be an enclosing
/// launch's launcher.
pub async fn run_tool(launchers: &Path, tool: &str, args: Vec<OsString>) -> Result<i32> {
    use std::os::unix::process::CommandExt;
    let manifest = launchers.join(MANIFEST);
    let secrets: Secrets = serde_json::from_slice(
        &std::fs::read(&manifest).with_context(|| format!("read {}", manifest.display()))?,
    )
    .with_context(|| format!("parse {}", manifest.display()))?;
    let search = path_after(&std::env::var_os("PATH").unwrap_or_default(), launchers)?;
    let program = crate::fsutil::find_executable(OsStr::new(tool), &search)
        .with_context(|| format!("{tool} is not on PATH"))?;
    let mut command = std::process::Command::new(&program);
    command.args(args);
    let mut names = Vec::new();
    for (name, secret) in &secrets {
        if secret.tools.iter().any(|candidate| candidate == tool) {
            command.env(name, fetch(name, secret, Some(&search)).await?);
            names.push(name);
        }
    }
    command.env(env::SECRET_ENV, exported_names(names.into_iter()));
    Err(command.exec()).with_context(|| format!("run {}", program.display()))
}

/// The PATH entries after `launchers`, or every other entry when PATH does
/// not hold it. An enclosing launch's launchers come later on PATH, so this
/// never leads back to an inner launcher for the same tool.
fn path_after(path: &OsStr, launchers: &Path) -> Result<OsString> {
    let entries: Vec<_> = std::env::split_paths(path).collect();
    let start = entries
        .iter()
        .position(|entry| env::same_directory(entry, launchers))
        .map_or(0, |index| index + 1);
    Ok(std::env::join_paths(
        entries[start..]
            .iter()
            .filter(|entry| !env::same_directory(entry, launchers)),
    )?)
}

/// [`env::SECRET_ENV`] after adding `names` to the inherited ones.
fn exported_names<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let mut all = env::inherited_secret_exports();
    all.extend(names.cloned());
    all.sort();
    all.dedup();
    all.join(":")
}

/// Run the secret's command without input, so it never consumes the
/// launching terminal's or a tool's; its diagnostics reach the caller's
/// stderr. `path` replaces PATH for it. One trailing line break is the
/// command's formatting, not the value's.
async fn fetch(name: &str, secret: &Secret, path: Option<&OsStr>) -> Result<OsString> {
    use std::os::unix::ffi::OsStringExt;
    let mut command = tokio::process::Command::new(&secret.command[0]);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let output = command
        .args(&secret.command[1..])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .await
        .with_context(|| format!("secrets.{name}: run {}", secret.command[0]))?;
    ensure!(
        output.status.success(),
        "secrets.{name}: {} exited with {}",
        secret.command[0],
        output.status
    );
    let mut value = output.stdout;
    if value.ends_with(b"\n") {
        value.pop();
        if value.ends_with(b"\r") {
            value.pop();
        }
    }
    ensure!(
        !value.is_empty(),
        "secrets.{name}: the command printed nothing"
    );
    ensure!(
        !value.contains(&0),
        "secrets.{name}: the value contains NUL"
    );
    Ok(OsString::from_vec(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(command: &[&str]) -> Secret {
        Secret {
            command: command.iter().map(|word| word.to_string()).collect(),
            tools: Vec::new(),
        }
    }

    #[test]
    fn names_and_commands_are_validated() {
        for (name, command) in [
            ("TOKEN", &[][..]),
            ("TOKEN", &[""][..]),
            ("TOKEN", &["printf", "a\0b"][..]),
            ("1TOKEN", &["true"][..]),
            ("SHOAL_TOKEN", &["true"][..]),
            ("PATH", &["true"][..]),
        ] {
            let secrets = Secrets::from([(name.into(), secret(command))]);
            assert!(validate(&secrets).is_err(), "{name} {command:?}");
        }
        for tool in ["", ".secrets.json", "bin/tool", "a\0b"] {
            let mut scoped = secret(&["true"]);
            scoped.tools = vec![tool.into()];
            let secrets = Secrets::from([("TOKEN".into(), scoped)]);
            assert!(validate(&secrets).is_err(), "{tool:?}");
        }
        let mut scoped = secret(&["true"]);
        scoped.tools = vec!["agent-browser".into()];
        assert!(validate(&Secrets::from([("TOKEN".into(), scoped)])).is_ok());
    }

    #[tokio::test]
    async fn fetch_strips_one_line_break_and_rejects_failures() {
        let value = fetch("TOKEN", &secret(&["printf", "a b\\n\\n"]), None)
            .await
            .unwrap();
        assert_eq!(value, "a b\n");
        let value = fetch("TOKEN", &secret(&["printf", "value\\r\\n"]), None)
            .await
            .unwrap();
        assert_eq!(value, "value");
        for command in [&["false"][..], &["printf", "\\n"], &["printf", "a\\0b"]] {
            assert!(
                fetch("TOKEN", &secret(command), None).await.is_err(),
                "{command:?}"
            );
        }
    }

    #[tokio::test]
    async fn launchers_that_cannot_join_path_stop_the_launch() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(root.path().join("a:b"));
        std::fs::create_dir_all(&paths.state).unwrap();
        let mut scoped = secret(&["true"]);
        scoped.tools = vec!["agent-browser".into()];
        let error = Launch::prepare(&Secrets::from([("TOKEN".into(), scoped)]), &paths)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("cannot go on PATH"),
            "{error:#}"
        );
    }

    #[test]
    fn launchers_search_only_later_path_entries() {
        let path = OsStr::new("/inner:/outer:/bin");
        assert_eq!(
            path_after(path, Path::new("/inner")).unwrap(),
            "/outer:/bin"
        );
        assert_eq!(path_after(path, Path::new("/outer")).unwrap(), "/bin");
        assert_eq!(
            path_after(path, Path::new("/elsewhere")).unwrap(),
            "/inner:/outer:/bin"
        );
    }

    #[tokio::test]
    async fn apply_records_the_names_hooks_drop() {
        let launch = Launch {
            environment: vec![
                ("B_TOKEN".into(), "b".into()),
                ("A_TOKEN".into(), "a".into()),
            ],
            launchers: None,
        };
        let mut command = tokio::process::Command::new("true");
        launch.apply(&mut command);
        let envs: BTreeMap<_, _> = command
            .as_std()
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned)))
            .collect();
        assert_eq!(envs[std::ffi::OsStr::new("A_TOKEN")], Some("a".into()));
        let names = envs[std::ffi::OsStr::new(crate::env::SECRET_ENV)]
            .clone()
            .unwrap();
        let names: Vec<_> = names.to_str().unwrap().split(':').collect();
        assert!(names.contains(&"A_TOKEN") && names.contains(&"B_TOKEN"));
    }
}
