//! Native machine load readings. Unavailable or malformed readings are errors,
//! never evidence that an agent should be stopped.
mod cpu;
mod memory;

pub use cpu::{CpuTicks, cpu_ticks};
pub use memory::{
    Memory, MemoryPressure, MemoryReading, SampledWork, Stall, WorkLimit, WorkMemory, memory, ram,
};
pub(crate) use memory::{MemoryLevel, MemoryLevels};
