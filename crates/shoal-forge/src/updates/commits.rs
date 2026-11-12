//! A commit a snapshot may lack, such as an issue's head, stored as the
//! empty text it was stored as before commits had a type of their own.
use serde::{Deserialize, Deserializer, Serializer};
use shoal_refs::CommitId;

pub(crate) fn serialize<S: Serializer>(
    commit: &Option<CommitId>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(commit.as_ref().map_or("", CommitId::as_str))
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<CommitId>, D::Error> {
    let text = String::deserialize(deserializer)?;
    Ok((!text.is_empty()).then(|| text.into()))
}
