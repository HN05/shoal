//! A hub member's ID: the key the hub gives a machine or device when it
//! pairs with it, tied to the credential it pairs with, which every
//! connection of the member presents. Machines are addressed by it, never
//! by their names, which may repeat.

crate::macros::text_id!(
    /// The ID of a machine or device paired with the hub, stored and
    /// serialized as its text.
    MemberId
);
