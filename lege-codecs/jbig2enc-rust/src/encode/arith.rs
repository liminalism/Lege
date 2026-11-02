// SPDX-License-Identifier: Apache-2.0
// Includes arithmetic-coder work from jbig2enc:
// Copyright 2006 Google Inc. All Rights Reserved.
// Author: Adam Langley (agl@imperialviolet.org).
// Rust translation and later changes are described in PROVENANCE.md.
// Apache-2.0 terms and upstream notices: LICENSE-APACHE and NOTICE.

// src/arithmetic_coder.rs

//! A pure Rust port of the context-adaptive arithmetic coder from jbig2enc.
//! This module provides low-level bit-level encoding for JBIG2, mirroring the
//! functionality of `jbig2arith.cc` and `jbig2arith.h`. It is designed to be
//! integrated into a larger JBIG2 encoding pipeline where binarization and
//! preprocessing are already handled.

use anyhow::Result;
use anyhow::anyhow;
#[cfg(debug_assertions)]
use std::collections::HashMap;

const JBIG2_MAX_CTX: usize = 65536;

/// One probability-estimation state (ISO/IEC 14492 Table E.1)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    /// Qe value (16 bit)
    pub qe: u16,
    /// next state if the coded symbol was the **MPS**
    pub nmps: u8,
    /// next state if the coded symbol was the **LPS**
    pub nlps: u8,
    /// if 1, toggle the current MPS after coding an LPS
    pub switch: bool,
}

/// Context for arithmetic encoding/decoding
pub type ArithContext = usize;

/// Parameters for encoding integer ranges.
struct IntEncRange {
    bot: i32,    // Lower bound of the range
    top: i32,    // Upper bound of the range
    data: u8,    // Prefix bits to encode
    bits: u8,    // Number of prefix bits
    delta: u32,  // Value to subtract before encoding
    intbits: u8, // Number of bits for the integer part
}

/// Table defining how to encode integers of various ranges.
const INT_ENC_RANGE: [IntEncRange; 13] = [
    IntEncRange {
        bot: 0,
        top: 3,
        data: 0,
        bits: 2,
        delta: 0,
        intbits: 2,
    },
    IntEncRange {
        bot: -1,
        top: -1,
        data: 9,
        bits: 4,
        delta: 0,
        intbits: 0,
    },
    IntEncRange {
        bot: -3,
        top: -2,
        data: 5,
        bits: 3,
        delta: 2,
        intbits: 1,
    },
    IntEncRange {
        bot: 4,
        top: 19,
        data: 2,
        bits: 3,
        delta: 4,
        intbits: 4,
    },
    IntEncRange {
        bot: -19,
        top: -4,
        data: 3,
        bits: 3,
        delta: 4,
        intbits: 4,
    },
    IntEncRange {
        bot: 20,
        top: 83,
        data: 6,
        bits: 4,
        delta: 20,
        intbits: 6,
    },
    IntEncRange {
        bot: -83,
        top: -20,
        data: 7,
        bits: 4,
        delta: 20,
        intbits: 6,
    },
    IntEncRange {
        bot: 84,
        top: 339,
        data: 14,
        bits: 5,
        delta: 84,
        intbits: 8,
    },
    IntEncRange {
        bot: -339,
        top: -84,
        data: 15,
        bits: 5,
        delta: 84,
        intbits: 8,
    },
    IntEncRange {
        bot: 340,
        top: 4435,
        data: 30,
        bits: 6,
        delta: 340,
        intbits: 12,
    },
    IntEncRange {
        bot: -4435,
        top: -340,
        data: 31,
        bits: 6,
        delta: 340,
        intbits: 12,
    },
    IntEncRange {
        bot: 4436,
        top: 2_000_000_000,
        data: 62,
        bits: 6,
        delta: 4436,
        intbits: 32,
    },
    IntEncRange {
        bot: -2_000_000_000,
        top: -4436,
        data: 63,
        bits: 6,
        delta: 4436,
        intbits: 32,
    },
];

// `IntProc` is shared protocol data (the JBIG2 IA* procedure identifiers) and
// now lives in the shared module. Re-exported to keep the historical
// `crate::jbig2arith::IntProc` path working (Phase 0 / §11).
pub use crate::shared::int_proc::IntProc;

/// Table E.1 – indices 0 … 46 (MPS = 0 half).
///
/// Derived from the canonical rows in [`crate::shared::mq_table`] so the
/// encoder and decoder share one source of truth (Phase 0 / §10). The values
/// are identical to the historical literal table.
pub const BASE: [State; 47] = build_base();

const fn build_base() -> [State; 47] {
    let mut t = [State {
        qe: 0,
        nmps: 0,
        nlps: 0,
        switch: false,
    }; 47];
    let mut i = 0;
    while i < 47 {
        let (qe, nmps, nlps, switch) = crate::shared::mq_table::BASE_ROWS[i];
        t[i] = State {
            qe,
            nmps,
            nlps,
            switch,
        };
        i += 1;
    }
    t
}

/// Context-adaptive arithmetic encoder for JBIG2.
const NUM_REFINEMENT_CONTEXTS: usize = 1 << 13; // 8192 for GRTEMPLATE=0 (13-bit context)
/// Initial index into CTBL for new contexts, typically 0 (MPS=0, Qe=0x5601).

#[derive(Clone)]
pub struct Jbig2ArithCoder {
    registers: ArithRegisters,
    context: Vec<u32>, // Packed bitmap state: table index in high16, cached Qe in low16.
    int_ctx: [[usize; 512]; 13], // Contexts for integer encoding, storing CTBL indices
    iaid_ctx: Vec<usize>, // Dynamically sized context tree for IAID symbols
    refinement_contexts: Vec<usize>, // Contexts for refinement region (GRTEMPLATE=0: 8192 states)
}

// Separate arithmetic state and bitmap statistics so the generic loop can borrow
// them independently; neither can alias the other through the coder handle.
#[derive(Clone)]
struct ArithRegisters {
    // Native-width storage avoids overlapping narrow stores and widened loads.
    // Every update preserves the original 16-bit arithmetic semantics.
    a: u32,        // Range register A (16-bit value in a native register)
    c: u32,        // Code register C
    b: u8,         // Current byte being built
    ct: i8,        // Countdown register CT
    bp: isize,     // Byte position in output
    data: Vec<u8>, // Output data
}

impl Jbig2ArithCoder {
    #[inline(always)]
    fn encode_bit_with_state_idx(&mut self, state_idx: usize, d: bool) -> usize {
        self.registers.encode_bit_with_state_idx(state_idx, d)
    }
    fn renorm(&mut self) {
        self.registers.renorm();
    }
    fn byte_out(&mut self) {
        self.registers.byte_out();
    }
    pub(crate) fn flush_with_termination(&mut self, with_marker: bool, canonical: bool) {
        self.registers
            .flush_with_termination(with_marker, canonical);
    }

    /// Encodes a generic region payload using high-level config, returning the raw arithmetically coded data.
    pub fn encode_generic_payload_cfg(
        image: &crate::jbig2sym::BitImage,
        cfg: &crate::jbig2structs::GenericRegionConfig,
    ) -> Result<Vec<u8>> {
        Self::encode_generic_payload_cfg_with_termination(image, cfg, false)
    }

    pub(crate) fn encode_generic_payload_cfg_with_termination(
        image: &crate::jbig2sym::BitImage,
        cfg: &crate::jbig2structs::GenericRegionConfig,
        canonical: bool,
    ) -> Result<Vec<u8>> {
        // Validate config
        cfg.validate().map_err(|e| anyhow::anyhow!(e))?;
        // Use the AT pixels from config - the config now has the correct defaults for template 0
        let at_pixels = cfg.at_pixels.clone();
        // Delegate to existing payload encoder
        Self::encode_generic_payload_with_termination(image, cfg.template, &at_pixels, canonical)
    }

    /// Encodes a generic region (internal) payload using high-level config, returning the raw arithmetically coded data.
    pub fn encode_generic_region_cfg(
        &mut self,
        packed_data: &[u32],
        cfg: &crate::jbig2structs::GenericRegionConfig,
    ) -> Result<()> {
        // Validate config
        cfg.validate().map_err(|e| anyhow::anyhow!(e))?;
        // Prepare parameters
        let width = cfg.width as usize;
        let height = cfg.height as usize;
        let template = cfg.template;
        // Use the AT pixels from config - the config now has the correct defaults for template 0
        let at_pixels = cfg.at_pixels.clone();
        // Use only up to 4 AT pixels - for template 0, this will use the default values
        let mut at = [(0i8, 0i8); 4];
        for (i, &p) in at_pixels.iter().take(4).enumerate() {
            at[i] = p;
        }
        // Delegate to existing inner encoder
        self.encode_generic_region_inner(packed_data, width, height, template, &at)
    }
    /// Returns a reference to the internal output buffer as a byte slice.
    pub fn as_bytes(&self) -> &[u8] {
        &self.registers.data
    }
    const INITIAL_STATE: u32 = crate::shared::mq_table::MQ_STATES[0].qe as u32;

    /// Creates a new arithmetic encoder with initial state.
    pub fn new() -> Self {
        let mut coder = Self {
            registers: ArithRegisters {
                a: 0,
                c: 0,
                b: 0,
                ct: 0,
                bp: 0,
                data: Vec::new(),
            },
            context: vec![Self::INITIAL_STATE; JBIG2_MAX_CTX],
            int_ctx: [[0; 512]; 13],
            iaid_ctx: Vec::new(),
            refinement_contexts: vec![0; NUM_REFINEMENT_CONTEXTS],
        };
        coder.reset();
        coder
    }

    /// Resets the encoder to its initial state, clearing output and contexts.
    pub fn reset(&mut self) {
        self.registers.a = 0x8000;
        self.registers.c = 0;
        self.registers.ct = 12;
        self.registers.b = 0;
        self.registers.bp = -1;
        self.registers.data.clear();
        self.context.fill(Self::INITIAL_STATE);
        for ctx in self.int_ctx.iter_mut() {
            ctx.fill(0);
        }
        self.iaid_ctx.fill(0);
        self.refinement_contexts.fill(0);
    }

    /// Reinitialise the MQ registers and integer/IAID statistics for a new
    /// coded stream while **preserving** the generic and generic-refinement
    /// (bitmap-coding) contexts. This is the encoder-side analogue of a symbol
    /// dictionary decoded with the "bitmap coding context used" flag (T.88
    /// §6.5.5 step 3): the next dictionary's bitmaps are coded starting from the
    /// previous dictionary's final statistics. Test-support only — it does not
    /// change the output of any normal encode path.
    pub fn reinit_registers_keep_bitmap_contexts(&mut self) {
        self.registers.a = 0x8000;
        self.registers.c = 0;
        self.registers.ct = 12;
        self.registers.b = 0;
        self.registers.bp = -1;
        self.registers.data.clear();
        for ctx in self.int_ctx.iter_mut() {
            ctx.fill(0);
        }
        self.iaid_ctx.fill(0);
        // self.context and self.refinement_contexts are intentionally preserved.
    }

    /// Finalizes the arithmetic coding stream.
    pub fn finalize(&mut self, _data: &mut Vec<u8>) -> anyhow::Result<()> {
        self.renorm();
        self.registers.c = self.registers.c.wrapping_add(self.registers.a as u32);
        self.byte_out();
        self.registers.c = self.registers.c.wrapping_add(self.registers.a as u32);
        self.byte_out();

        Ok(())
    }

    /// Finalizes the arithmetic coding stream, preserving legacy termination bytes.
    pub fn flush(&mut self, with_marker: bool) {
        self.flush_with_termination(with_marker, false);
    }

    #[inline(always)]
    fn update_prev(prev: u32, bit: bool) -> u32 {
        if (prev & 0x100) != 0 {
            (((prev << 1) | (bit as u32)) & 0x1ff) | 0x100
        } else {
            (prev << 1) | (bit as u32)
        }
    }

    /// Encodes a single bit `d` in the given context `ctx`.
    #[inline(always)]
    pub fn encode_bit(&mut self, ctx: usize, d: bool) {
        self.registers.encode_bitmap_bit(&mut self.context[ctx], d);
    }

    /// Encodes an integer `v` of `bits` width using a specific context `ctx`.
    #[inline]
    pub fn encode_int_with_ctx(&mut self, v: i32, bits: i32, ctx: IntProc) -> anyhow::Result<()> {
        if bits <= 0 || bits > 32 {
            return Err(anyhow!("encode_int_with_ctx: invalid bit width {}", bits));
        }
        let mut prev = 1u32;
        let proc_idx = ctx as usize;
        for i in (0..bits).rev() {
            let bit = ((v >> i) & 1) != 0;
            let idx = (prev & 0x1ff) as usize;
            let next_state = self.encode_bit_with_state_idx(self.int_ctx[proc_idx][idx], bit);
            self.int_ctx[proc_idx][idx] = next_state;
            prev = Self::update_prev(prev, bit);
        }
        Ok(())
    }

    /// Encodes an IAID-coded symbol identifier using the dedicated IAID context tree.
    pub fn encode_iaid(&mut self, value: u32, bits: u8) -> anyhow::Result<()> {
        if bits == 0 || bits > 24 {
            return Err(anyhow!("encode_iaid: invalid symbol id bit width {}", bits));
        }
        if value >= (1u32 << bits) {
            return Err(anyhow!(
                "encode_iaid: symbol id {} does not fit in {} bits",
                value,
                bits
            ));
        }
        let needed = 1usize << bits;
        if self.iaid_ctx.len() < needed {
            self.iaid_ctx.resize(needed, 0);
        }
        let mut prev: u32 = 1;
        let mut shifted = value << (32 - bits);
        for _ in 0..bits {
            let bit = (shifted & 0x8000_0000) != 0;
            let idx = (prev as usize).min(self.iaid_ctx.len() - 1);
            let next_state = self.encode_bit_with_state_idx(self.iaid_ctx[idx], bit);
            self.iaid_ctx[idx] = next_state;
            prev = (prev << 1) | (bit as u32);
            shifted <<= 1;
        }
        Ok(())
    }

    /// Encodes an integer using the specified procedure.
    pub fn encode_integer(&mut self, proc: IntProc, value: i32) -> anyhow::Result<()> {
        if !(-2_000_000_000..=2_000_000_000).contains(&value) {
            return Err(anyhow!("Integer value out of encodable range"));
        }
        let range_info = INT_ENC_RANGE
            .iter()
            .find(|r| r.bot <= value && r.top >= value)
            .expect("Value out of range");
        let mut v = if value < 0 { -value } else { value } as u32;
        v = v.saturating_sub(range_info.delta);

        let context_idx = proc as usize;
        let mut prev_ctx = 1u32;

        // Prefix bits are emitted LSB-first from `data`, matching jbig2enc C implementation.
        let mut prefix = range_info.data;
        for _ in 0..range_info.bits {
            let bit = (prefix & 1) != 0;
            let idx = (prev_ctx & 0x1ff) as usize;
            let next_state = self.encode_bit_with_state_idx(self.int_ctx[context_idx][idx], bit);
            self.int_ctx[context_idx][idx] = next_state;
            prev_ctx = Self::update_prev(prev_ctx, bit);
            prefix >>= 1;
        }

        // Encode the range payload as MSB-first bits.
        if range_info.intbits > 0 {
            v <<= 32 - range_info.intbits;
            for _ in 0..range_info.intbits {
                let bit = (v & 0x8000_0000) != 0;
                let idx = (prev_ctx & 0x1ff) as usize;
                let next_state =
                    self.encode_bit_with_state_idx(self.int_ctx[context_idx][idx], bit);
                self.int_ctx[context_idx][idx] = next_state;
                prev_ctx = Self::update_prev(prev_ctx, bit);
                v <<= 1;
            }
        }
        Ok(())
    }

    /// Encodes an Out-of-Band (OOB) value for a given integer procedure.
    /// The OOB signal is the bit pattern '1000' as per Table A.1 in the JBIG2 specification.
    pub fn encode_oob(&mut self, proc: IntProc) -> anyhow::Result<()> {
        let context_idx = proc as usize;
        for (idx, bit) in [
            (1usize, true),
            (3usize, false),
            (6usize, false),
            (12usize, false),
        ] {
            let next_state = self.encode_bit_with_state_idx(self.int_ctx[context_idx][idx], bit);
            self.int_ctx[context_idx][idx] = next_state;
        }

        Ok(())
    }

    /// Encodes a generic region payload, returning the raw arithmetically coded data.
    /// This is the high-level entry point for generic region encoding.
    pub fn encode_generic_region(
        &mut self,
        packed_data: &[u32],
        width: usize,
        height: usize,
        template: u8,
        _at_pixels: &[(i8, i8)],
    ) -> Result<()> {
        #[cfg(debug_assertions)]
        {
            log::debug!(
                "encode_generic_region: width={}, height={}, template={}",
                width,
                height,
                template
            );

            if template != 0 {
                log::warn!(
                    "template {} is not fully tested, only template 0 is well-supported",
                    template
                );
            }

            if width == 0 || height == 0 {
                log::warn!(
                    "empty region ({}x{}) provided to encode_generic_region",
                    width,
                    height
                );
                return Ok(());
            }

            // Verify packed data size is sufficient
            let expected_words = ((width + 31) / 32) * height;
            if packed_data.len() < expected_words {
                log::warn!(
                    "packed_data size {} is less than expected {} for {}x{} bitmap",
                    packed_data.len(),
                    expected_words,
                    width,
                    height
                );
            }
        }

        // Use the provided AT pixels for template 0 (C encoder compatibility)
        let mut at = [(0i8, 0i8); 4];
        for (i, &p) in _at_pixels.iter().take(4).enumerate() {
            at[i] = p;
        }
        self.encode_generic_region_inner(packed_data, width, height, template, &at)
    }

    pub fn encode_generic_payload(
        image: &crate::jbig2sym::BitImage,
        template: u8,
        _at_pixels: &[(i8, i8)],
    ) -> Result<Vec<u8>> {
        Self::encode_generic_payload_with_termination(image, template, _at_pixels, false)
    }

    pub(crate) fn encode_generic_payload_with_termination(
        image: &crate::jbig2sym::BitImage,
        template: u8,
        _at_pixels: &[(i8, i8)],
        canonical: bool,
    ) -> Result<Vec<u8>> {
        #[cfg(debug_assertions)]
        {
            log::debug!(
                "encode_generic_payload: image={:?}x{:?}, template={}",
                image.width,
                image.height,
                template
            );
        }

        let packed_data = image.packed_words();

        // Verify bit-order correctness: first black pixel should match between image and packed data
        if let Some(expected_first_pixel) = crate::jbig2enc::first_black_pixel(image) {
            let actual_first_pixel = crate::jbig2sym::first_black_pixel_in_packed(
                packed_data,
                image.width,
                image.height,
            );
            assert_eq!(
                actual_first_pixel,
                Some(expected_first_pixel),
                "bit-order / row-order mismatch in packer! Expected first black pixel at {:?}, got {:?}",
                expected_first_pixel,
                actual_first_pixel
            );
        }

        let mut coder = Jbig2ArithCoder::new();

        // Use provided AT pixels for context formation (C encoder compatibility)
        let mut at = [(0i8, 0i8); 4];
        for (i, &p) in _at_pixels.iter().take(4).enumerate() {
            at[i] = p;
        }
        coder.encode_generic_region_inner(
            packed_data,
            image.width,
            image.height,
            template,
            &at, // Use the provided AT pixels
        )?;

        // Finalize the arithmetic coder state with the JBIG2 terminator
        coder.flush_with_termination(true, canonical);
        // Get the result
        let result = coder.registers.data;

        debug_assert!(
            !result.is_empty(),
            "empty arithmetic stream – context generation broken"
        );

        log::debug!(
            "generic-region payload {} bytes (template={})",
            result.len(),
            template
        );

        Ok(result)
    }

    /// Encode a generic region using the specified template, conforming to
    /// ITU-T T.88 §7.4 and Annex E.
    pub fn encode_generic_region_inner(
        &mut self,
        packed: &[u32],
        width: usize,
        height: usize,
        template: u8,
        _at: &[(i8, i8); 4],
    ) -> Result<()> {
        // Only template 0 is implemented: the context builders below are
        // hard-coded to its 16-pixel layout, so any other value yields a stream
        // whose contexts do not match the template its region header declares.
        // This check used to live inside the `debug_assertions` block, which
        // left the shipping profile as the one profile that could not report
        // the mismatch -- and left `template` unused there besides.
        // Every caller currently passes 0 (the `Jbig2Config` defaults and the
        // symbol dictionary all hard-code it), so this stays silent in practice.
        // The matching "using template 0" line is left in the block below rather
        // than hoisted with the rest: it fires once per symbol, and it says
        // nothing a release log needs to hear.
        match template {
            0 => {}
            1..=3 => log::warn!("Template {} is not fully tested", template),
            _ => log::error!("Invalid template {}", template),
        }

        #[cfg(debug_assertions)]
        {
            log::debug!(
                "encode_generic_region_inner: {}x{}, template={}",
                width,
                height,
                template
            );
            log::debug!(
                "  packed data: {} words ({} bytes), expected {} words for {}x{} bitmap",
                packed.len(),
                packed.len() * 4,
                ((width + 31) / 32) * height,
                width,
                height
            );

            if let Some(idx) = packed.iter().position(|w| *w != 0) {
                let first_bit = (idx * 32) + (31 - packed[idx].leading_zeros() as usize);
                let row = first_bit / width;
                let col = first_bit % width;
                log::debug!(
                    "first black pixel according to Rust packer: ({}, {})",
                    col,
                    row
                );
            } else {
                log::debug!("packed data all zero - no black pixels");
            }

            if template == 0 {
                log::debug!("Using Template 0 (Standard 16-pixel context)");
            }
        }

        let words_per_row = (width + 31) / 32;

        #[cfg(debug_assertions)]
        let mut context_distribution: HashMap<usize, usize> = HashMap::new();
        #[cfg(debug_assertions)]
        let mut last_reported_progress = -1;

        let contexts: &mut [u32; JBIG2_MAX_CTX] = self.context.as_mut_slice().try_into().unwrap();
        let registers = &mut self.registers;
        for y in 0..height {
            #[cfg(debug_assertions)]
            {
                let progress = (y as i32 * 10) / height as i32;
                if progress > last_reported_progress {
                    last_reported_progress = progress;
                    let unique_contexts = context_distribution.len();
                    let total_samples: usize = context_distribution.values().sum();
                    let avg_occurrences = if unique_contexts > 0 {
                        total_samples / unique_contexts
                    } else {
                        0
                    };

                    log::debug!(
                        "Progress: {}% - Line {}/{} - Contexts: {} unique (avg {:.1} uses/context)",
                        progress * 10,
                        y,
                        height,
                        unique_contexts,
                        avg_occurrences as f32
                    );
                    context_distribution.clear();
                }
            }

            let row = &packed[y * words_per_row..(y + 1) * words_per_row];
            let prev1 = if y > 0 {
                Some(&packed[(y - 1) * words_per_row..y * words_per_row])
            } else {
                None
            };
            let prev2 = if y > 1 {
                Some(&packed[(y - 2) * words_per_row..(y - 1) * words_per_row])
            } else {
                None
            };

            // Load future context bits once per word. The two 64-bit windows
            // supply x+3/x+4 without a per-pixel index, shift count or bounds
            // check. Tail masking keeps public packed input's padding invisible.
            let tail_mask = match width & 31 {
                0 => u32::MAX,
                bits => u32::MAX << (32 - bits),
            };
            let word = |line: Option<&[u32]>, index: usize| -> u32 {
                let value = line.and_then(|line| line.get(index)).copied().unwrap_or(0);
                if index + 1 == words_per_row {
                    value & tail_mask
                } else {
                    value
                }
            };
            let mut context =
                (((word(prev2, 0) >> 29) as u16) << 11) | (((word(prev1, 0) >> 28) as u16) << 4);
            for block in 0..words_per_row {
                let mut pixels = row[block];
                let mut above2 =
                    (((word(prev2, block) as u64) << 32) | word(prev2, block + 1) as u64) << 3;
                let mut above1 =
                    (((word(prev1, block) as u64) << 32) | word(prev1, block + 1) as u64) << 4;
                let count = (width - block * 32).min(32);
                // All pixels and incoming context bits are zero for this word.
                // The context remains zero throughout, so batch the common MPS
                // updates until the next required probability-state transition.
                if context == 0 && pixels == 0 && (above1 >> 32) == 0 && (above2 >> 32) == 0 {
                    registers.encode_zero_run(&mut contexts[0], count);
                    #[cfg(debug_assertions)]
                    {
                        *context_distribution.entry(0).or_insert(0) += count;
                    }
                    continue;
                }
                for _ in 0..count {
                    let pixel = (pixels >> 31) as u16;
                    let state = &mut contexts[context as usize];
                    registers.encode_bitmap_bit(state, pixel != 0);
                    #[cfg(debug_assertions)]
                    {
                        *context_distribution.entry(context as usize).or_insert(0) += 1;
                    }
                    context = ((context << 1) & 0xf7ee)
                        | ((above2 >> 52) as u16 & 0x0800)
                        | ((above1 >> 59) as u16 & 0x0010)
                        | pixel;
                    above2 <<= 1;
                    above1 <<= 1;
                    pixels <<= 1;
                }
            }

            #[cfg(debug_assertions)]
            {
                log::trace!("Finished encoding line {}/{}", y + 1, height);
            }
        }

        Ok(())
    }

    #[cfg(feature = "line_verify")]
    fn verify_line_contexts(
        width: usize,
        y: usize,
        static_buf: &[bool],  // pixels from all previous lines
        current_buf: &[bool], // pixels encoded so far on this line
        enc_cxs: &[usize],    // encoder contexts for this line
        at: &[(i8, i8); 4],
    ) -> Result<(), String> {
        const STATIC: [(i8, i8); 6] = [(-1, -2), (0, -2), (1, -2), (-2, -1), (-1, -1), (0, -1)];

        let mut idx = 0;
        for x in 0..width as i32 {
            // --- rebuild cx exactly as decoder will ---
            let mut cx = 0usize;
            let mut bp = 0;

            for &(dx, dy) in &STATIC {
                let xx = x + dx as i32;
                let yy = y as i32 + dy as i32;
                let bit = if yy < 0 || xx < 0 || xx >= width as i32 || yy >= y as i32 {
                    false
                } else {
                    static_buf[yy as usize * width + xx as usize]
                };
                cx |= (bit as usize) << bp;
                bp += 1;
            }

            // four AT pixels
            for (dx, dy) in at {
                let xx = x + *dx as i32;
                let yy = y as i32 + *dy as i32;
                let bit = if yy == y as i32 {
                    if xx < 0 || xx >= x {
                        false
                    } else {
                        current_buf[xx as usize]
                    }
                } else if yy == y as i32 - 1 {
                    if xx < 0 || xx >= width as i32 || yy < 0 {
                        false
                    } else {
                        static_buf[yy as usize * width + xx as usize]
                    }
                } else if yy < 0 || xx < 0 || xx >= width as i32 || yy >= y as i32 {
                    false
                } else {
                    static_buf[yy as usize * width + xx as usize]
                };
                cx |= (bit as usize) << bp;
                bp += 1;
            }

            // --- compare with encoder ---
            if cx != enc_cxs[idx] {
                return Err(format!(
                    "Context mismatch @ ({},{})  enc={:#05x}  dec={:#05x}",
                    x, y, enc_cxs[idx], cx
                ));
            }
            idx += 1;
        }
        Ok(())
    }

    /// Encodes a refinement region using the JBIG2 generic refinement procedure (§6.3).
    ///
    /// The target bitmap is encoded pixel-by-pixel using a 13-bit context formed
    /// from 3 already-coded target pixels + 9 reference pixels (3×3 neighbourhood)
    /// + 1 adaptive template (AT) pixel from the reference. This matches the
    /// GRTEMPLATE=0 context layout from T.88 Table 12, verified against jbig2dec.
    ///
    /// # Arguments
    /// * `target` - The bitmap to encode (the actual instance glyph)
    /// * `reference` - The prototype/dictionary symbol used as predictor
    /// * `grdx` - Horizontal offset: reference pos = target pos − (grdx, grdy)
    /// * `grdy` - Vertical offset
    /// * `template` - GRTEMPLATE (0 or 1). Only 0 is implemented.
    /// * `grat` - Adaptive template pixel offsets; for GRTEMPLATE=0 supply one (x,y) pair
    pub fn encode_refinement_region(
        &mut self,
        target: &crate::jbig2sym::BitImage,
        reference: &crate::jbig2sym::BitImage,
        grdx: i32,
        grdy: i32,
        template: u8,
        grat: &[(i8, i8)],
    ) -> Result<()> {
        // GRTEMPLATE=0: 13-bit context → 8192 possible contexts
        // Context bit layout (matching jbig2dec for interoperability):
        //   bit  0: ref(rx-1, ry-1)    upper-left in reference
        //   bit  1: ref(rx,   ry-1)    upper-center
        //   bit  2: ref(rx+1, ry-1)    upper-right
        //   bit  3: ref(rx-1, ry)      middle-left
        //   bit  4: ref(rx,   ry)      center (the prediction pixel)
        //   bit  5: ref(rx+1, ry)      middle-right
        //   bit  6: target(x-1, y)     left (already coded)
        //   bit  7: ref(rx-1, ry+1)    lower-left
        //   bit  8: ref(rx,   ry+1)    lower-center
        //   bit  9: ref(rx+1, ry+1)    lower-right
        //   bit 10: target(x+1, y-1)   above-right (already coded)
        //   bit 11: target(x, y-1)     above (already coded)
        //   bit 12: target AT pixel    adaptive template (GRAT1, nominal (-1,-1))
        //
        // T.88 §6.3.5.3 Figure 12: GRTEMPLATE=0 has TWO adaptive pixels — GRAT1 in
        // the already-coded (target/GRREG) bitmap and GRAT2 in the reference. With
        // nominal AT = (-1,-1), GRAT2 coincides with the fixed reference pixel at
        // (rx-1, ry-1) (bit 0), so the distinct 13th pixel is the *target* GRAT1 at
        // (x-1, y-1). Reading bit 12 from the reference instead duplicates a
        // reference pixel and omits the target AT pixel, which mismatches jbig2dec's
        // context (blank decode + arithmetic desync).

        let grat_x = grat.first().map_or(-1i32, |g| i32::from(g.0));
        let grat_y = grat.first().map_or(-1i32, |g| i32::from(g.1));
        // With the nominal AT the 13th pixel is target(x-1, y-1), which is the
        // bit 11 value of the previous column; any other AT is read per pixel.
        let nominal_at = template == 0 && grat_x == -1 && grat_y == -1;
        let (target_width, target_height) = (target.width, target.height);
        let reference_width = reference.width;
        let target_row = |y: i32| -> Option<&[u32]> {
            ((y as u32) < target_height as u32).then(|| target.row_words(y as usize))
        };
        let reference_row = |y: i32| -> Option<&[u32]> {
            ((y as u32) < reference.height as u32).then(|| reference.row_words(y as usize))
        };
        // Zero outside the bitmap, exactly like `get_pixel_safely`.
        #[inline(always)]
        fn bit(row: Option<&[u32]>, width: usize, x: i32) -> usize {
            match row {
                Some(words) if (x as u32) < width as u32 => {
                    ((words[(x >> 5) as usize] >> (31 - (x & 31))) & 1) as usize
                }
                _ => 0,
            }
        }

        for y in 0..target_height as i32 {
            let ry = y.wrapping_sub(grdy);
            let above = reference_row(ry.wrapping_sub(1));
            let middle = reference_row(ry);
            let below = reference_row(ry.wrapping_add(1));
            let target_above = target_row(y.wrapping_sub(1));
            let target_at = target_row(y.wrapping_add(grat_y));
            let row = target_row(y);

            // Three-pixel reference windows: bit 0 is rx-1, bit 2 is rx+1.
            // They slide right by one pixel per column, like the generic
            // kernel's context, so each column reads only the incoming bits.
            let rx = 0i32.wrapping_sub(grdx);
            let window = |line: Option<&[u32]>| -> usize {
                bit(line, reference_width, rx.wrapping_sub(1))
                    | (bit(line, reference_width, rx) << 1)
                    | (bit(line, reference_width, rx.wrapping_add(1)) << 2)
            };
            let mut window_above = window(above);
            let mut window_middle = window(middle);
            let mut window_below = window(below);
            // Target row above: bit 1 is target(x, y-1), bit 0 is target(x+1, y-1).
            let mut coded_above =
                (bit(target_above, target_width, 0) << 1) | bit(target_above, target_width, 1);
            let mut left = 0usize; // target(x-1, y), already coded
            let mut at_previous = 0usize; // target(x-1, y-1) for the nominal AT

            for x in 0..target_width as i32 {
                let at = if nominal_at {
                    at_previous
                } else if template == 0 {
                    bit(target_at, target_width, x.wrapping_add(grat_x))
                } else {
                    0
                };
                let cx = window_above
                    | (window_middle << 3)
                    | (left << 6)
                    | (window_below << 7)
                    | (coded_above << 10)
                    | (at << 12);
                let pixel = bit(row, target_width, x) != 0;

                // Encode using the dedicated refinement context array
                let next_state =
                    self.encode_bit_with_state_idx(self.refinement_contexts[cx], pixel);
                self.refinement_contexts[cx] = next_state;

                let rx = x.wrapping_sub(grdx).wrapping_add(2);
                at_previous = coded_above >> 1;
                left = pixel as usize;
                window_above = (window_above >> 1) | (bit(above, reference_width, rx) << 2);
                window_middle = (window_middle >> 1) | (bit(middle, reference_width, rx) << 2);
                window_below = (window_below >> 1) | (bit(below, reference_width, rx) << 2);
                coded_above = ((coded_above << 1) & 0b11)
                    | bit(target_above, target_width, x.wrapping_add(2));
            }
        }

        Ok(())
    }

    /// The per-pixel context formation the windowed coder replaced. Test
    /// oracle only: thirteen bounded reads per pixel.
    #[cfg(test)]
    pub(crate) fn encode_refinement_region_reference(
        &mut self,
        target: &crate::jbig2sym::BitImage,
        reference: &crate::jbig2sym::BitImage,
        grdx: i32,
        grdy: i32,
        template: u8,
        grat: &[(i8, i8)],
    ) {
        let grat_x = grat.first().map_or(-1i8, |g| g.0);
        let grat_y = grat.first().map_or(-1i8, |g| g.1);
        for y in 0..target.height as i32 {
            for x in 0..target.width as i32 {
                let rx = x.wrapping_sub(grdx);
                let ry = y.wrapping_sub(grdy);
                let mut cx: usize = 0;
                cx |= reference.get_pixel_safely(rx.wrapping_sub(1), ry.wrapping_sub(1)) as usize;
                cx |= (reference.get_pixel_safely(rx, ry.wrapping_sub(1)) as usize) << 1;
                cx |= (reference.get_pixel_safely(rx.wrapping_add(1), ry.wrapping_sub(1)) as usize)
                    << 2;
                cx |= (reference.get_pixel_safely(rx.wrapping_sub(1), ry) as usize) << 3;
                cx |= (reference.get_pixel_safely(rx, ry) as usize) << 4;
                cx |= (reference.get_pixel_safely(rx.wrapping_add(1), ry) as usize) << 5;
                cx |= (target.get_pixel_safely(x.wrapping_sub(1), y) as usize) << 6;
                cx |= (reference.get_pixel_safely(rx.wrapping_sub(1), ry.wrapping_add(1)) as usize)
                    << 7;
                cx |= (reference.get_pixel_safely(rx, ry.wrapping_add(1)) as usize) << 8;
                cx |= (reference.get_pixel_safely(rx.wrapping_add(1), ry.wrapping_add(1)) as usize)
                    << 9;
                cx |= (target.get_pixel_safely(x + 1, y - 1) as usize) << 10;
                cx |= (target.get_pixel_safely(x, y - 1) as usize) << 11;
                if template == 0 {
                    cx |= (target.get_pixel_safely(
                        x.wrapping_add(grat_x as i32),
                        y.wrapping_add(grat_y as i32),
                    ) as usize)
                        << 12;
                }
                let pixel = target.get_pixel_safely(x, y) != 0;
                let next_state =
                    self.encode_bit_with_state_idx(self.refinement_contexts[cx], pixel);
                self.refinement_contexts[cx] = next_state;
            }
        }
    }

    /// Resets only the refinement contexts, keeping other state intact.
    /// Call this between successive refinement regions within the same text region
    /// segment so that each symbol instance starts from a clean prediction state.
    pub fn reset_refinement_contexts(&mut self) {
        self.refinement_contexts.fill(0);
    }

    /// Returns the output buffer as a Vec<u8>.
    pub fn into_vec(mut self) -> Vec<u8> {
        self.flush(false); // Flush any pending data, do not add JBIG2 marker
        std::mem::take(&mut self.registers.data)
    }
}
// End of Jbig2ArithCoder implementation

impl ArithRegisters {
    fn renorm(&mut self) {
        loop {
            // Shift A and C left by 1 bit
            self.a = (self.a << 1) & 0xffff;
            self.c <<= 1;

            // Decrement the countdown register
            self.ct -= 1;

            // If CT reaches zero, perform byte out
            if self.ct == 0 {
                self.byte_out();
            }

            // Continue until A's high bit is set (A >= 0x8000)
            if (self.a & 0x8000) != 0 {
                break;
            }
        }
    }

    #[inline]
    fn byte_out(&mut self) {
        if self.b == 0xFF {
            if self.bp >= 0 {
                self.data.push(self.b);
            }
            self.b = (self.c >> 20) as u8;
            self.bp += 1;
            self.c &= 0x0F_FFFF;
            self.ct = 7;
            return;
        }

        if self.c < 0x800_0000 {
            if self.bp >= 0 {
                self.data.push(self.b);
            }
            self.b = (self.c >> 19) as u8;
            self.bp += 1;
            self.c &= 0x07_FFFF;
            self.ct = 8;
            return;
        }

        self.b = self.b.wrapping_add(1);
        if self.b == 0xFF {
            self.c &= 0x7_FFFF_FF;
            if self.bp >= 0 {
                self.data.push(self.b);
            }
            self.b = (self.c >> 20) as u8;
            self.bp += 1;
            self.c &= 0x0F_FFFF;
            self.ct = 7;
        } else {
            if self.bp >= 0 {
                self.data.push(self.b);
            }
            self.b = (self.c >> 19) as u8;
            self.bp += 1;
            self.c &= 0x07_FFFF;
            self.ct = 8;
        }
    }

    pub(crate) fn flush_with_termination(&mut self, with_marker: bool, canonical: bool) {
        let temp_c = self.c + self.a as u32;
        self.c |= 0x0000_FFFF;
        if self.c >= temp_c {
            self.c -= 0x8000;
        }
        self.c <<= self.ct as u32;
        self.byte_out();
        self.c <<= self.ct as u32;
        self.byte_out();

        // A final FF can serve as the marker prefix. Emitting it separately
        // before FF AC produces FF FF AC, leaving a redundant byte for readers.
        // The compatibility branch preserves the existing low-level API bytes.
        if self.bp >= 0 && (self.b != 0xFF || (with_marker && !canonical)) {
            self.data.push(self.b);
        }
        if with_marker {
            self.data.push(0xFF);
            self.data.push(0xAC);
        }
    }

    #[inline(always)]
    fn encode_bit_with_state_idx(&mut self, state_idx: usize, d: bool) -> usize {
        let qe = crate::shared::mq_table::MQ_STATES[state_idx].qe as u32;
        self.a = self.a.wrapping_sub(qe) & 0xffff;
        if d == (state_idx >= 47) && self.a >= 0x8000 {
            self.c = self.c.wrapping_add(qe);
            return state_idx;
        }
        self.encode_bit_renormalized(state_idx, d)
    }

    #[inline(always)]
    fn encode_bitmap_bit(&mut self, cached: &mut u32, d: bool) {
        let state_idx = (*cached >> 16) as usize;
        let qe = *cached & 0xffff;
        self.a = self.a.wrapping_sub(qe) & 0xffff;
        if d == (state_idx >= 47) && self.a >= 0x8000 {
            self.c = self.c.wrapping_add(qe);
            return;
        }
        let next = self.encode_bit_renormalized(state_idx, d);
        *cached = ((next as u32) << 16) | crate::shared::mq_table::MQ_STATES[next].qe as u32;
    }

    /// Encode a known run in context zero without changing its per-bit model.
    #[inline]
    fn encode_zero_run(&mut self, state_idx: &mut u32, mut count: usize) {
        // Only the caller's single packed word is accepted: count * Qe and
        // 0x8000 + count * Qe fit u32 even for the largest probability estimate.
        debug_assert!(count <= 32);
        debug_assert!(self.a >= 0x8000);
        while count != 0 {
            let qe = *state_idx & 0xffff;
            if (*state_idx >> 16) < 47 {
                let decrement = qe * count as u32;
                // Every skipped MPS must leave A normalized. No context state
                // changes on this path; C advances by the same total interval.
                if self.a >= 0x8000 + decrement {
                    self.a -= decrement;
                    self.c = self.c.wrapping_add(decrement);
                    return;
                }
                let normalized = ((self.a - 0x8000) / qe) as usize;
                self.a -= qe * normalized as u32;
                self.c = self.c.wrapping_add(qe * normalized as u32);
                count -= normalized;
            }
            self.encode_bitmap_bit(state_idx, false);
            count -= 1;
        }
    }

    // Keep state transitions and byte output out of the common MPS pixel path.
    #[inline(never)]
    fn encode_bit_renormalized(&mut self, state_idx: usize, d: bool) -> usize {
        let state = crate::shared::mq_table::MQ_STATES[state_idx];
        let qe = state.qe as u32;
        let next_state = if d != (state_idx >= 47) {
            if self.a < qe {
                self.c = self.c.wrapping_add(qe as u32);
            } else {
                self.a = qe;
            }
            state.next_lps as usize
        } else {
            if self.a < qe {
                self.a = qe;
            } else {
                self.c = self.c.wrapping_add(qe as u32);
            }
            state.next_mps as usize
        };
        loop {
            self.a = (self.a << 1) & 0xffff;
            self.c <<= 1;
            self.ct -= 1;
            if self.ct == 0 {
                self.byte_out();
            }
            if self.a >= 0x8000 {
                break;
            }
        }
        next_state
    }
}

#[cfg(test)]
mod refinement_window_tests {
    use super::Jbig2ArithCoder;
    use crate::jbig2sym::BitImage;

    fn random_image(seed: &mut u64, width: u32, height: u32, density: u64) -> BitImage {
        let mut image = BitImage::new(width, height).unwrap();
        for y in 0..height as usize {
            for x in 0..width as usize {
                *seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                if (*seed >> 33) % 8 < density {
                    image.set_usize(x, y, true);
                }
            }
        }
        image
    }

    #[test]
    fn windowed_refinement_contexts_match_per_pixel_reference() {
        let mut seed = 0x5eed_c0de_u64;
        let ats: [&[(i8, i8)]; 5] = [&[(-1, -1)], &[(2, -2)], &[(-3, 1)], &[(0, -1)], &[]];
        for case in 0..400u32 {
            seed = seed.wrapping_add(u64::from(case));
            let width = 1 + (seed >> 20) as u32 % 70;
            let height = 1 + (seed >> 27) as u32 % 24;
            let ref_width = 1 + (seed >> 34) as u32 % 70;
            let ref_height = 1 + (seed >> 41) as u32 % 24;
            let target = random_image(&mut seed, width, height, 1 + case as u64 % 7);
            let reference =
                random_image(&mut seed, ref_width, ref_height, 1 + (case / 7) as u64 % 7);
            let (grdx, grdy) = match case % 5 {
                0 => (0, 0),
                1 => ((seed >> 5) as i32 % 9 - 4, (seed >> 9) as i32 % 9 - 4),
                2 => (-(ref_width as i32) - 3, ref_height as i32 + 2),
                3 => (i32::MAX - 1, i32::MIN + 1),
                _ => (
                    (seed >> 13) as i32 % 150 - 75,
                    (seed >> 21) as i32 % 60 - 30,
                ),
            };
            let template = (case % 3 == 2) as u8;
            let grat = ats[case as usize % ats.len()];
            let mut expected = Jbig2ArithCoder::new();
            expected.encode_refinement_region_reference(
                &target, &reference, grdx, grdy, template, grat,
            );
            expected.flush(true);
            let mut actual = Jbig2ArithCoder::new();
            actual
                .encode_refinement_region(&target, &reference, grdx, grdy, template, grat)
                .unwrap();
            actual.flush(true);
            assert_eq!(
                actual.as_bytes(),
                expected.as_bytes(),
                "case {case}: {width}x{height} vs {ref_width}x{ref_height}, grd=({grdx},{grdy}), template {template}, grat {grat:?}"
            );
        }
    }
}

#[cfg(test)]
mod refinement_window_timing {
    use super::Jbig2ArithCoder;
    use crate::jbig2sym::BitImage;

    fn glyph(seed: &mut u64, width: u32, height: u32) -> BitImage {
        let mut image = BitImage::new(width, height).unwrap();
        for y in 0..height as usize {
            for x in 0..width as usize {
                *seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                if (*seed >> 33).is_multiple_of(3) {
                    image.set_usize(x, y, true);
                }
            }
        }
        image
    }

    /// `cargo test --release --lib refinement_window_timing -- --ignored --nocapture`
    #[test]
    #[ignore = "timing comparison of the windowed and per-pixel refinement coders"]
    fn windowed_versus_per_pixel_refinement_time() {
        let mut seed = 7u64;
        // Realistic pairs: the target is the reference with about 2% of its
        // pixels flipped, which is what a refined instance looks like.
        let pairs: Vec<(BitImage, BitImage)> = (0..2000)
            .map(|_| {
                let reference = glyph(&mut seed, 31, 37);
                let mut target = reference.clone();
                for y in 0..37 {
                    for x in 0..31 {
                        seed = seed
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        if (seed >> 33).is_multiple_of(50) {
                            target.set_usize(x, y, !target.get_usize(x, y));
                        }
                    }
                }
                (target, reference)
            })
            .collect();
        let pixels = pairs.len() * 31 * 37;
        for round in 0..10 {
            let start = std::time::Instant::now();
            let mut coder = Jbig2ArithCoder::new();
            for (target, reference) in &pairs {
                coder
                    .encode_refinement_region(target, reference, 0, 0, 0, &[(-1, -1)])
                    .unwrap();
            }
            let windowed = start.elapsed();
            let bytes = coder.as_bytes().len();
            let start = std::time::Instant::now();
            let mut coder = Jbig2ArithCoder::new();
            for (target, reference) in &pairs {
                coder.encode_refinement_region_reference(target, reference, 0, 0, 0, &[(-1, -1)]);
            }
            let reference = start.elapsed();
            assert_eq!(bytes, coder.as_bytes().len());
            eprintln!(
                "round {round}: windowed {:.2} ns/px, per-pixel {:.2} ns/px, ratio {:.3}",
                windowed.as_nanos() as f64 / pixels as f64,
                reference.as_nanos() as f64 / pixels as f64,
                windowed.as_secs_f64() / reference.as_secs_f64()
            );
        }
    }
}
