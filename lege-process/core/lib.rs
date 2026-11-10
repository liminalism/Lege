// lib.rs
// Module declarations

// Several shared fallbacks compile cleanly on Android and then misbehave
// silently rather than failing: `get_available_ram_gb` falls past its
// Windows/Linux arms to a hardcoded 8 GB, and `app_dirs::data_dir` resolves to
// `PathBuf::from(".")` once every `dirs::*` lookup returns `None`. The platform
// feature therefore must not be optional — an unfeatured Android build is
// rejected here rather than shipped.
#[cfg(all(target_os = "android", not(feature = "android")))]
compile_error!("building lege for Android requires the `android` feature (--features android)");

// And the converse, so the feature cannot be switched on for a desktop build
// where its platform dependencies (android_logger, Android libc) do not exist.
#[cfg(all(feature = "android", not(target_os = "android")))]
compile_error!(
    "the `android` feature is only valid for Android targets \
     (--target aarch64-linux-android)"
);

#[cfg(feature = "android")]
#[path = "../android/mod.rs"]
pub mod android;

/// Book layout model, embedded so layout detection works with no external
/// asset: RF-DETR-S (DINOv2-S backbone + deformable DETR decoder, Apache-2.0)
/// fine-tuned on Lege's own book corpus (v2, 2026-11-10; see
/// `models/layout-rfdetr-s.md`), exported to the PP-DocLayout contract
/// (`pp_image` [1,3,640,640] in; xyxy boxes and PP-23 class scores out) with
/// fp16 weights that the runtime widens to f32 at load. A `layout.onnx` (or
/// legacy `doclayout.onnx`) runtime asset overrides it. The previous model,
/// PP-DocLayout-M, is no longer compiled in unless the `doclayout-m` feature
/// asks for it.
#[cfg(all(feature = "layout-detection", not(feature = "doclayout-m")))]
pub static EMBEDDED_LAYOUT_MODEL: &[u8] = include_bytes!("../models/layout-rfdetr-s.onnx");

/// PP-DocLayout-M (fp16), the layout model before 2026-11-08; opt-in via the
/// `doclayout-m` feature.
#[cfg(all(feature = "layout-detection", feature = "doclayout-m"))]
pub static EMBEDDED_LAYOUT_MODEL: &[u8] = include_bytes!("../models/doclayout.onnx");

/// The DIBCO Sauvola model, the heavy binarizer before the tileable one.
/// Embedded only with the `sauvola-model` feature, as the CPU fallback of
/// heavy binarization; a `sauvola.prepared.onnx` or `sauvola.onnx` runtime
/// asset is used for that in any build.
#[cfg(feature = "sauvola-model")]
pub static EMBEDDED_SAUVOLA_MODEL: &[u8] = include_bytes!("../models/sauvola.onnx");

/// Tileable heavy binarizer (TileNet v1, 409k parameters), distilled from the
/// Sauvola model plus DIBCO 2009-2016 on Colab (2026-11-07). Same I/O
/// contract as `sauvola.onnx` (`img01_inp` NHWC grey/255 in, >0.5 = paper out),
/// but fully convolutional with no global statistics, so it runs on the wgpu
/// executor, and on the CPU executor when the GPU is off or fails. Spatial
/// dims must be a multiple of 8. A `binarizer-tilenet.onnx` runtime asset
/// overrides it; `LEGE_HEAVY_BINARIZER=sauvolanet` keeps heavy binarization
/// off the GPU.
pub static EMBEDDED_TILENET_BINARIZER: &[u8] = include_bytes!("../models/binarizer-tilenet.onnx");

pub mod accumulator;
pub mod app_dirs;
pub mod bbox_trace;
pub mod clean_gray; // Grayscale scanned-page cleanup + MRC split (rounds 1-7)
pub mod cli_progress;
#[path = "../color/mod.rs"]
pub mod color;
#[path = "../colorquant/mod.rs"]
pub mod colorquant;
pub mod debug_log;
pub mod djvu; // Native Rust DJVU encoder
#[path = "../encoding/mod.rs"]
pub mod encoding;
#[cfg(feature = "layout-detection")]
#[cfg_attr(target_os = "linux", path = "engine_impl.rs")]
#[cfg_attr(not(target_os = "linux"), path = "engine.rs")]
pub mod engine;
#[cfg(not(feature = "layout-detection"))]
#[path = "engine_disabled.rs"]
pub mod engine;
pub mod glyphless_font; // Minimal glyphless TTF for the invisible OCR text layer
pub mod hocr; // hOCR parsing + line grouping (moved out of accumulator)
pub mod icon;
#[cfg(feature = "layout-detection")]
#[cfg(feature = "layout-detection")]
pub mod layout_visualize;
#[cfg(not(feature = "layout-detection"))]
#[path = "layout_visualize_disabled.rs"]
pub mod layout_visualize;
pub mod margin;
pub mod nms;
#[cfg(feature = "ocr")]
#[path = "../ocr/mod.rs"]
pub mod ocr;
#[cfg(not(feature = "ocr"))]
#[path = "ocr_disabled.rs"]
pub mod ocr;
pub mod pagerender;
pub mod pdf_artifact; // accumulator::Page -> lege_pdf_write::PdfPageArtifact adapter
pub mod pdf_to_png;
#[path = "../pipeline/mod.rs"]
pub mod pipeline; // Modular pipeline actions (includes config, inference, processing)
pub mod pnginference;
pub mod preprocess;
pub mod processing_log;
pub mod progress;
#[path = "../reflow/mod.rs"]
pub mod reflow; // RasterReflow: raster-first reflow for scanned books (scaffolding)
#[path = "../resize/mod.rs"]
pub mod resize;
pub mod resolution_preset;
pub mod runtime_stats;
pub mod truetype_writer; // sfnt assembler shared by the glyphless and glyph fonts
// resize context is now merged into pipeline::policies
pub mod text_loader;
pub mod toc; // Automatic table of contents: title capture + outline synthesis
pub mod types;
pub mod unicode_font;
pub mod windows_dirs;
pub use layout_visualize::{
    run_image_debug_mode, run_layout_visualize_image, run_layout_visualize_mode,
};
pub use pdf_to_png::{run_pdf_to_jp2_debug_mode, run_pdf_to_png_mode};
pub use pnginference::{
    DebugCropKind, run_folder_layout_crop_debug, run_images_to_images_mode,
    run_pdf_layout_crop_debug, run_pdf_to_images_mode, run_png_mode, run_png_mode_with_config,
    run_zip_mode_with_config,
}; // Re-export for CLI debug cropping and new image modes

// Re-export common image-encoding functionality.
pub use color::DEFAULT_K_FACTOR;
pub use encoding::jpeg::{EncodeOptions, ImageFormat, encode_jpeg};

// Re-export key types from pipeline submodules for convenience
pub use pipeline::{
    // Config types
    ImageRegionDitherMode,
    // Inference types
    InferenceHandle,
    PageRange,
    PageSelection,
    PageTask,
    PipelineConfig,
    ProcessingPipeline,
    RenderedPageData,
    ShutdownReason,
    // Helper functions and types
    ShutdownSignal,
    build_hocr_from_pdf_text,
    get_available_ram_gb,
    is_ocr_available,
    // Other
    runtime_asset_path,
    runtime_asset_path_if_exists,
    should_treat_as_cover_page,
};

// #[global_allocator]
// static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Profiling log with timing information (enabled only with debug-logging feature)
#[macro_export]
#[cfg(feature = "debug-logging")]
macro_rules! perf_log {
    ($start:expr, $($arg:tt)*) => {{
        let duration = $start.elapsed();
        println!(
            "[PERF] {} (took {:?})",
            format_args!($($arg)*),
            duration
        );
    }};
}

/// No-op version of perf_log when debug-logging is disabled
#[macro_export]
#[cfg(not(feature = "debug-logging"))]
macro_rules! perf_log {
    ($start:expr, $($arg:tt)*) => {{
        // Reference both the timer and the message arguments, so bindings that exist
        // only for this log do not read as unused. `if false` keeps them unevaluated.
        let _ = &$start;
        if false {
            let _ = ::core::format_args!($($arg)*);
        }
    }};
}

// Map legacy logging macros to current helpers
#[cfg(feature = "debug-logging")]
#[macro_export]
macro_rules! info_log {
    ($($arg:tt)*) => { $crate::info_println!($($arg)*) }
}

#[cfg(not(feature = "debug-logging"))]
#[macro_export]
macro_rules! info_log {
    ($($arg:tt)*) => {{
        // Still reference the arguments so a binding used only by this log does not
        // read as unused with the feature off. `if false` keeps them unevaluated.
        if false {
            let _ = ::core::format_args!($($arg)*);
        }
    }};
}

#[cfg(feature = "debug-logging")]
#[macro_export]
macro_rules! success_log {
    ($($arg:tt)*) => { $crate::info_println!($($arg)*) }
}

#[cfg(not(feature = "debug-logging"))]
#[macro_export]
macro_rules! success_log {
    ($($arg:tt)*) => {{
        // Still reference the arguments so a binding used only by this log does not
        // read as unused with the feature off. `if false` keeps them unevaluated.
        if false {
            let _ = ::core::format_args!($($arg)*);
        }
    }};
}
#[macro_export]
macro_rules! error_log {
    ($($arg:tt)*) => { $crate::error_println!($($arg)*) }
}

#[cfg(feature = "debug-logging")]
#[macro_export]
macro_rules! warn_log {
    ($($arg:tt)*) => {{
        // Don't use colors in macros that may be called from GUI
        println!(
            "[{}] WARNING: {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            format!($($arg)*)
        );
    }};
}

#[cfg(not(feature = "debug-logging"))]
#[macro_export]
macro_rules! warn_log {
    ($($arg:tt)*) => {{
        // Still reference the arguments so a binding used only by this log does not
        // read as unused with the feature off. `if false` keeps them unevaluated.
        if false {
            let _ = ::core::format_args!($($arg)*);
        }
    }};
}

#[cfg(feature = "debug-logging")]
#[macro_export]
macro_rules! dprintln {
    ($($arg:tt)*) => {
        /* Temporarily disabled to focus on JP2 debug logs */
    };
}

#[cfg(not(feature = "debug-logging"))]
#[macro_export]
macro_rules! dprintln {
    ($($arg:tt)*) => {{
        // Still reference the arguments so a binding used only by this log does not
        // read as unused with the feature off. `if false` keeps them unevaluated.
        if false {
            let _ = ::core::format_args!($($arg)*);
        }
    }};
}

/// `LEGE_BBOX_TRACE=1` → stderr lines for layout bboxes, region encodes, PDF `Do` placement.
#[macro_export]
macro_rules! bbox_trace {
    ($($arg:tt)*) => {{
        if $crate::bbox_trace::enabled() {
            eprintln!($($arg)*);
        }
    }};
}

// DAG-related imports
// Added missing imports and type aliases
// brings debug_log!, info_println!, error_println!
// resize params now centralized in pipeline::policies for inference path; keep legacy resize module for other callers
pub use crate::types::{AppConfig, CliConfigBuilder, CoverFormat};
// The `fast_image_resize` re-imports that used to sit here were unused and only
// compiled because of `#[allow(unused_imports)]`. The resize backend owns those
// types; see `crate::resize`.
// Action types are re-exported from the top-level pub use pipeline::...

// ShutdownReason and ShutdownSignal are now in pipeline::helper_functions

/// Returns the external version string (e.g., "1.1.4.0") as set during build time
pub fn get_external_version() -> &'static str {
    option_env!("LEGE_EXTERNAL_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// Returns the internal package version from Cargo.toml
pub fn get_internal_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Configure runtime library environment for dynamically loaded dependencies.
pub fn configure_runtime_env() {
    configure_rayon_runtime();
}

fn configure_rayon_runtime() {
    static INIT: std::sync::Once = std::sync::Once::new();

    INIT.call_once(|| {
        let threads = rayon_thread_count();
        let mut builder =
            rayon::ThreadPoolBuilder::new().thread_name(|idx| format!("lege-rayon-{idx}"));
        if let Some(threads) = threads {
            builder = builder.num_threads(threads);
        }

        let _ = builder.build_global();
    });
}

fn rayon_thread_count() -> Option<usize> {
    if let Some(threads) = parse_positive_env_usize("LEGE_RAYON_THREADS") {
        return Some(threads);
    }

    if std::env::var_os("RAYON_NUM_THREADS").is_some() {
        return None;
    }

    let cores = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2)
        .max(1);
    Some(cores.saturating_sub(1).max(1))
}

fn parse_positive_env_usize(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
}
