use clap::{Args, ValueEnum};

use crate::{cli::args::item_reference, model::Relation};

// An optional item selection followed by its relation.
#[derive(Debug, Args)]
pub struct RelationArgs {
    /// pr/issue, a number or URL, then assigned, opened or related.
    #[arg(required = true, num_args = 1..=3, value_name = "[ITEM] RELATION")]
    pub args: Vec<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

impl RelationArgs {
    /// Split the trailing relation from the item selection before it.
    pub fn split(self) -> anyhow::Result<(ItemArgs, Relation)> {
        let mut args = self.args;
        let last = args.pop().unwrap_or_default();
        let relation = Relation::from_str(&last, false).map_err(|_| {
            anyhow::anyhow!("end with a relation: assigned, opened or related (got {last})")
        })?;
        let mut items = args
            .into_iter()
            .map(|arg| item_reference(&arg).unwrap_or_else(|never| match never {}));
        let items = ItemArgs {
            kind_or_url: items.next(),
            item: items.next(),
            workspace: self.workspace,
        };
        Ok((items, relation))
    }
}

#[derive(Debug, Args)]
pub struct ItemArgs {
    /// pr/issue to select a kind, or an issue or PR number or URL.
    #[arg(value_parser = item_reference)]
    pub kind_or_url: Option<String>,
    /// Number or URL when a kind is supplied.
    #[arg(requires = "kind_or_url", value_parser = item_reference)]
    pub item: Option<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}
