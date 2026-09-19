//! Plain-text templates: substitute known fields once, never evaluate their values.
use std::{fs, io::Write, path::Path};

use anyhow::{Context, Result};

use crate::{config::Config, paths::Paths};

pub const ISSUE_FILE: &str = "issue-template.md";
pub const ISSUE_DEFAULT: &str = include_str!("../issue-template.md");

pub fn read(directory: &Path, name: &str) -> Result<Option<String>> {
    let path = directory.join(name);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

pub fn install(paths: &Paths) -> Result<()> {
    let config = Config::path(paths);
    install_at(config.parent().context("config has no directory")?)
}

fn install_at(directory: &Path) -> Result<()> {
    fs::create_dir_all(directory)?;
    for (name, contents) in [(ISSUE_FILE, ISSUE_DEFAULT)] {
        let path = directory.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => file.write_all(contents.as_bytes())?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).with_context(|| format!("create {}", path.display())),
        }
    }
    Ok(())
}

pub fn render(template: &str, fields: &[(&str, &str)]) -> String {
    let mut output = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        if let Some((key, value)) = fields.iter().find(|(key, _)| rest.starts_with(key)) {
            output.push_str(value);
            rest = &rest[key.len()..];
        } else {
            output.push('{');
            rest = &rest[1..];
        }
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_values_and_unknown_fields_stay_literal() {
        assert_eq!(
            render(
                "{title}: {body} {title} {unknown}",
                &[("{title}", "{body}"), ("{body}", "$(false)\n日本語")]
            ),
            "{body}: $(false)\n日本語 {body} {unknown}"
        );
    }

    #[test]
    fn install_seeds_missing_templates_and_preserves_edits() {
        let directory = tempfile::tempdir().unwrap();
        install_at(directory.path()).unwrap();
        let path = directory.path().join(ISSUE_FILE);
        assert_eq!(fs::read_to_string(&path).unwrap(), ISSUE_DEFAULT);
        fs::write(&path, "custom").unwrap();
        install_at(directory.path()).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "custom");
    }
}
