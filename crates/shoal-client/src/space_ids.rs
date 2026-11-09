//! A space's ID and number. The ID is the key the daemon gives a workspace
//! or chat when it creates it, which records, the protocol and the store
//! refer to it by. It is never a name or a selector's text, which name a
//! space only until the daemon resolves them. The number is the short one
//! commands select a space by while it exists.

crate::ids::text_id!(
    /// The ID of a workspace or chat, stored and serialized as its text.
    SpaceId
);

crate::ids::number!(
    /// The number of a workspace or chat, unique among the spaces that
    /// exist and freed when one goes; 0 for none.
    SpaceNumber(u32)
);
