//! Closed Shoal enums; persisted and wire spellings remain stable.

/// A closed set of lowercase state names with matching serde, `Display`, and
/// SQLite conversions. Unknown values are rejected everywhere.
/// Add `#[derive(Default)]` with a `#[default]` variant for a default, or
/// `: ValueEnum` after the type name to expose the wire names to clap.
macro_rules! states {
    ($(#[$enum_meta:meta])* $name:ident: ValueEnum {
        $($(#[$meta:meta])* $variant:ident => $wire:literal),+ $(,)?
    }) => {
        $crate::state::states!($(#[$enum_meta])* $name {
            $($(#[$meta])* $variant => $wire),+
        });
        impl ::clap::ValueEnum for $name {
            fn value_variants<'a>() -> &'a [Self] {
                &[$(Self::$variant),+]
            }

            fn to_possible_value(&self) -> Option<::clap::builder::PossibleValue> {
                Some(::clap::builder::PossibleValue::new(self.as_str()))
            }
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
        impl ::rusqlite::types::ToSql for $name {
            fn to_sql(&self) -> ::rusqlite::Result<::rusqlite::types::ToSqlOutput<'_>> {
                Ok(::rusqlite::types::ToSqlOutput::Borrowed(::rusqlite::types::ValueRef::Text(self.as_str().as_bytes())))
            }
        }
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

/// Serde and SQLite text conversions for a key type spelled by `Display` and
/// parsed by `FromStr` with an `anyhow::Error`; the spelling is the stored key.
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
        impl ::rusqlite::types::ToSql for $name {
            fn to_sql(&self) -> ::rusqlite::Result<::rusqlite::types::ToSqlOutput<'_>> {
                Ok(::rusqlite::types::ToSqlOutput::from(self.to_string()))
            }
        }
        impl ::rusqlite::types::FromSql for $name {
            fn column_result(
                value: ::rusqlite::types::ValueRef<'_>,
            ) -> ::rusqlite::types::FromSqlResult<Self> {
                value.as_str()?.parse().map_err(|error: ::anyhow::Error| {
                    ::rusqlite::types::FromSqlError::Other(error.into())
                })
            }
        }
    };
}
pub(crate) use text_key;

states!(WorkspaceState {
    Preparing => "preparing",
    Ready => "ready",
    Stopping => "stopping",
    Removing => "removing",
    Reconciling => "reconciling",
    Failed => "failed",
});
states!(ExecutionState {
    Running => "running",
    Unknown => "unknown",
});

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    fn assert_spellings<T>(values: &[(T, &str)])
    where
        T: Copy
            + std::fmt::Debug
            + std::fmt::Display
            + PartialEq
            + serde::Serialize
            + serde::de::DeserializeOwned
            + rusqlite::types::ToSql
            + rusqlite::types::FromSql,
    {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for &(value, wire) in values {
            assert_eq!(value.to_string(), wire);
            assert_eq!(serde_json::to_value(value).unwrap(), wire);
            assert_eq!(serde_json::from_value::<T>(wire.into()).unwrap(), value);
            assert_eq!(
                db.query_row("SELECT ?1", [value], |row| row.get::<_, String>(0))
                    .unwrap(),
                wire
            );
            assert_eq!(
                db.query_row("SELECT ?1", [wire], |row| row.get::<_, T>(0))
                    .unwrap(),
                value
            );
        }
        assert!(serde_json::from_str::<T>("\"not_a_value\"").is_err());
        assert!(
            db.query_row("SELECT 'not_a_value'", [], |row| row.get::<_, T>(0))
                .is_err()
        );
    }

    #[test]
    fn migrated_enums_preserve_spellings_and_defaults() {
        use crate::{
            agent::{BuiltinAgent, CodexMode},
            config::repo::ConflictPolicy,
            daemon::{
                access::{DecisionStatus, Lifetime},
                doctor::CheckStatus,
                recovery::DirectoryState,
                resources::{LockMode, ResourceKind},
                workspace::ExecutionKind,
            },
        };
        assert_spellings(&[
            (Lifetime::Lease, "lease"),
            (Lifetime::Workspace, "workspace"),
        ]);
        assert_spellings(&[
            (DecisionStatus::Pending, "pending"),
            (DecisionStatus::Approved, "approved"),
            (DecisionStatus::Denied, "denied"),
        ]);
        assert_spellings(&[
            (ResourceKind::Semaphore, "semaphore"),
            (ResourceKind::Rwlock, "rwlock"),
        ]);
        assert_spellings(&[
            (LockMode::Permit, "permit"),
            (LockMode::Read, "read"),
            (LockMode::Write, "write"),
        ]);
        assert_spellings(&[
            (ConflictPolicy::Auto, "auto"),
            (ConflictPolicy::Suggest, "suggest"),
        ]);
        assert_spellings(&[
            (DirectoryState::Valid, "valid"),
            (DirectoryState::Missing, "missing"),
            (DirectoryState::Moved, "moved"),
            (DirectoryState::Unverified, "unverified"),
        ]);
        assert_spellings(&[
            (CheckStatus::Ok, "ok"),
            (CheckStatus::Warning, "warning"),
            (CheckStatus::Error, "error"),
            (CheckStatus::Skipped, "skipped"),
        ]);
        assert_spellings(&[(CodexMode::Cli, "cli"), (CodexMode::App, "app")]);
        assert_spellings(&[
            (ExecutionKind::Command, "command"),
            (ExecutionKind::Land, "land"),
            (ExecutionKind::Setup, "setup"),
        ]);
        assert_spellings(&[
            (BuiltinAgent::Claude, "claude"),
            (BuiltinAgent::Codex, "codex"),
        ]);
        assert_eq!(Lifetime::default(), Lifetime::Lease);
        assert_eq!(ResourceKind::default(), ResourceKind::Semaphore);
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::Suggest);
        assert_eq!(CodexMode::default(), CodexMode::Cli);
    }

    states!(
        /// Test spellings that clap cannot infer from the Rust names.
        #[derive(Default)]
        Example: ValueEnum {
            First => "first_value",
            #[default]
            Second => "other",
        }
    );

    #[test]
    fn optional_default_and_clap_values_share_wire_spellings() {
        assert_eq!(Example::default(), Example::Second);
        assert_eq!(
            Example::value_variants(),
            &[Example::First, Example::Second]
        );
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for (value, wire) in [(Example::First, "first_value"), (Example::Second, "other")] {
            assert_eq!(value.to_string(), wire);
            assert_eq!(serde_json::to_value(value).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<Example>(wire.into()).unwrap(),
                value
            );
            assert_eq!(value.to_possible_value().unwrap().get_name(), wire);
            assert_eq!(Example::from_str(wire, false).unwrap(), value);
            assert_eq!(
                db.query_row("SELECT ?1", [value], |row| row.get::<_, String>(0))
                    .unwrap(),
                wire
            );
            assert_eq!(
                db.query_row("SELECT ?1", [wire], |row| row.get::<_, Example>(0))
                    .unwrap(),
                value
            );
        }
        for invalid in ["First", "first-value", "second", "unknown"] {
            assert!(Example::from_str(invalid, false).is_err());
            assert!(serde_json::from_value::<Example>(invalid.into()).is_err());
            assert!(
                db.query_row("SELECT ?1", [invalid], |row| row.get::<_, Example>(0))
                    .is_err()
            );
        }
    }

    #[test]
    fn persisted_and_wire_states_are_compatible_and_reject_unknown_values() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for (wire, state) in [
            ("preparing", WorkspaceState::Preparing),
            ("ready", WorkspaceState::Ready),
            ("stopping", WorkspaceState::Stopping),
            ("removing", WorkspaceState::Removing),
            ("reconciling", WorkspaceState::Reconciling),
            ("failed", WorkspaceState::Failed),
        ] {
            assert_eq!(
                serde_json::from_value::<WorkspaceState>(serde_json::json!(wire)).unwrap(),
                state
            );
            assert_eq!(serde_json::to_value(state).unwrap(), wire);
            let saved: String = db
                .query_row("SELECT ?1", [state], |row| row.get(0))
                .unwrap();
            assert_eq!(saved, wire);
            assert_eq!(
                db.query_row("SELECT ?1", [wire], |row| row.get::<_, WorkspaceState>(0))
                    .unwrap(),
                state
            );
        }
        for (wire, state) in [
            ("running", ExecutionState::Running),
            ("unknown", ExecutionState::Unknown),
        ] {
            assert_eq!(
                serde_json::from_value::<ExecutionState>(serde_json::json!(wire)).unwrap(),
                state
            );
            assert_eq!(serde_json::to_value(state).unwrap(), wire);
            assert_eq!(
                db.query_row("SELECT ?1", [state], |row| row.get::<_, String>(0))
                    .unwrap(),
                wire
            );
            assert_eq!(
                db.query_row("SELECT ?1", [wire], |row| row.get::<_, ExecutionState>(0))
                    .unwrap(),
                state
            );
        }
        assert!(
            db.query_row("SELECT 'running'", [], |row| row
                .get::<_, WorkspaceState>(0))
                .is_err()
        );
        assert!(
            db.query_row("SELECT 'ready'", [], |row| row.get::<_, ExecutionState>(0))
                .is_err()
        );
        assert!(serde_json::from_str::<WorkspaceState>("\"unknown\"").is_err());
        assert!(serde_json::from_str::<ExecutionState>("\"finished\"").is_err());
    }
}
