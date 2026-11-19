//! Native machine load readings. Unavailable or malformed readings are errors,
//! never evidence that an agent should be stopped.
mod cpu;
mod memory;

pub use cpu::{CpuTicks, cpu_ticks};
pub use memory::{Memory, critical_memory, memory, memory_warning, safe_memory};
