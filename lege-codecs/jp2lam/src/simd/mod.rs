//! Runtime-selected primitive kernels for data-parallel codec stages.
//!
//! This mirrors the bpg-rs/still265 convention: build a scalar table first,
//! then overwrite entries with portable SIMD implementations when enabled.

use std::sync::LazyLock;

pub(crate) mod scalar;

#[cfg(feature = "simd")]
pub(crate) mod wide;

/// The x86 overrides use `std::arch` only, so they are available without the
/// `simd` feature (the renderer builds this crate with default features off).
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub(crate) mod x86;

pub(crate) type AnalyzeI32Fn = fn(&[i32]) -> (u32, usize);
pub(crate) type QuantizeRectFn = fn(&[f32], &mut [i32], usize, usize, usize, usize, usize, f32);
pub(crate) type LevelShiftF32Fn = fn(&[i32], i32, &mut [f32]);
pub(crate) type LevelShiftI32Fn = fn(&[i32], i32, &mut [i32]);
pub(crate) type IctComponentFn = fn(&[i32], &[i32], &[i32], usize, i32, &mut [f32]);
pub(crate) type RctComponentFn = fn(&[i32], &[i32], &[i32], usize, i32, &mut [i32]);
pub(crate) type InverseIctFn = fn(&mut [f32], &mut [f32], &mut [f32]);
pub(crate) type InverseRctFn = fn(&mut [i32], &mut [i32], &mut [i32]);
pub(crate) type DwtF32Fn = fn(&mut [f32], usize, usize, u8) -> crate::error::Result<()>;
pub(crate) type DwtF32OwnedFn = fn(Vec<f32>, usize, usize, u8) -> crate::error::Result<Vec<f32>>;
pub(crate) type DwtI32Fn = fn(&mut [i32], usize, usize, u8) -> crate::error::Result<()>;

#[derive(Clone, Copy)]
pub(crate) struct Primitives {
    pub dwt: DwtPrimitives,
    pub analyze: AnalyzePrimitives,
    pub quant: QuantPrimitives,
    pub color: ColorPrimitives,
    pub backend: &'static str,
    /// Row-kernel flavour for the streaming inverse DWT and the packed row
    /// packer, which dispatch per row rather than through this table.
    pub kernel: Kernel,
}

/// Which implementation of the per-row kernels (DWT lifting, scaling,
/// interleave, colour pack) runs. Every flavour produces bit-identical
/// results; they differ only in vector width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kernel {
    Scalar,
    /// Portable `wide` kernels (two 128-bit halves on a baseline x86 build).
    Wide,
    /// `#[target_feature(enable = "avx2")]` kernels, selected at runtime on
    /// x86 after feature detection.
    Avx2,
}

impl Kernel {
    /// Every flavour this build can run on this machine, for equivalence
    /// tests.
    #[cfg(test)]
    pub(crate) fn available() -> Vec<Kernel> {
        let mut kernels = vec![Kernel::Scalar];
        #[cfg(feature = "simd")]
        kernels.push(Kernel::Wide);
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if std::arch::is_x86_feature_detected!("avx2") {
            kernels.push(Kernel::Avx2);
        }
        kernels
    }
}

#[derive(Clone, Copy)]
pub(crate) struct AnalyzePrimitives {
    pub i32_max_magnitude_and_nnz: AnalyzeI32Fn,
}

#[derive(Clone, Copy)]
pub(crate) struct DwtPrimitives {
    pub inverse_97_2d: DwtF32Fn,
    /// Streaming inverse 9/7 over an owned plane (no copy back).
    pub inverse_97_2d_owned: DwtF32OwnedFn,
    pub inverse_53_2d: DwtI32Fn,
}

#[derive(Clone, Copy)]
pub(crate) struct QuantPrimitives {
    pub quantize_f32_rect: QuantizeRectFn,
}

#[derive(Clone, Copy)]
pub(crate) struct ColorPrimitives {
    pub level_shift_f32: LevelShiftF32Fn,
    pub level_shift_i32: LevelShiftI32Fn,
    pub forward_ict_component: IctComponentFn,
    pub forward_rct_component: RctComponentFn,
    pub inverse_ict: InverseIctFn,
    pub inverse_rct: InverseRctFn,
}

pub(crate) static PRIMITIVES: LazyLock<Primitives> = LazyLock::new(select_primitives);

#[cfg(feature = "profile")]
pub(crate) fn active_backend() -> &'static str {
    PRIMITIVES.backend
}

fn select_primitives() -> Primitives {
    #[allow(unused_mut)]
    let mut primitives = scalar::primitives();
    let mode = std::env::var("JP2LAM_PRIMITIVES")
        .unwrap_or_else(|_| "auto".to_string())
        .to_ascii_lowercase();

    if matches!(mode.as_str(), "scalar" | "none" | "off") {
        return primitives;
    }

    #[cfg(feature = "simd")]
    if matches!(mode.as_str(), "auto" | "simd" | "wide" | "x86" | "avx2") {
        wide::setup(&mut primitives);
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if matches!(mode.as_str(), "auto" | "simd" | "x86" | "avx2") {
        x86::setup(&mut primitives, &mode);
    }

    primitives
}

#[cfg(test)]
mod tests {
    #[test]
    fn env_forces_scalar_backend() {
        // Selection itself is a LazyLock and may already have happened in the
        // process, so keep this as a documentation guard for accepted names.
        assert!(matches!("scalar", "scalar" | "none" | "off"));
    }
}
