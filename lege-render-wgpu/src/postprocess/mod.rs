//! GPU execution of exprim's postprocess graph, and the adaptive policy that
//! picks the GPU or exprim's normative CPU executor per graph.

mod adaptive;
#[cfg(feature = "gpu")]
mod gpu;

pub use adaptive::{
    AdaptivePostprocess, ExecutionBackend, ExecutionStats, FallbackReason, PostprocessPreference,
    PostprocessReport,
};
#[cfg(feature = "gpu")]
pub use gpu::WgpuPostprocess;
