//! The Git settings a command hands the Git it runs through the
//! `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_<n>` and `GIT_CONFIG_VALUE_<n>`
//! variables.
use std::ffi::{OsStr, OsString};

/// Pass `settings` to `command`'s Git after the ones this process inherited,
/// so they take precedence over every Git config file. Inherited author and
/// committer variables would override an identity in `settings`, so they are
/// dropped for each identity setting it names.
pub fn append(command: &mut std::process::Command, settings: &[(String, String)]) {
    if settings.is_empty() {
        return;
    }
    let inherited = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|count| count.parse::<usize>().ok())
        .unwrap_or(0);
    for (index, (key, value)) in settings.iter().enumerate() {
        let index = inherited + index;
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
        for field in ["name", "email"] {
            if key.eq_ignore_ascii_case(&format!("user.{field}")) {
                let field = field.to_ascii_uppercase();
                command
                    .env_remove(format!("GIT_AUTHOR_{field}"))
                    .env_remove(format!("GIT_COMMITTER_{field}"));
            }
        }
    }
    command.env("GIT_CONFIG_COUNT", (inherited + settings.len()).to_string());
}

/// Keep only the Git settings `command` passes on that `keep` accepts, as
/// `(key, value)`; the others keep their order.
pub fn retain(command: &mut tokio::process::Command, keep: impl Fn(&(OsString, OsString)) -> bool) {
    let entries = passed_on(command);
    let kept: Vec<_> = entries.iter().filter(|entry| keep(entry)).collect();
    if kept.len() == entries.len() {
        return;
    }
    for index in 0..entries.len() {
        command
            .env_remove(format!("GIT_CONFIG_KEY_{index}"))
            .env_remove(format!("GIT_CONFIG_VALUE_{index}"));
    }
    for (index, (key, value)) in kept.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    command.env("GIT_CONFIG_COUNT", kept.len().to_string());
}

/// The Git settings `command` passes on, as `(key, value)`: those it sets,
/// else those this process inherited.
pub fn passed_on(command: &tokio::process::Command) -> Vec<(OsString, OsString)> {
    let set: Vec<(OsString, Option<OsString>)> = command
        .as_std()
        .get_envs()
        .map(|(name, value)| (name.to_owned(), value.map(OsStr::to_owned)))
        .collect();
    let read = |name: &str| match set.iter().find(|(set, _)| set == name) {
        Some((_, value)) => value.clone(),
        None => std::env::var_os(name),
    };
    let count = read("GIT_CONFIG_COUNT")
        .and_then(|count| count.into_string().ok())
        .and_then(|count| count.parse::<usize>().ok())
        .unwrap_or(0);
    (0..count)
        .map(|index| {
            (
                read(&format!("GIT_CONFIG_KEY_{index}")).unwrap_or_default(),
                read(&format!("GIT_CONFIG_VALUE_{index}")).unwrap_or_default(),
            )
        })
        .collect()
}
