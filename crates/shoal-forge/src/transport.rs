//! How requests reach a forge and as whom: REST through a CLI's login or
//! curl, files fetched the same ways, the programs that carry them, and the
//! account each request is signed as, whose memory is this transport's cache.
pub mod account;
pub(crate) mod api;
pub(crate) mod download;
pub mod limits;
pub mod memory;
pub mod programs;
pub(crate) mod resend;
pub(crate) mod response;
