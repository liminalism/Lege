//! Shared types for color processing and binarization
use serde::{Deserialize, Serialize};

/// Per-image binarization options, derived from `BinarizationConfig`.
///
/// There is one method: the binarizer model on the GPU, with the adaptive
/// CPU algorithm where no hardware GPU is available.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BinarizationOptions {
    /// Invert output (white on black).
    pub invert: bool,
    /// The page is light on dark: binarize it as its inverse.
    pub invert_input: bool,
    /// Blank pages only: a fixed global threshold instead of the binarizer,
    /// so that paper noise comes out white. Set by the pipeline when the CPU
    /// fallback is in use; the model does not need it.
    #[serde(default)]
    pub blank_threshold: Option<u8>,
    /// Use the CPU fallback even when the GPU model is available.
    #[serde(default)]
    pub disable_gpu: bool,
    /// "Fast": the adaptive algorithm outright, in place of the model.
    #[serde(default)]
    pub adaptive: bool,
}

/// Binarization settings of a job.
#[derive(Clone, Debug, Default)]
pub struct BinarizationConfig {
    /// Invert output (black on white vs. white on black).
    pub invert: bool,
    /// Invert input grayscale before thresholding (for inverted documents).
    pub invert_input: bool,
    /// Use the adaptive CPU fallback even when the GPU model is available.
    /// No user switch sets this; tests and diagnostics do.
    pub force_cpu: bool,
    /// "Fast" instead of "Best": the adaptive algorithm on the CPU, about
    /// twice the pages per second of the model at reading resolutions. For
    /// bulk jobs; the model is the default because it is cleaner on scans.
    pub fast: bool,
}
