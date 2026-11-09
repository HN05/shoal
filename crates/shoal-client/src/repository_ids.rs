//! A repository's ID: the key the daemon gives a repository when it
//! registers it, which records, the protocol and the store refer to it by.
//! It is never a name, path or source, which a selector resolves to a
//! repository.

crate::ids::text_id!(
    /// The ID of a registered repository, stored and serialized as its text.
    RepositoryId
);
