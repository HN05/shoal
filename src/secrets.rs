//! Secrets that user-configured commands fetch for tracked agents. Shoal keeps
//! the commands, never the values.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ffi::OsString, process::Stdio};

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Secret {
    /// Argument array whose standard output is the value.
    pub command: Vec<String>,
}

/// Configured secrets keyed by the variable that carries each value.
pub type Secrets = BTreeMap<String, Secret>;

pub fn validate(secrets: &Secrets) -> Result<()> {
    for (name, secret) in secrets {
        crate::env::validate_configured_name("[secrets]", name)?;
        ensure!(
            !crate::env::PROTECTED.contains(&name.as_str()),
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
    }
    Ok(())
}

/// The secret variables an agent launch adds to its child's environment.
#[derive(Debug, Default)]
pub struct Launch {
    environment: Vec<(String, OsString)>,
}

impl Launch {
    /// Fetch every secret now, so a failing command stops the launch before
    /// the agent starts.
    pub async fn prepare(secrets: &Secrets) -> Result<Self> {
        validate(secrets)?;
        let mut environment = Vec::new();
        for (name, secret) in secrets {
            environment.push((name.clone(), fetch(name, secret).await?));
        }
        Ok(Self { environment })
    }

    /// Hooks started beneath the agent drop the names recorded in
    /// [`crate::env::SECRET_ENV`], including those of an enclosing launch.
    pub fn apply(&self, command: &mut tokio::process::Command) {
        if self.environment.is_empty() {
            return;
        }
        let mut names = crate::env::inherited_secret_exports();
        for (name, value) in &self.environment {
            command.env(name, value);
            names.push(name.clone());
        }
        names.sort();
        names.dedup();
        command.env(crate::env::SECRET_ENV, names.join(":"));
    }
}

/// Run the secret's command without input, so it never consumes the
/// launching terminal's; its diagnostics reach the caller's stderr. One
/// trailing line break is the command's formatting, not the value's.
async fn fetch(name: &str, secret: &Secret) -> Result<OsString> {
    use std::os::unix::ffi::OsStringExt;
    let output = tokio::process::Command::new(&secret.command[0])
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
        assert!(validate(&Secrets::from([("TOKEN".into(), secret(&["true"]))])).is_ok());
    }

    #[tokio::test]
    async fn fetch_strips_one_line_break_and_rejects_failures() {
        let value = fetch("TOKEN", &secret(&["printf", "a b\\n\\n"]))
            .await
            .unwrap();
        assert_eq!(value, "a b\n");
        let value = fetch("TOKEN", &secret(&["printf", "value\\r\\n"]))
            .await
            .unwrap();
        assert_eq!(value, "value");
        for command in [&["false"][..], &["printf", "\\n"], &["printf", "a\\0b"]] {
            assert!(
                fetch("TOKEN", &secret(command)).await.is_err(),
                "{command:?}"
            );
        }
    }

    #[tokio::test]
    async fn apply_records_the_names_hooks_drop() {
        let launch = Launch {
            environment: vec![
                ("B_TOKEN".into(), "b".into()),
                ("A_TOKEN".into(), "a".into()),
            ],
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
