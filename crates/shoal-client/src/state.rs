//! Shoal enums whose persisted and wire spellings remain stable.

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
pub use states;

// A `cfg` inside an exported macro's body is checked in the crate that
// expands it, so each helper below is defined twice, by this crate's
// features: once with the impls and once empty.

/// [`states!`]'s clap values.
#[cfg(feature = "clap")]
#[doc(hidden)]
#[macro_export]
macro_rules! states_value_enum {
    ($name:ident) => {
        impl ::clap::ValueEnum for $name {
            fn value_variants<'a>() -> &'a [Self] {
                Self::ALL
            }

            fn to_possible_value(&self) -> Option<::clap::builder::PossibleValue> {
                Some(::clap::builder::PossibleValue::new(self.as_str()))
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
pub use open_enum;

/// Serde and, with the `rusqlite` feature, SQLite text conversions for a key
/// type spelled by `Display` and parsed by `FromStr` with an `anyhow::Error`;
/// the spelling is the stored key.
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
pub use text_key;

/// [`text_key!`]'s SQLite conversions.
#[cfg(feature = "rusqlite")]
#[doc(hidden)]
#[macro_export]
macro_rules! text_key_sql {
    ($name:ty) => {
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

#[cfg(not(feature = "rusqlite"))]
#[doc(hidden)]
#[macro_export]
macro_rules! text_key_sql {
    ($name:ty) => {};
}

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
    /// A new agent that `shoal start` plans.
    Launch => "launch",
    /// A protection-stopped agent's automatic restore in its own workspace.
    Restore => "restore",
    /// A CI job waiting in the forge's queue for this machine's runner.
    Job => "job",
});
states!(
    /// How a stopped chat comes back.
    ChatRestore {
        /// The daemon restores it once it starts agents again, as after a
        /// shutdown or a logout.
        Automatic => "automatic",
        /// It waits for `shoal resume`, as after `shoal daemon stop` or once
        /// its agent exited.
        Resume => "resume",
    }
);
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

#[cfg(all(test, feature = "rusqlite", feature = "clap"))]
mod tests;
