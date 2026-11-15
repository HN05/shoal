//! An agent queue entry's ID: the key the daemon gives a launch it admits
//! or queues, which records, the protocol and the store refer to it by.

crate::macros::text_id!(
    /// The ID of an agent queue entry, stored and serialized as its text.
    QueueEntryId
);
