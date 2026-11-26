//! Each forge's implementations of the traits that cover what forges do
//! differently. No other module names this one.
pub mod bitbucket;
pub(crate) mod forgejo;
pub(crate) mod github;
mod github_rest;
pub(crate) mod gitlab;
pub(crate) mod rest;
