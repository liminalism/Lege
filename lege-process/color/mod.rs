//! Color Operations Module
//!
//! Image processing library for binarization, grayscale processing, and dithering.
//! Text is binarized by a model on the GPU, with a CPU fallback.

// Private modules - not exposed in public API
pub mod linearize;
mod types;

// Public modules - only keeping essential modules for 1-bit processing
pub mod binarization;
pub mod color_processing;

// Re-export necessary types and traits
// Re-export key types from top-level types module
pub use types::{BinarizationConfig, BinarizationOptions};

// Export essential binarization utilities
pub use binarization::{
    HeavySauvolaProcessor, binarize_gray, binarize_image, binarize_image_raw, binarizes_on_gpu,
};

// Export color processing utilities
pub use color_processing::{ImageRegionDitherMode, process_image_region};

// GPU-related convenience functions removed - using direct binarization and dithering functions instead
