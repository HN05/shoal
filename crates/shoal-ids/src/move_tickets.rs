//! A move ticket: the key the daemon gives each delivery of a space to
//! another joined machine, which that machine's claims of the space carry,
//! so the hub tells them from those of a delivery its source gave up on.

crate::macros::text_id!(
    /// The ID of one delivery of a moved space, stored and serialized as
    /// its text.
    MoveTicket
);
