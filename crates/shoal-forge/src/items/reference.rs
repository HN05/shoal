//! An issue, PR or CI run, parsed once from the URL or number that names it
//! and passed on from there. Each serializes and stores as the text it was
//! parsed from, its canonical URL or the number.
use std::{fmt, hash, str::FromStr, sync::Arc};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    ForgeRepo, IssueInput,
    hosts::HostTypes,
    items::link::LinkKind,
    numbers::{IssueNumber, PullNumber, RunNumber},
};

/// An item's number in its repository, typed by the item's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemNumber {
    Issue(IssueNumber),
    Pr(PullNumber),
    /// The number a CI run's web URL holds.
    Run(RunNumber),
}

impl ItemNumber {
    pub fn kind(self) -> LinkKind {
        match self {
            Self::Issue(_) => LinkKind::Issue,
            Self::Pr(_) => LinkKind::Pr,
            Self::Run(_) => LinkKind::Ci,
        }
    }

    pub fn get(self) -> u64 {
        match self {
            Self::Issue(number) => number.get(),
            Self::Pr(number) => number.get(),
            Self::Run(number) => number.get(),
        }
    }
}

/// An issue, PR or CI run of a repository, read from its URL. Two are the
/// same item when their canonical URLs are. Copies share what was read.
#[derive(Debug, Clone)]
pub struct ItemRef(Arc<Parsed>);

/// What an [`ItemRef`] read from its URL.
#[derive(Debug)]
struct Parsed {
    repository: ForgeRepo,
    number: ItemNumber,
    url: String,
}

impl ItemRef {
    /// The `kind` item `url` names, read by the route its kind takes on any
    /// forge; a host Shoal has not resolved keeps the forge its route names.
    pub fn parse_kind(kind: LinkKind, url: &str) -> Result<Self> {
        ensure!(
            IssueInput::parse(url) == IssueInput::Url,
            "expected {} URL",
            article(kind)
        );
        let repository = ForgeRepo::from_item_url(kind, url)?;
        let (number, url) = repository.item(kind, url)?;
        Ok(Self::new(repository, kind, number, url))
    }

    /// The `kind` item `input` names in `repository`, where a number takes
    /// the repository's route and a URL must name one of its items.
    pub fn in_repository(
        repository: &ForgeRepo,
        kind: LinkKind,
        input: &ItemInput,
    ) -> Result<Self> {
        match input {
            ItemInput::Number(number) => {
                let url = repository.numbered_url(kind, *number)?;
                Ok(Self::new(repository.clone(), kind, *number, url))
            }
            ItemInput::Url(item) => {
                ensure!(item.kind() == kind, "{item} is not {}", article(kind));
                item.belongs_to(repository)?;
                Ok(item.clone())
            }
        }
    }

    /// The `kind` item numbered `number` in `repository`.
    pub fn numbered(repository: &ForgeRepo, kind: LinkKind, number: u64) -> Result<Self> {
        ensure!(number > 0, "item number must be positive");
        Self::in_repository(repository, kind, &ItemInput::Number(number))
    }

    fn new(repository: ForgeRepo, kind: LinkKind, number: u64, url: String) -> Self {
        let number = match kind {
            LinkKind::Issue => ItemNumber::Issue(IssueNumber::new(number)),
            LinkKind::Pr => ItemNumber::Pr(PullNumber::new(number)),
            LinkKind::Ci => ItemNumber::Run(RunNumber::new(number)),
        };
        Self(Arc::new(Parsed {
            repository,
            number,
            url,
        }))
    }

    pub fn kind(&self) -> LinkKind {
        self.0.number.kind()
    }

    pub fn number(&self) -> ItemNumber {
        self.0.number
    }

    /// Its number, refusing any item but an issue.
    pub fn issue(&self) -> Result<IssueNumber> {
        match self.0.number {
            ItemNumber::Issue(number) => Ok(number),
            _ => bail!("{} is not an issue", self.0.url),
        }
    }

    /// Its number, refusing any item but a PR.
    pub fn pull(&self) -> Result<PullNumber> {
        match self.0.number {
            ItemNumber::Pr(number) => Ok(number),
            _ => bail!("{} is not a PR", self.0.url),
        }
    }

    /// Its number, refusing any item but a CI run.
    pub fn run(&self) -> Result<RunNumber> {
        match self.0.number {
            ItemNumber::Run(number) => Ok(number),
            _ => bail!("{} is not a CI run", self.0.url),
        }
    }

    /// The repository the URL names, on the forge its route or host gave
    /// when it was read; [`forge`](Self::forge) asks the host.
    pub fn repository(&self) -> &ForgeRepo {
        &self.0.repository
    }

    pub fn url(&self) -> &str {
        &self.0.url
    }

    /// Refuse it unless it is an item of `repository`, as one recorded
    /// before the repository's origin changed is not.
    pub fn belongs_to(&self, repository: &ForgeRepo) -> Result<()> {
        ensure!(
            self.0.repository == *repository,
            "{self} belongs to a different repository than {}",
            repository.repository_url()
        );
        Ok(())
    }

    /// Whether it belongs to the repository `remote` names.
    pub fn in_remote(&self, remote: &str) -> bool {
        self.0.repository.is_remote(remote)
    }

    /// Its repository, on the forge `hosts` says its host serves.
    pub async fn forge(&self, hosts: &impl HostTypes) -> Result<ForgeRepo> {
        ForgeRepo::resolve(&self.0.repository.repository_url(), hosts).await
    }
}

/// `kind` with its article, as in `an issue`.
fn article(kind: LinkKind) -> String {
    match kind {
        LinkKind::Issue => "an issue".into(),
        kind => format!("a {}", kind.noun()),
    }
}

impl PartialEq for ItemRef {
    fn eq(&self, other: &Self) -> bool {
        self.0.url == other.0.url
    }
}

impl Eq for ItemRef {}

impl hash::Hash for ItemRef {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.0.url.hash(state);
    }
}

impl fmt::Display for ItemRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.url)
    }
}

/// The item a URL names, of whichever kind its route gives.
impl FromStr for ItemRef {
    type Err = anyhow::Error;

    fn from_str(url: &str) -> Result<Self> {
        ensure!(
            IssueInput::parse(url) == IssueInput::Url,
            "expected an issue, PR or CI run URL"
        );
        // A kind's route may also appear in a repository's path, so every
        // kind is tried before the first whose route the URL has says why
        // it fails.
        let mut routed = None;
        for kind in [LinkKind::Issue, LinkKind::Pr, LinkKind::Ci] {
            match Self::parse_kind(kind, url) {
                Ok(item) => return Ok(item),
                Err(error) if ForgeRepo::from_item_url(kind, url).is_ok() => {
                    routed.get_or_insert(error);
                }
                Err(_) => {}
            }
        }
        Err(routed.unwrap_or_else(|| anyhow::anyhow!("{url} is no issue, PR or CI run URL")))
    }
}

impl Serialize for ItemRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.url)
    }
}

impl<'de> Deserialize<'de> for ItemRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let url = String::deserialize(deserializer)?;
        url.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::ToSql for ItemRef {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.url.to_sql()
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::FromSql for ItemRef {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|error: anyhow::Error| rusqlite::types::FromSqlError::Other(error.into()))
    }
}

/// An issue, PR or CI run as a person names it: its number in the
/// repository the context gives, `#42` included, or its URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemInput {
    /// A positive number, whose kind the command or context decides.
    Number(u64),
    Url(ItemRef),
}

impl ItemInput {
    /// The kind a URL names; a number names none.
    pub fn kind(&self) -> Option<LinkKind> {
        match self {
            Self::Number(_) => None,
            Self::Url(item) => Some(item.kind()),
        }
    }
}

impl FromStr for ItemInput {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        let number = input
            .strip_prefix('#')
            .filter(|number| IssueInput::parse(number) == IssueInput::Number)
            .unwrap_or(input);
        match IssueInput::parse(number) {
            IssueInput::Number => {
                let number = number.parse::<u64>().context("item number is too large")?;
                ensure!(number > 0, "item number must be positive");
                Ok(Self::Number(number))
            }
            IssueInput::Url => Ok(Self::Url(input.parse()?)),
            IssueInput::Invalid => {
                bail!("expected an issue, PR or CI run number or URL, not {input}")
            }
        }
    }
}

impl fmt::Display for ItemInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(number) => number.fmt(f),
            Self::Url(item) => item.fmt(f),
        }
    }
}

impl From<ItemRef> for ItemInput {
    fn from(item: ItemRef) -> Self {
        Self::Url(item)
    }
}

impl Serialize for ItemInput {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ItemInput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let input = String::deserialize(deserializer)?;
        input.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests;
