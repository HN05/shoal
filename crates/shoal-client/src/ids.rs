//! The shape every ID the daemon gives a record shares: text that holds
//! only that kind of ID, serialized and stored as the text it is.

/// Defines `$name` as an ID held as text, with the conversions and
/// comparisons code that reads, stores and prints it needs. It does not
/// dereference to `str`: code that uses it as text says so with `as_str()`
/// or an `AsRef` impl.
macro_rules! text_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            PartialEq,
            Eq,
            Hash,
            PartialOrd,
            Ord,
            serde::Serialize,
            serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// A new record's ID.
            pub fn generate() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<std::ffi::OsStr> for $name {
            fn as_ref(&self) -> &std::ffi::OsStr {
                self.0.as_ref()
            }
        }

        impl AsRef<std::path::Path> for $name {
            fn as_ref(&self) -> &std::path::Path {
                self.0.as_ref()
            }
        }

        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        /// The ID that recorded text, such as an environment variable or an
        /// argument Shoal passes itself, holds.
        impl From<String> for $name {
            fn from(id: String) -> Self {
                Self(id)
            }
        }

        /// The ID an argument Shoal passes itself holds, as clap parses it.
        impl std::str::FromStr for $name {
            type Err = std::convert::Infallible;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ok(Self(value.to_owned()))
            }
        }

        /// IDs tests write as literals.
        #[cfg(any(test, feature = "test-support"))]
        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl From<$name> for std::ffi::OsString {
            fn from(id: $name) -> Self {
                id.0.into()
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                &self.0 == other
            }
        }

        impl PartialEq<$name> for str {
            fn eq(&self, other: &$name) -> bool {
                self == other.0
            }
        }

        impl PartialEq<$name> for &str {
            fn eq(&self, other: &$name) -> bool {
                *self == other.0
            }
        }

        impl PartialEq<$name> for String {
            fn eq(&self, other: &$name) -> bool {
                *self == other.0
            }
        }

        #[cfg(feature = "rusqlite")]
        impl rusqlite::types::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                self.0.to_sql()
            }
        }

        #[cfg(feature = "rusqlite")]
        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                String::column_result(value).map(Self)
            }
        }
    };
}

pub(crate) use text_id;

/// Defines `$name` as a number that counts things of one kind, held as the
/// unsigned integer `$inner` and stored as SQLite's integer.
macro_rules! number {
    ($(#[$meta:meta])* $name:ident($inner:ty)) => {
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
        pub struct $name($inner);

        impl $name {
            pub const fn new(number: $inner) -> Self {
                Self(number)
            }

            pub const fn get(self) -> $inner {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = std::num::ParseIntError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }

        /// Numbers tests write as literals.
        #[cfg(any(test, feature = "test-support"))]
        impl From<$inner> for $name {
            fn from(number: $inner) -> Self {
                Self(number)
            }
        }

        #[cfg(any(test, feature = "test-support"))]
        impl PartialEq<$inner> for $name {
            fn eq(&self, other: &$inner) -> bool {
                self.0 == *other
            }
        }

        #[cfg(feature = "rusqlite")]
        impl rusqlite::types::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                i64::try_from(self.0)
                    .map(rusqlite::types::ToSqlOutput::from)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))
            }
        }

        #[cfg(feature = "rusqlite")]
        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let number = value.as_i64()?;
                <$inner>::try_from(number)
                    .map(Self)
                    .map_err(|_| rusqlite::types::FromSqlError::OutOfRange(number))
            }
        }
    };
}

pub(crate) use number;
