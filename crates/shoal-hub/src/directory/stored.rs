//! A space summary as the directory's tables keep it: the space's number in
//! a column, which queries match, beside its ID, and the rest as JSON.
use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Map, Value};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::hub::directory::SpaceSummary;

/// The fields of a summary that columns keep instead of its JSON.
const ID: &str = "id";
const NUMBER: &str = "number";

/// A summary's number and the JSON of its other fields but its ID.
pub(super) struct Stored {
    pub(super) number: SpaceNumber,
    pub(super) summary: String,
}

impl Stored {
    pub(super) fn of(space: &SpaceSummary) -> Result<Self> {
        let Value::Object(mut fields) = serde_json::to_value(space)? else {
            bail!("a space summary is no JSON object");
        };
        for column in [ID, NUMBER] {
            ensure!(
                fields.remove(column).is_some(),
                "a space summary has no field {column}"
            );
        }
        Ok(Self {
            number: space.number,
            summary: serde_json::to_string(&fields)?,
        })
    }

    /// The summary of the space `id` this keeps.
    pub(super) fn summary(&self, id: &SpaceId) -> Result<SpaceSummary> {
        let mut fields: Map<String, Value> =
            serde_json::from_str(&self.summary).context("read a space summary the hub kept")?;
        fields.insert(ID.to_owned(), serde_json::to_value(id)?);
        fields.insert(NUMBER.to_owned(), serde_json::to_value(self.number)?);
        serde_json::from_value(Value::Object(fields)).context("read a space summary the hub kept")
    }
}

#[cfg(test)]
mod tests;
