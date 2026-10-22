//! Closed sets of names with the same spelling in serde, `Display` and,
//! with the `rusqlite` and `clap` features, SQLite columns and CLI values.
macro_rules! states {
    ($(#[$enum_meta:meta])* $name:ident: ValueEnum {
        $($(#[$meta:meta])* $variant:ident => $wire:literal),+ $(,)?
    }) => {
        $crate::state::states!($(#[$enum_meta])* $name: Variants {
            $($(#[$meta])* $variant => $wire),+
        });
        #[cfg(feature = "clap")]
        impl ::clap::ValueEnum for $name {
            fn value_variants<'a>() -> &'a [Self] {
                Self::ALL
            }

            fn to_possible_value(&self) -> Option<::clap::builder::PossibleValue> {
                Some(::clap::builder::PossibleValue::new(self.as_str()))
            }
        }
    };
    ($(#[$enum_meta:meta])* $name:ident: Variants {
        $($(#[$meta:meta])* $variant:ident => $wire:literal),+ $(,)?
    }) => {
        $crate::state::states!($(#[$enum_meta])* $name {
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
        #[derive(Debug, Clone, Copy, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        pub enum $name { $($(#[$meta])* #[serde(rename = $wire)] $variant),+ }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.as_str()) }
        }
        #[cfg(feature = "rusqlite")]
        impl ::rusqlite::types::ToSql for $name {
            fn to_sql(&self) -> ::rusqlite::Result<::rusqlite::types::ToSqlOutput<'_>> {
                Ok(::rusqlite::types::ToSqlOutput::Borrowed(::rusqlite::types::ValueRef::Text(self.as_str().as_bytes())))
            }
        }
        #[cfg(feature = "rusqlite")]
        impl ::rusqlite::types::FromSql for $name {
            fn column_result(value: ::rusqlite::types::ValueRef<'_>) -> ::rusqlite::types::FromSqlResult<Self> {
                match value.as_str()? {
                    $($wire => Ok(Self::$variant),)+
                    other => Err(::rusqlite::types::FromSqlError::Other(Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData,
                        format!("invalid {}: {other}", stringify!($name)))))),
                }
            }
        }
    };
}
pub(crate) use states;
