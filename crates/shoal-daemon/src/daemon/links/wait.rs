//! Whether an idle agent is due a wake for its messages and linked items'
//! activity, and the watches' tests across subsystems.

mod elsewhere;
mod merge_runs;
mod start;
mod wake;

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
