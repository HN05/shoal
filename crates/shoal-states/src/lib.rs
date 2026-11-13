//! Macros that give Shoal's typed names one spelling in serde, `Display`
//! and, with the `rusqlite` and `clap` features, SQLite columns and CLI
//! values. A crate that expands them depends on `serde`, whose derives name
//! it; this crate's features decide the SQLite and clap impls.

#[doc(hidden)]
pub mod __private {
    #[cfg(feature = "clap")]
    pub use clap;
    #[cfg(feature = "rusqlite")]
    pub use rusqlite;
}

/// A closed set of lowercase state names with matching serde, `Display`, and,
/// with the `rusqlite` feature, SQLite conversions. Unknown values are
/// rejected everywhere. Add `#[derive(Default)]` with a `#[default]` variant
/// for a default, or `: ValueEnum` after the type name to expose the wire
/// names to clap with the `clap` feature. Use `: Variants` for an ordered
/// `ALL` list without a clap implementation.
#[macro_export]
macro_rules! states {
    ($(#[$enum_meta:meta])* $name:ident: ValueEnum {
        $($(#[$meta:meta])* $variant:ident => $wire:literal),+ $(,)?
    }) => {
        $crate::states!($(#[$enum_meta])* $name: Variants {
            $($(#[$meta])* $variant => $wire),+
        });
        $crate::states_value_enum!($name);
    };
    ($(#[$enum_meta:meta])* $name:ident: Variants {
        $($(#[$meta:meta])* $variant:ident => $wire:literal),+ $(,)?
    }) => {
        $crate::states!($(#[$enum_meta])* $name {
            $($(#[$meta])* $variant => $wire),+
        });
        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
        }
    };
    ($(#[$enum_meta:meta])* $name:ident {
        $($(#[$meta:meta])* $variant:ident => $wire:literal),+ $(,)?
    }) => {
        $(#[$enum_meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize)]
        pub enum $name { $($(#[$meta])* #[serde(rename = $wire)] $variant),+ }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.as_str()) }
        }
        $crate::states_sql!($name { $($variant => $wire),+ });
    };
}

// A `cfg` inside an exported macro's body is checked in the crate that
// expands it, so each helper below is defined twice, by this crate's
// features: once with the impls and once empty.

/// [`states!`]'s clap values.
#[cfg(feature = "clap")]
#[doc(hidden)]
#[macro_export]
macro_rules! states_value_enum {
    ($name:ident) => {
        impl $crate::__private::clap::ValueEnum for $name {
            fn value_variants<'a>() -> &'a [Self] {
                Self::ALL
            }

            fn to_possible_value(&self) -> Option<$crate::__private::clap::builder::PossibleValue> {
                Some($crate::__private::clap::builder::PossibleValue::new(
                    self.as_str(),
                ))
            }
        }
    };
}

#[cfg(not(feature = "clap"))]
#[doc(hidden)]
#[macro_export]
macro_rules! states_value_enum {
    ($name:ident) => {};
}

/// [`states!`]'s SQLite conversions.
#[cfg(feature = "rusqlite")]
#[doc(hidden)]
#[macro_export]
macro_rules! states_sql {
    ($name:ident { $($variant:ident => $wire:literal),+ }) => {
        impl $crate::__private::rusqlite::types::ToSql for $name {
            fn to_sql(&self) -> $crate::__private::rusqlite::Result<$crate::__private::rusqlite::types::ToSqlOutput<'_>> {
                Ok($crate::__private::rusqlite::types::ToSqlOutput::Borrowed($crate::__private::rusqlite::types::ValueRef::Text(self.as_str().as_bytes())))
            }
        }
        impl $crate::__private::rusqlite::types::FromSql for $name {
            fn column_result(value: $crate::__private::rusqlite::types::ValueRef<'_>) -> $crate::__private::rusqlite::types::FromSqlResult<Self> {
                match value.as_str()? {
                    $($wire => Ok(Self::$variant),)+
                    other => Err($crate::__private::rusqlite::types::FromSqlError::Other(Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData,
                        format!("invalid {}: {other}", stringify!($name)))))),
                }
            }
        }
    };
}

#[cfg(not(feature = "rusqlite"))]
#[doc(hidden)]
#[macro_export]
macro_rules! states_sql {
    ($($tokens:tt)*) => {};
}

/// An open set of wire names with matching serde and `Display` spellings.
/// Unfamiliar strings decode to `Unknown` and keep their spelling, so a peer's
/// newer vocabulary round-trips without matching a known variant.
#[macro_export]
macro_rules! open_enum {
    ($(#[$enum_meta:meta])* $name:ident {
        $($(#[$meta:meta])* $variant:ident => $wire:literal),+ $(,)?
    }) => {
        $(#[$enum_meta])*
        #[derive(Debug, Clone, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(from = "String")]
        pub enum $name {
            $($(#[$meta])* #[serde(rename = $wire)] $variant,)+
            /// An unfamiliar value, preserved verbatim.
            #[serde(untagged)]
            Unknown(String),
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                match value.as_str() {
                    $($wire => Self::$variant,)+
                    _ => Self::Unknown(value),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(match self {
                    $(Self::$variant => $wire,)+
                    Self::Unknown(value) => value,
                })
            }
        }
    };
}

/// Serde and, with the `rusqlite` feature, SQLite text conversions for a key
/// type spelled by `Display` and parsed by `FromStr` with an error that
/// converts into a boxed one, such as `anyhow::Error`; the spelling is the
/// stored key.
#[macro_export]
macro_rules! text_key {
    ($name:ty) => {
        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }
        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Self, D::Error> {
                <String as ::serde::Deserialize>::deserialize(deserializer)?
                    .parse()
                    .map_err(::serde::de::Error::custom)
            }
        }
        $crate::text_key_sql!($name);
    };
}

/// [`text_key!`]'s SQLite conversions.
#[cfg(feature = "rusqlite")]
#[doc(hidden)]
#[macro_export]
macro_rules! text_key_sql {
    ($name:ty) => {
        impl $crate::__private::rusqlite::types::ToSql for $name {
            fn to_sql(
                &self,
            ) -> $crate::__private::rusqlite::Result<
                $crate::__private::rusqlite::types::ToSqlOutput<'_>,
            > {
                Ok($crate::__private::rusqlite::types::ToSqlOutput::from(
                    self.to_string(),
                ))
            }
        }
        impl $crate::__private::rusqlite::types::FromSql for $name {
            fn column_result(
                value: $crate::__private::rusqlite::types::ValueRef<'_>,
            ) -> $crate::__private::rusqlite::types::FromSqlResult<Self> {
                value.as_str()?.parse::<$name>().map_err(|error| {
                    $crate::__private::rusqlite::types::FromSqlError::Other(error.into())
                })
            }
        }
    };
}

#[cfg(not(feature = "rusqlite"))]
#[doc(hidden)]
#[macro_export]
macro_rules! text_key_sql {
    ($name:ty) => {};
}

#[cfg(all(test, feature = "rusqlite", feature = "clap"))]
mod tests;
