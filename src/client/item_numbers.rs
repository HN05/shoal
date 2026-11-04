//! The numbers of questions and approval requests: the number of the space
//! that asked, then the item's own number within it, written `3.2`.
use std::{fmt, str::FromStr};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, ser::SerializeStruct};

/// A question or approval request by its space's number and its own. Both
/// kinds share one sequence per space, so a number names one item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemNumber {
    pub space: u32,
    pub item: u32,
}

impl fmt::Display for ItemNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.space, self.item)
    }
}

impl FromStr for ItemNumber {
    type Err = anyhow::Error;

    /// `S.N`; a bare number names no space, so it is refused with the form
    /// that does.
    fn from_str(text: &str) -> Result<Self> {
        let Some((space, item)) = text.split_once('.') else {
            if text.parse::<u32>().is_ok() {
                bail!(
                    "{text} names no space: questions and approval requests are numbered SPACE.ITEM, such as 3.{text}"
                );
            }
            bail!("invalid number {text}: expected SPACE.ITEM, such as 3.2");
        };
        let part = |part: &str| {
            part.parse::<u32>()
                .ok()
                .filter(|number| *number > 0)
                .with_context(|| format!("invalid number {text}: expected SPACE.ITEM, such as 3.2"))
        };
        Ok(Self {
            space: part(space)?,
            item: part(item)?,
        })
    }
}

/// A question or an approval request, which share their numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Question,
    Approval,
}

impl ItemKind {
    /// The refusal of `number`, which names an item of this kind, given
    /// where the other kind is expected.
    pub fn misdirected(self, number: ItemNumber) -> String {
        match self {
            Self::Question => format!("{number} is a question; use shoal questions {number}"),
            Self::Approval => {
                format!("{number} is an approval request; use shoal approvals {number}")
            }
        }
    }
}

/// Fields of the records an item number flattens into: both numbers and the
/// `3.2` text.
const FIELDS: [&str; 3] = ["space_number", "item", "number"];

impl Serialize for ItemNumber {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut fields = serializer.serialize_struct("ItemNumber", FIELDS.len())?;
        fields.serialize_field(FIELDS[0], &self.space)?;
        fields.serialize_field(FIELDS[1], &self.item)?;
        fields.serialize_field(FIELDS[2], &self.to_string())?;
        fields.end()
    }
}

impl<'de> Deserialize<'de> for ItemNumber {
    /// Reads both numbers; the text repeats them.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Numbers {
            #[serde(default)]
            space_number: u32,
            #[serde(default)]
            item: u32,
        }
        let Numbers { space_number, item } = Numbers::deserialize(deserializer)?;
        Ok(Self {
            space: space_number,
            item,
        })
    }
}

#[cfg(test)]
mod tests;
