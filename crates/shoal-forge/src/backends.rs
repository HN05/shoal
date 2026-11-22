//! What differs between forges' requests. Each forge's marker type, which
//! implements its routes in `shoal_forge_types::backends`, implements
//! [`Backend`] and, in a module of its own for each, the area traits over
//! it, such as [`Items`] and [`Runs`]; `ForgeKind` names the one a
//! repository uses and forwards each call to it, so a new forge adds its
//! variant and routes there and implements the traits here.
pub mod bitbucket;
pub(crate) mod forgejo;
pub(crate) mod github;
mod github_rest;
pub(crate) mod gitlab;
pub(crate) mod rest;

mod area;
pub(crate) use area::{Activity, BranchFields, Branches, Conventions, Reviews};
