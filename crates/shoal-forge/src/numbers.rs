//! The numbers a forge gives its issues, PRs, runs, jobs, runners and
//! comments, each its own type so one is never passed as another. Each
//! serializes and stores as the number it holds; backends convert where they
//! read a response or build a request.
use std::{fmt, num::ParseIntError, str::FromStr};

/// Defines `$name` as a forge number held as a `u64`.
macro_rules! forge_number {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            Copy,
            Default,
            PartialEq,
            Eq,
            Hash,
            PartialOrd,
            Ord,
            serde::Serialize,
            serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub const fn new(number: u64) -> Self {
                Self(number)
            }

            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = ParseIntError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                text.parse().map(Self)
            }
        }

        /// A forge number in a JSON field, compared as the number it holds.
        impl PartialEq<$name> for serde_json::Value {
            fn eq(&self, other: &$name) -> bool {
                *self == other.0
            }
        }

        #[cfg(feature = "rusqlite")]
        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                let number = i64::try_from(self.0)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?;
                Ok(number.into())
            }
        }

        #[cfg(feature = "rusqlite")]
        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let number = i64::column_result(value)?;
                u64::try_from(number)
                    .map(Self)
                    .map_err(|_| rusqlite::types::FromSqlError::OutOfRange(number))
            }
        }
    };
}

forge_number!(
    /// An issue's number in its repository. On a forge that numbers issues
    /// and PRs in one sequence, a PR's number is also an issue number.
    IssueNumber
);
forge_number!(
    /// A PR's number in its repository.
    PullNumber
);
forge_number!(
    /// The ID a forge's API reads and acts on a CI run by.
    RunId
);
forge_number!(
    /// The number a CI run's web URL holds: its number in the repository on
    /// Forgejo, and its ID on forges whose web URLs name runs by ID.
    RunNumber
);
forge_number!(
    /// The ID a forge's API reads and acts on a CI job by.
    JobId
);
forge_number!(
    /// The ID of a CI runner the forge registered.
    RunnerId
);
forge_number!(
    /// The ID of a comment on an issue or PR, or in a review thread.
    CommentId
);

#[cfg(test)]
mod tests;
