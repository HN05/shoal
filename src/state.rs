//! Shoal enums whose persisted and wire spellings remain stable.

/// A closed set of lowercase state names with matching serde, `Display`, and
/// SQLite conversions. Unknown values are rejected everywhere.
/// Add `#[derive(Default)]` with a `#[default]` variant for a default, or
/// `: ValueEnum` after the type name to expose the wire names to clap.
/// Use `: Variants` for an ordered `ALL` list without a clap implementation.
macro_rules! states {
    ($(#[$enum_meta:meta])* $name:ident: ValueEnum {
        $($(#[$meta:meta])* $variant:ident => $wire:literal),+ $(,)?
    }) => {
        $crate::state::states!($(#[$enum_meta])* $name: Variants {
            $($(#[$meta])* $variant => $wire),+
        });
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

/// An open set of wire names with matching serde and `Display` spellings.
/// Unfamiliar strings decode to `Unknown` and keep their spelling, so a peer's
/// newer vocabulary round-trips without matching a known variant.
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
pub(crate) use open_enum;

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

states!(WorkspaceState: Variants {
    Preparing => "preparing",
    Ready => "ready",
    Stopping => "stopping",
    Removing => "removing",
    Reconciling => "reconciling",
    /// Returning to a checkpoint.
    Restoring => "restoring",
    /// Fast-forwarding to its branch's pushed head.
    Updating => "updating",
    Failed => "failed",
});

impl WorkspaceState {
    /// Lifecycle operations interrupted when the daemon stops.
    pub fn transient() -> impl Iterator<Item = Self> {
        Self::ALL.iter().copied().filter(|state| match state {
            Self::Preparing
            | Self::Stopping
            | Self::Removing
            | Self::Reconciling
            | Self::Restoring
            | Self::Updating => true,
            Self::Ready | Self::Failed => false,
        })
    }

    /// A workspace being stopped, removed, reconciled, restored or updated
    /// takes no link changes.
    pub fn accepts_links(self) -> bool {
        matches!(self, Self::Ready | Self::Preparing | Self::Failed)
    }
}

states!(ExecutionState {
    Running => "running",
    Unknown => "unknown",
});
states!(QueueState {
    /// Waiting for an agent slot.
    Queued => "queued",
    /// Admitted to a slot; its agent has not registered yet.
    Starting => "starting",
});
states!(QueueKind {
    /// A new agent that `shoal add` plans.
    Launch => "launch",
    /// A protection-stopped agent's automatic restore in its own workspace.
    Restore => "restore",
});
states!(DependencyKind {
    /// A workspace, by ID.
    Workspace => "workspace",
    /// A queued launch, by entry ID, until it opens its workspace.
    Launch => "launch",
    /// An issue, by URL.
    Issue => "issue",
});
states!(AgentState: ValueEnum {
    /// The agent is working on a turn.
    Working => "working",
    /// The agent stopped mid-turn for the user, such as a permission prompt.
    Waiting => "waiting",
    /// The agent finished its turn.
    Idle => "idle",
});
states!(TurnFailure {
    /// The API refused the request for a rate or usage limit.
    RateLimited => "rate_limited",
    /// The API was overloaded.
    Overloaded => "overloaded",
    /// The API failed on its side.
    ServerError => "server_error",
    /// Any other error, such as authentication or an invalid request.
    Other => "other",
});

impl TurnFailure {
    /// Whether the same turn may succeed once the API recovers.
    pub fn retryable(self) -> bool {
        self != Self::Other
    }

    /// The failure as the user reads it.
    pub fn describe(self) -> &'static str {
        match self {
            Self::RateLimited => "rate limited",
            Self::Overloaded => "API overloaded",
            Self::ServerError => "API server error",
            Self::Other => "API error",
        }
    }
}

#[cfg(test)]
mod tests;
