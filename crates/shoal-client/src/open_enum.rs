//! Enums that stay open on receipt: a value this version does not know
//! decodes as `Unknown` with its spelling, so a newer daemon or client can
//! add values without breaking an older peer.

/// An open enum of string values, with its JSON Schema.
macro_rules! open_enum {
    ($(#[$meta:meta])* $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $wire:literal),+ $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$variant_meta])* $variant,)+
            /// A value this version does not know, as it was spelled.
            Unknown(String),
        }

        impl $name {
            /// The spellings of every value this version knows.
            pub const KNOWN: &'static [&'static str] = &[$($wire),+];

            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::Unknown(value) => value,
                }
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value {
                    $($wire => Self::$variant,)+
                    _ => Self::Unknown(value.to_owned()),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = <String as ::serde::Deserialize>::deserialize(deserializer)?;
                Ok(Self::from(value.as_str()))
            }
        }

        impl ::schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            /// The known values, and any other string, which a newer peer may send.
            fn json_schema(_: &mut ::schemars::SchemaGenerator) -> ::schemars::Schema {
                ::schemars::json_schema!({
                    "anyOf": [{"enum": [$($wire),+]}, {"type": "string"}]
                })
            }
        }
    };
}

pub(crate) use open_enum;
