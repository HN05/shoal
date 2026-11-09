//! A space's ID: the key the daemon gives a workspace or chat when it
//! creates it, which records, the protocol and the store refer to it by. It
//! is never a name or a selector's text, which name a space only until the
//! daemon resolves them.

crate::ids::text_id!(
    /// The ID of a workspace or chat, stored and serialized as its text.
    SpaceId
);
