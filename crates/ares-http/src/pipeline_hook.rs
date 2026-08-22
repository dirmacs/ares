//! Cycle-break: HTTP/trigger code cannot name `ares-server::pipeline`.
//! Re-exports [`ares_agent::pipeline`] fan-out types.

#[cfg(feature = "postgres")]
pub use ares_agent::pipeline::{PipelineFanout, PipelineFanoutHandle, PipelineOrigin};
