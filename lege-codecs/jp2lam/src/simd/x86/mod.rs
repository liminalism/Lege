//! x86 runtime dispatch layer for primitive kernels.
//!
//! This mirrors the bpg-rs shape: portable `wide` kernels are installed first
//! (with the `simd` feature), then x86-specific kernels may overwrite
//! individual entries when runtime CPU feature detection proves they are
//! available. The AVX2 overrides are the per-row kernels of the streaming
//! inverse 9/7 and the packed row packer (`Kernel::Avx2`), where 256-bit lanes
//! are a measured win over `wide`'s two 128-bit halves; they depend only on
//! `std::arch`, so they are installed with or without the `simd` feature.

use super::Primitives;

pub(crate) fn setup(primitives: &mut Primitives, mode: &str) {
    if avx2_enabled(mode) && avx2_available() {
        avx2::setup(primitives);
    }
}

fn avx2_enabled(mode: &str) -> bool {
    matches!(mode, "auto" | "simd" | "x86" | "avx2")
}

#[cfg(target_arch = "x86")]
fn avx2_available() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

#[cfg(target_arch = "x86_64")]
fn avx2_available() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

mod avx2 {
    use super::Primitives;
    use crate::simd::Kernel;

    pub(super) fn setup(primitives: &mut Primitives) {
        primitives.dwt.inverse_97_2d_owned = inverse_97_2d_owned;
        primitives.kernel = Kernel::Avx2;
        primitives.backend = "avx2";
    }

    fn inverse_97_2d_owned(
        data: Vec<f32>,
        width: usize,
        height: usize,
        levels: u8,
    ) -> crate::error::Result<Vec<f32>> {
        Ok(crate::dwt::inverse_97_2d_even(
            data,
            width,
            height,
            levels,
            Kernel::Avx2,
            None,
        ))
    }
}
