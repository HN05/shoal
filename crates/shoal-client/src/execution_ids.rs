//! An execution's ID: the key the daemon gives a tracked execution, such as
//! an agent, a `shoal run` command or setup, when it registers it, which
//! records, the protocol, the store and the wrapper's environment refer to
//! it by. It is never an agent's name.
use crate::ids::text_id;

text_id!(
    /// The ID of a tracked execution, stored and serialized as its text.
    ExecutionId
);
