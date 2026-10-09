//! Open issues and PRs, for choosing one interactively.
use anyhow::{Context, Result, ensure};

use super::{ForgeKind, ForgeRepo, Query, strip_bidi_isolates};

/// An open issue or PR.
#[derive(Debug, PartialEq, serde::Deserialize)]
pub(crate) struct Item {
    pub number: u64,
    pub title: String,
}

impl ForgeRepo {
    pub async fn open_issues(&self, path: &std::path::Path) -> Result<Vec<Item>> {
        self.kind.open_items(self, path, "issue").await
    }

    pub async fn open_pulls(&self, path: &std::path::Path) -> Result<Vec<Item>> {
        self.kind.open_items(self, path, "pr").await
    }
}

impl ForgeKind {
    /// `noun` is the `issue` or `pr` subcommand both CLIs share.
    async fn open_items(
        self,
        repo: &ForgeRepo,
        path: &std::path::Path,
        noun: &str,
    ) -> Result<Vec<Item>> {
        if self == Self::GitHub {
            let repo = format!("{}/{}", repo.host, repo.path);
            let args = [
                noun,
                "list",
                "--repo",
                &repo,
                "--state",
                "open",
                "--limit",
                "200",
                "--json",
                "number,title",
            ];
            let text = self.query(path, &args, Query::List).await?;
            serde_json::from_str(&text).with_context(|| format!("invalid gh {noun} list response"))
        } else {
            let args = [
                "--style", "minimal", noun, "search", "--host", &repo.host, "--repo", &repo.path,
                "--state", "open",
            ];
            fj_items(&self.query(path, &args, Query::List).await?)
        }
    }
}

/// fj's minimal search prints a count, then `#<number>: <title> (by <user>)`.
fn fj_items(text: &str) -> Result<Vec<Item>> {
    strip_bidi_isolates(text)
        .lines()
        .filter_map(|line| line.strip_prefix('#'))
        .map(|line| {
            let item = line
                .split_once(": ")
                .and_then(|(number, rest)| {
                    let (title, _) = rest.rsplit_once(" (by ")?;
                    Some(Item {
                        number: number.parse().ok()?,
                        title: title.to_owned(),
                    })
                })
                .context("unrecognized fj search output")?;
            ensure!(item.number > 0, "unrecognized fj search output");
            Ok(item)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fj_search_reads_numbers_and_titles() {
        let text = "\u{2068}2\u{2069} issues\n\
            #\u{2068}498\u{2069}: \u{2068}Add: issue (by) options\u{2069} (by \u{2068}HN05\u{2069})\n\
            #\u{2068}2\u{2069}: \u{2068}Sandbox\u{2069} (by \u{2068}user\u{2069})\n";
        assert_eq!(
            fj_items(text).unwrap(),
            [
                Item {
                    number: 498,
                    title: "Add: issue (by) options".into()
                },
                Item {
                    number: 2,
                    title: "Sandbox".into()
                },
            ]
        );
        assert_eq!(fj_items("0 issues\n").unwrap(), []);
        for text in [
            "#x: title (by user)\n",
            "#0: title (by user)\n",
            "#7 title\n",
        ] {
            assert!(fj_items(text).is_err(), "{text}");
        }
    }
}
