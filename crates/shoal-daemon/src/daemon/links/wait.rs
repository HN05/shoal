//! Whether an idle agent is due a wake for its messages and linked items'
//! activity, and the watches' tests across subsystems.

mod elsewhere;
mod merge_runs;
mod wake;

#[cfg(any(test, feature = "test-support"))]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
