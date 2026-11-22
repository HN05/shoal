//! Each forge's way of doing what forges do differently. Each forge's marker
//! type, which implements its routes in `shoal_forge_types::backends`,
//! implements here, in a module of its own for each, the trait of every
//! module that covers what it does its own way, such as
//! `transport::backend::Backend` and `ci::runs::Runs`;
//! `ForgeKind` names the one a repository uses and forwards each call to it,
//! so a new forge adds its variant and routes there and implements the traits
//! here. No other module names this one.
pub mod bitbucket;
pub(crate) mod forgejo;
pub(crate) mod github;
mod github_rest;
pub(crate) mod gitlab;
pub(crate) mod rest;
