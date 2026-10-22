//! What a repository asks of its issues and PRs: labels its configuration
//! requires. Shoal checks them where agents open items, so the repository's
//! conventions reach agents at the moment they apply.
use anyhow::{Result, bail};

use super::{ForgeRepo, account::Account, labels::Label, link::ItemKind};

/// Refuse a new item of `kind` whose labels leave a required entry unmet,
/// naming the repository's labels that would meet it.
pub(crate) async fn require(
    forge: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    required: &[String],
    labels: &[String],
) -> Result<()> {
    let missing = missing(required, labels);
    if missing.is_empty() {
        return Ok(());
    }
    // The refusal stands without the choices when they cannot be read.
    let available = forge.labels(account).await.unwrap_or_default();
    bail!(refusal(kind, &missing, &available))
}

/// The required entries none of `labels` satisfies; a label satisfies an
/// entry it starts with, so `area/` takes any area and `bug` only itself.
fn missing<'a>(required: &'a [String], labels: &[String]) -> Vec<&'a str> {
    required
        .iter()
        .filter(|entry| !labels.iter().any(|label| label.starts_with(entry.as_str())))
        .map(String::as_str)
        .collect()
}

/// Why a new item of `kind` is refused, with the repository's labels that
/// would satisfy each missing entry.
fn refusal(kind: ItemKind, missing: &[&str], available: &[Label]) -> String {
    let mut message = format!(
        "{} needs a label starting with {} ([{kind}] required_labels); pass --label",
        new_item(kind),
        missing.join(", ")
    );
    let choices: Vec<&Label> = available
        .iter()
        .filter(|label| missing.iter().any(|entry| label.name.starts_with(entry)))
        .collect();
    if !choices.is_empty() {
        message.push_str(" with one of:");
        for label in choices {
            message.push_str(&format!("\n  {}", described(label)));
        }
    }
    message
}

fn new_item(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Issue => "a new issue",
        ItemKind::Pr => "a new PR",
    }
}

fn described(label: &Label) -> String {
    match label.description.as_str() {
        "" => label.name.clone(),
        description => format!("{}: {description}", label.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(name: &str, description: &str) -> Label {
        Label {
            name: name.into(),
            description: description.into(),
            exclusive: false,
        }
    }

    #[test]
    fn required_entries_are_label_prefixes() {
        let required = vec!["area/".to_owned(), "type/".to_owned(), "bug".to_owned()];
        assert_eq!(
            missing(&required, &["area/cli".into(), "bugfix".into()]),
            ["type/"]
        );
        assert_eq!(missing(&required, &[]), ["area/", "type/", "bug"]);
        assert!(missing(&[], &[]).is_empty());
    }

    #[test]
    fn refusals_offer_the_labels_that_satisfy_each_missing_entry() {
        let available = [
            label("area/cli", "Command line"),
            label("type/bug", ""),
            label("tracking", "Standing issue"),
        ];
        assert_eq!(
            refusal(ItemKind::Pr, &["area/", "type/"], &available),
            "a new PR needs a label starting with area/, type/ ([pr] required_labels); \
             pass --label with one of:\n  area/cli: Command line\n  type/bug"
        );
        assert_eq!(
            refusal(ItemKind::Issue, &["area/"], &[]),
            "a new issue needs a label starting with area/ ([issue] required_labels); pass --label"
        );
    }
}
