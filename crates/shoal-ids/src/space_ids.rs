crate::macros::text_id!(
    /// The ID of a workspace or chat. It is never a name or a selector's
    /// text, which name a space only until the daemon resolves them.
    SpaceId
);

crate::macros::number!(
    /// The number of a workspace or chat, unique among the spaces that
    /// exist and freed when one goes; 0 for none.
    SpaceNumber(u32)
);
