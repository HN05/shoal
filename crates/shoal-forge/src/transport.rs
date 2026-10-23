//! How requests reach a forge: REST through a CLI's login or curl, and the
//! programs that carry them.
pub(crate) mod api;
pub mod programs;
pub(crate) mod resend;
