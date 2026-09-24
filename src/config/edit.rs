//! Small, format-preserving edits shared by global and saved repository config.
use anyhow::{Context, Result, ensure};
use toml_edit::{DocumentMut, Item, Key, Table, Value};

pub fn edit(text: &str, key: &str, value: Option<&str>) -> Result<String> {
    let keys = Key::parse(key).context("expected a TOML dotted key")?;
    let (last, parents) = keys.split_last().context("config key cannot be empty")?;
    let mut document: DocumentMut = text.parse().context("parse existing config")?;
    let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
    for key in parents {
        if !table.contains_key(key.get()) && value.is_some() {
            let mut child = Table::new();
            child.set_implicit(true);
            table.insert(key.get(), Item::Table(child));
        }
        table = table
            .get_mut(key.get())
            .and_then(Item::as_table_like_mut)
            .with_context(|| format!("config key {} is missing or is not a table", key.get()))?;
    }
    if let Some(raw) = value {
        // Shell-friendly strings; arrays, booleans and numbers retain TOML types.
        let mut value = raw.parse::<Value>().unwrap_or_else(|_| Value::from(raw));
        if let Some(previous) = table.get(last.get()).and_then(Item::as_value) {
            *value.decor_mut() = previous.decor().clone();
        }
        if let Some(existing) = table.get_mut(last.get()) {
            *existing = Item::Value(value);
        } else {
            table.insert(last.get(), Item::Value(value));
        }
    } else {
        ensure!(
            table.remove(last.get()).is_some(),
            "config key {key} is not set in this layer"
        );
    }
    Ok(document.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_dotted_and_quoted_keys_without_losing_comments() {
        let source = "# policy\n[auto_cleanup]\nenabled = true # keep\nidle_minutes = 30\n";
        let result = edit(source, "auto_cleanup.enabled", Some("false")).unwrap();
        assert_eq!(result, source.replace("true", "false"));
        let result = edit(&result, "commands.\"test.unit\"", Some("['cargo', 'test']")).unwrap();
        let parsed: toml::Value = toml::from_str(&result).unwrap();
        assert_eq!(parsed["commands"]["test.unit"][0].as_str(), Some("cargo"));
        let result = edit(&result, "commands.\"test.unit\"", None).unwrap();
        assert!(!result.contains("test.unit"));
        assert!(result.contains("# keep"));
    }

    #[test]
    fn handles_inline_tables_and_refuses_to_traverse_scalars() {
        let result = edit(
            "codex = { default_mode = 'app' }\n",
            "codex.default_mode",
            Some("cli"),
        )
        .unwrap();
        let parsed: toml::Value = toml::from_str(&result).unwrap();
        assert_eq!(parsed["codex"]["default_mode"].as_str(), Some("cli"));
        assert!(edit("default_agent = 'codex'", "default_agent.mode", Some("app")).is_err());
        assert!(edit("", "missing", None).is_err());
        assert!(edit("", "x..y", Some("1")).is_err());
    }
}
