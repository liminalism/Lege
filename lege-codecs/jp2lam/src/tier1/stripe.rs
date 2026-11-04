//! Packed stripe-column Tier-1 coding state for the decoder.
//!
//! One `u32` per column of a four-row stripe holds everything the three
//! coding passes ask about that column (the OpenJPEG `opj_flag_t` layout):
//!
//! * bits 0..18 — significance of the 3 × 6 neighbourhood: three columns
//!   (west, this, east) by six rows (the row above the stripe, its four rows,
//!   the row below), bit `3 * row + column`. Shifting the word right by
//!   `3 * ci` lines sample `ci`'s own 3 × 3 neighbourhood up with the
//!   [`T1_SIGMA_NW`](super::consts::T1_SIGMA_NW) … `T1_SIGMA_SE` bits, so the
//!   zero-coding LUT indexes it directly, and `T1_SIGMA_THIS` (bit 4) is the
//!   sample itself.
//! * bits 18, 19 + 3 ci, 31 — `CHI`: the sign of the row above, of sample
//!   `ci`, and of the row below.
//! * bit 20 + 3 ci — `MU`: sample `ci` has been refined before.
//! * bit 21 + 3 ci — `PI`: sample `ci` was coded in this bit-plane's
//!   significance pass.
//!
//! A word of zero means "nothing in this column or next to it has happened",
//! which is exactly the cleanup pass's run-length condition and lets the
//! significance and refinement passes skip the column with one test. The
//! grid has one padding column on each side and one padding stripe above
//! and below, so neighbour updates never leave the grid.

/// The sample itself, for `ci = 0`.
pub(crate) const T1_SIGMA_THIS: u32 = 1 << 4;
/// Row-below neighbours (south-west, south, south-east of `ci = 3`).
pub(crate) const T1_SIGMA_15: u32 = 1 << 15;
pub(crate) const T1_SIGMA_16: u32 = 1 << 16;
pub(crate) const T1_SIGMA_17: u32 = 1 << 17;
/// Sign of the row above the stripe.
pub(crate) const T1_CHI_0_I: u32 = 18;
/// Sign of sample `ci = 0`; sample `ci` is at `T1_CHI_1_I + 3 * ci`.
pub(crate) const T1_CHI_1_I: u32 = 19;
/// Sign of sample `ci = 1` (the south neighbour of `ci = 0`).
pub(crate) const T1_CHI_2_I: u32 = 22;
/// Sign of the row below the stripe.
pub(crate) const T1_CHI_5_I: u32 = 31;
/// Refinement history of sample `ci = 0`.
pub(crate) const T1_MU_0: u32 = 1 << 20;
/// Significance-pass membership of sample `ci = 0`.
pub(crate) const T1_PI_0: u32 = 1 << 21;
/// All four `PI` bits.
pub(crate) const T1_PI_ALL: u32 = T1_PI_0 | (T1_PI_0 << 3) | (T1_PI_0 << 6) | (T1_PI_0 << 9);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StripeFlags {
    stride: usize,
    words: Vec<u32>,
}

impl StripeFlags {
    pub(crate) fn new() -> Self {
        Self {
            stride: 0,
            words: Vec::new(),
        }
    }

    /// Reuse the backing allocation for a `width × height` code block.
    pub(crate) fn reset(&mut self, width: usize, height: usize) {
        self.stride = width + 2;
        let rows = height.div_ceil(4) + 2;
        self.words.resize(self.stride * rows, 0);
        self.words.fill(0);
    }

    #[inline(always)]
    pub(crate) fn stride(&self) -> usize {
        self.stride
    }

    #[inline(always)]
    pub(crate) fn words_mut(&mut self) -> &mut [u32] {
        &mut self.words
    }

    /// Whether any sample still carries a significance-pass membership bit;
    /// the cleanup pass must clear them all.
    pub(crate) fn has_visited(&self) -> bool {
        self.words.iter().any(|&word| word & T1_PI_ALL != 0)
    }
}
