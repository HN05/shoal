//! Typed lifecycle states; persisted and wire spellings remain stable.
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

macro_rules! states {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $wire)] $variant),+ }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.as_str()) }
        }
        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> { Ok(ToSqlOutput::Borrowed(ValueRef::Text(self.as_str().as_bytes()))) }
        }
        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                match value.as_str()? {
                    $($wire => Ok(Self::$variant),)+
                    other => Err(FromSqlError::Other(Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData,
                        format!("invalid {}: {other}", stringify!($name)))))),
                }
            }
        }
    };
}

states!(WorkspaceState {
    Preparing => "preparing",
    Ready => "ready",
    Stopping => "stopping",
    Removing => "removing",
    Failed => "failed",
});
states!(ExecutionState {
    Running => "running",
    Unknown => "unknown",
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_and_wire_states_are_compatible_and_reject_unknown_values() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for (wire, state) in [
            ("preparing", WorkspaceState::Preparing),
            ("ready", WorkspaceState::Ready),
            ("stopping", WorkspaceState::Stopping),
            ("removing", WorkspaceState::Removing),
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
