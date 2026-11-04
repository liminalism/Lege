//! Arithmetic generic-region decoding, template 0 (jbig2decplan.md §14, T.88
//! §6.2).
//!
//! This mirrors the encoder's packed-word context organisation
//! (`encode::arith::Jbig2ArithCoder::encode_generic_region_inner`) so a
//! round-trip is pixel-identical. One windowed decoder serves all four
//! arithmetic templates (0–3) and TPGDON typical prediction: the context is
//! a single integer shifted one pixel at a time, fed from 64-bit row
//! look-ahead registers loaded once per 32-pixel block (see `Geometry`).
//! Nominal AT pixels are part of the row windows, and uniform background runs
//! fold their arithmetic steps together; any other AT position takes the
//! general instantiation, which reads the AT pixels from their own windows.
//! The previous per-pixel decoders are kept under `#[cfg(test)]` as the
//! bit-exactness reference. MMR generic regions are handled by
//! [`crate::decode::mmr`].

use crate::decode::arith::ArithmeticDecoder;
use crate::decode::error::{DecodeError, ParseError};
use crate::shared::bitmap::MonoBitmap;
use crate::shared::limits::DecodeLimits;
use crate::shared::mq_table::MqContext;
use crate::shared::reader::Reader;

/// Nominal template-0 adaptive-template offsets (T.88 §6.2.5.3): AT1 (3,-1),
/// AT2 (-3,-1), AT3 (2,-2), AT4 (-2,-2). Also this crate's encoder default.
pub const NOMINAL_AT0: [(i8, i8); 4] = [(3, -1), (-3, -1), (2, -2), (-2, -2)];

/// Reusable row buffers for the generic decoder: a zero row for the top-boundary
/// rows and the current-row accumulator. Pooled in the [`DecoderContext`] and
/// reused across every region and every dictionary symbol, so the decode of a
/// symbol-heavy page allocates these once instead of twice per symbol.
#[derive(Default)]
pub struct GenericScratch {
    zero: Vec<u32>,
    cur: Vec<u32>,
}

impl GenericScratch {
    /// Return a zeroed zero-row and a zeroed current-row buffer of `stride`
    /// words each, reusing the existing allocations (disjoint fields, so both
    /// may be borrowed at once).
    #[inline]
    fn rows(&mut self, stride: usize) -> (&[u32], &mut [u32]) {
        self.zero.clear();
        self.zero.resize(stride, 0);
        self.cur.clear();
        self.cur.resize(stride, 0);
        (&self.zero, &mut self.cur)
    }
}

/// Nominal AT1 offset for templates 1–3 (T.88 §6.2.5.3 Figures 5–7): template 1
/// uses (3,-1); templates 2 and 3 use (2,-1).
pub const NOMINAL_AT1: [(i8, i8); 3] = [(3, -1), (2, -1), (2, -1)];

// SLTP pseudo-pixel contexts for TPGDON (T.88 §6.2.5.7, Figure 8), the literal
// spec/jbig2dec values. `decode_template0_general`'s bit layout is the pdf.js/
// jbig2dec sorted-template order, so template 0's SLTP context is the spec's
// 0x9B25 (not a re-expressed value). Templates 1–3 likewise use spec numbering.
const SLTP_CTX_T0: usize = 0x9B25;
const SLTP_CTX_T1: usize = 0x0795;
const SLTP_CTX_T2: usize = 0x00E5;
const SLTP_CTX_T3: usize = 0x0195;

/// A parsed generic-region segment: geometry, coding flags, and the coded data.
#[derive(Clone, Debug)]
pub struct GenericRegion<'a> {
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    /// External combination operator (region flags low 3 bits).
    pub comb_operator: u8,
    pub mmr: bool,
    pub template: u8,
    pub tpgdon: bool,
    pub at: [(i8, i8); 4],
    /// Arithmetic (or MMR) coded data.
    pub data: &'a [u8],
}

/// Parse the generic-region segment payload (T.88 §7.4.6): region segment info
/// (§7.4.1) + generic flags + AT pixels + coded data.
pub fn parse_generic_region<'a>(payload: &'a [u8]) -> Result<GenericRegion<'a>, ParseError> {
    let mut r = Reader::new(payload);
    // §7.4.1 region segment information field.
    let width = r.read_u32_be()?;
    let height = r.read_u32_be()?;
    let x = r.read_u32_be()?;
    let y = r.read_u32_be()?;
    let region_flags = r.read_u8()?;
    let comb_operator = region_flags & 0x07;

    // §7.4.6.2 generic region segment flags.
    let flags = r.read_u8()?;
    let mmr = flags & 0x01 != 0;
    let template = (flags >> 1) & 0x03;
    let tpgdon = flags & 0x08 != 0;

    // AT pixels (T.88 §7.4.6.3): template 0 has 4, templates 1–3 have 1 each.
    // (Only present for arithmetic coding, i.e. when MMR is off.)
    let mut at = [(0i8, 0i8); 4];
    if !mmr {
        let at_count = if template == 0 { 4 } else { 1 };
        for slot in at.iter_mut().take(at_count) {
            let ax = r.read_i8()?;
            let ay = r.read_i8()?;
            *slot = (ax, ay);
        }
    }

    let data = &payload[r.position()..];
    Ok(GenericRegion {
        width,
        height,
        x,
        y,
        comb_operator,
        mmr,
        template,
        tpgdon,
        at,
        data,
    })
}

/// Decode a parsed generic region into a fresh region-local [`MonoBitmap`].
///
/// `contexts` must be at least `1 << 16` entries; it is not reset here (the
/// caller resets before each independent region — a fresh segment starts with
/// all-zero states).
pub fn decode_generic_region(
    region: &GenericRegion<'_>,
    limits: &DecodeLimits,
    contexts: &mut [MqContext],
    scratch: &mut GenericScratch,
) -> Result<MonoBitmap, DecodeError> {
    if region.mmr {
        // MMR (Group 4) generic region: no arithmetic coder, no AT pixels; the
        // payload is byte-aligned T.6 data for the full region (T.88 §6.2.6).
        return crate::decode::mmr::decode_mmr_bitmap(
            region.data,
            region.width,
            region.height,
            limits,
        );
    }
    if (contexts.len() as u64) < (1u64 << 16) {
        return Err(DecodeError::Overflow {
            operation: "generic region context array too small",
        });
    }

    let mut bitmap = MonoBitmap::new(region.width, region.height, false, limits)?;
    if region.width == 0 || region.height == 0 {
        return Ok(bitmap);
    }

    let mut decoder = ArithmeticDecoder::new(region.data);
    decode_into(
        &mut bitmap,
        &mut decoder,
        region.template,
        region.at,
        region.tpgdon,
        contexts,
        scratch,
    );
    Ok(bitmap)
}

/// Decode a generic region **into** a caller-provided bitmap, reusing its
/// backing allocation (the zero-alloc `decode_embedded_into` path). `dest` must
/// already be sized to `region.width` × `region.height` and all-white, as
/// produced by the context's bitmap pool. Behaviour otherwise matches
/// [`decode_generic_region`].
#[allow(clippy::too_many_arguments)]
pub fn decode_generic_region_into(
    region: &GenericRegion<'_>,
    limits: &DecodeLimits,
    contexts: &mut [MqContext],
    scratch: &mut GenericScratch,
    dest: &mut MonoBitmap,
) -> Result<(), DecodeError> {
    debug_assert_eq!(dest.width(), region.width);
    debug_assert_eq!(dest.height(), region.height);
    if region.mmr {
        // MMR has no reusable-buffer decoder; decode into a temporary and copy
        // into `dest` (reusing `dest`'s allocation). MMR generic regions are not
        // the zero-alloc target.
        let bm = crate::decode::mmr::decode_mmr_bitmap(
            region.data,
            region.width,
            region.height,
            limits,
        )?;
        dest.assign_from(&bm);
        return Ok(());
    }
    if (contexts.len() as u64) < (1u64 << 16) {
        return Err(DecodeError::Overflow {
            operation: "generic region context array too small",
        });
    }
    if region.width == 0 || region.height == 0 {
        return Ok(());
    }
    let mut decoder = ArithmeticDecoder::new(region.data);
    decode_into(
        dest,
        &mut decoder,
        region.template,
        region.at,
        region.tpgdon,
        contexts,
        scratch,
    );
    Ok(())
}

/// Decode a single template-0 generic bitmap from an *existing* arithmetic
/// decoder, without resetting `contexts` (jbig2decplan.md §16).
///
/// Symbol-dictionary bitmaps share one arithmetic stream and one generic-context
/// bank across every symbol in the dictionary, so this variant neither creates
/// its own decoder nor zeroes the contexts — the dictionary decoder owns both.
/// `contexts` must be at least `1 << 16` entries.
#[allow(clippy::too_many_arguments)]
pub fn decode_generic_bitmap(
    decoder: &mut ArithmeticDecoder<'_>,
    width: u32,
    height: u32,
    template: u8,
    at: [(i8, i8); 4],
    contexts: &mut [MqContext],
    limits: &DecodeLimits,
    scratch: &mut GenericScratch,
) -> Result<MonoBitmap, DecodeError> {
    if (contexts.len() as u64) < (1u64 << 16) {
        return Err(DecodeError::Overflow {
            operation: "generic bitmap context array too small",
        });
    }
    let mut bitmap = MonoBitmap::new(width, height, false, limits)?;
    if width == 0 || height == 0 {
        return Ok(bitmap);
    }
    // Symbol-dictionary and refinement generic bitmaps never use TPGDON.
    decode_into(&mut bitmap, decoder, template, at, false, contexts, scratch);
    Ok(bitmap)
}

/// Skip-aware generic bitmap decode (T.88 §6.2.5.7 USESKIP / halftone
/// HENABLESKIP §6.6.5.1): pixels where `skip` is set are forced to 0 and *not*
/// arithmetically decoded; their neighbours still contribute to later contexts
/// as 0. Per-pixel (no fast rolling path) — used only for the rare skip case.
/// TPGDON is not combined with skip here (the gray-scale planes never set it).
#[allow(clippy::too_many_arguments)]
pub fn decode_generic_bitmap_skip(
    decoder: &mut ArithmeticDecoder<'_>,
    width: u32,
    height: u32,
    template: u8,
    at: [(i8, i8); 4],
    contexts: &mut [MqContext],
    limits: &DecodeLimits,
    skip: &MonoBitmap,
    scratch: &mut GenericScratch,
) -> Result<MonoBitmap, DecodeError> {
    if (contexts.len() as u64) < (1u64 << 16) {
        return Err(DecodeError::Overflow {
            operation: "generic bitmap context array too small",
        });
    }
    let mut bitmap = MonoBitmap::new(width, height, false, limits)?;
    if width == 0 || height == 0 {
        return Ok(bitmap);
    }
    let stride = bitmap.stride_words() as usize;
    let (zero_row, cur) = scratch.rows(stride);
    for y in 0..height {
        for w in cur.iter_mut() {
            *w = 0;
        }
        let prev1: &[u32] = if y >= 1 { bitmap.row(y - 1) } else { zero_row };
        let prev2: &[u32] = if y >= 2 { bitmap.row(y - 2) } else { zero_row };
        for x in 0..width {
            if skip.get(x, y) {
                continue; // implicitly 0, no arithmetic bit consumed
            }
            let ctx = pixel_context(
                template, &bitmap, &*cur, prev1, prev2, width, y, x as i64, &at,
            );
            let bit = decoder.decode_bit(&mut contexts[ctx]);
            if bit {
                let xu = x as usize;
                cur[xu >> 5] |= 1u32 << (31 - (xu & 31));
            }
        }
        bitmap.row_mut(y).copy_from_slice(cur);
    }
    Ok(bitmap)
}

/// Per-pixel context for any template, matching the per-template decoders'
/// bit layouts. Used by the skip-aware path.
#[allow(clippy::too_many_arguments)]
#[inline]
fn pixel_context(
    template: u8,
    bitmap: &MonoBitmap,
    cur: &[u32],
    prev1: &[u32],
    prev2: &[u32],
    width: u32,
    y: u32,
    xi: i64,
    at: &[(i8, i8); 4],
) -> usize {
    let a1 = (at[0].0 as i64, at[0].1 as i64);
    // AT slots read the bitmap directly so they can reference any earlier row;
    // fixed pixels stay on the cached `prev1`/`prev2` rows.
    let a = |ax: i64, ay: i64| at_pixel(bitmap, cur, width, y, xi, ax, ay);
    match template {
        0 => {
            let a2 = (at[1].0 as i64, at[1].1 as i64);
            let a3 = (at[2].0 as i64, at[2].1 as i64);
            let a4 = (at[3].0 as i64, at[3].1 as i64);
            let p = |dx: i64, dy: i64| -> u32 {
                let xx = xi + dx;
                match dy {
                    0 => {
                        if xx < 0 || xx >= xi || xx >= width as i64 {
                            0
                        } else {
                            sample(cur, width, xx)
                        }
                    }
                    -1 => sample(prev1, width, xx),
                    _ => sample(prev2, width, xx),
                }
            };
            ((a(a4.0, a4.1) << 15)
                | (p(-1, -2) << 14)
                | (p(0, -2) << 13)
                | (p(1, -2) << 12)
                | (a(a3.0, a3.1) << 11)
                | (a(a2.0, a2.1) << 10)
                | (p(-2, -1) << 9)
                | (p(-1, -1) << 8)
                | (p(0, -1) << 7)
                | (p(1, -1) << 6)
                | (p(2, -1) << 5)
                | (a(a1.0, a1.1) << 4)
                | (p(-4, 0) << 3)
                | (p(-3, 0) << 2)
                | (p(-2, 0) << 1)
                | p(-1, 0)) as usize
        }
        1 => {
            let a = a(a1.0, a1.1);
            (sample(cur, width, xi - 1)
                | (sample(cur, width, xi - 2) << 1)
                | (sample(cur, width, xi - 3) << 2)
                | (a << 3)
                | (sample(prev1, width, xi + 2) << 4)
                | (sample(prev1, width, xi + 1) << 5)
                | (sample(prev1, width, xi) << 6)
                | (sample(prev1, width, xi - 1) << 7)
                | (sample(prev1, width, xi - 2) << 8)
                | (sample(prev2, width, xi + 2) << 9)
                | (sample(prev2, width, xi + 1) << 10)
                | (sample(prev2, width, xi) << 11)
                | (sample(prev2, width, xi - 1) << 12)) as usize
        }
        2 => {
            let a = a(a1.0, a1.1);
            (sample(cur, width, xi - 1)
                | (sample(cur, width, xi - 2) << 1)
                | (a << 2)
                | (sample(prev1, width, xi + 1) << 3)
                | (sample(prev1, width, xi) << 4)
                | (sample(prev1, width, xi - 1) << 5)
                | (sample(prev1, width, xi - 2) << 6)
                | (sample(prev2, width, xi + 1) << 7)
                | (sample(prev2, width, xi) << 8)
                | (sample(prev2, width, xi - 1) << 9)) as usize
        }
        _ => {
            let a = a(a1.0, a1.1);
            (sample(cur, width, xi - 1)
                | (sample(cur, width, xi - 2) << 1)
                | (sample(cur, width, xi - 3) << 2)
                | (sample(cur, width, xi - 4) << 3)
                | (a << 4)
                | (sample(prev1, width, xi + 1) << 5)
                | (sample(prev1, width, xi) << 6)
                | (sample(prev1, width, xi - 1) << 7)
                | (sample(prev1, width, xi - 2) << 8)
                | (sample(prev1, width, xi - 3) << 9)) as usize
        }
    }
}

/// Dispatch to the windowed decoder for `template`. `template` selects the
/// pixel neighbourhood (0–3); values outside that range take template 3's
/// behaviour (the flags field is only 2 bits wide, so `template <= 3` always
/// holds for parsed regions). Nominal AT pixels sit inside the sliding row
/// windows; any other AT position takes the general instantiation, which
/// samples the AT pixels from their own windows.
#[allow(clippy::too_many_arguments)]
fn decode_into(
    bitmap: &mut MonoBitmap,
    decoder: &mut ArithmeticDecoder<'_>,
    template: u8,
    at: [(i8, i8); 4],
    tpgdon: bool,
    contexts: &mut [MqContext],
    scratch: &mut GenericScratch,
) {
    match template {
        0 if at == NOMINAL_AT0 => {
            decode_windowed::<0, false>(bitmap, decoder, contexts, at, tpgdon, scratch)
        }
        0 => decode_windowed::<0, true>(bitmap, decoder, contexts, at, tpgdon, scratch),
        1 if at[0] == NOMINAL_AT1[0] => {
            decode_windowed::<1, false>(bitmap, decoder, contexts, at, tpgdon, scratch)
        }
        1 => decode_windowed::<1, true>(bitmap, decoder, contexts, at, tpgdon, scratch),
        2 if at[0] == NOMINAL_AT1[1] => {
            decode_windowed::<2, false>(bitmap, decoder, contexts, at, tpgdon, scratch)
        }
        2 => decode_windowed::<2, true>(bitmap, decoder, contexts, at, tpgdon, scratch),
        _ if at[0] == NOMINAL_AT1[2] => {
            decode_windowed::<3, false>(bitmap, decoder, contexts, at, tpgdon, scratch)
        }
        _ => decode_windowed::<3, true>(bitmap, decoder, contexts, at, tpgdon, scratch),
    }
}

#[inline(always)]
fn sample(row: &[u32], width: u32, x: i64) -> u32 {
    if x < 0 || x >= width as i64 {
        return 0;
    }
    let x = x as usize;
    // row length is stride = ceil(width/32); x < width so x>>5 is in range.
    (row[x >> 5] >> (31 - (x & 31) as u32)) & 1
}

/// Context bit geometry of one generic template with its nominal AT pixels.
///
/// Every template's context (in the T.88 / jbig2dec numbering this decoder
/// uses, see [`SLTP_CTX_T0`]) is three contiguous windows, most significant
/// bit = leftmost pixel: row `y-2` (`x-l2 ..= x+r2`) above row `y-1`
/// (`x-l1 ..= x+r1`) above the current row (`x-p1_lsb ..= x-1`). With the
/// nominal AT positions the AT pixels are window members:
///
/// | template | bits | `y-2` window  | `y-1` window | current | AT slots |
/// |---|---|---|---|---|---|
/// | 0 | 16 | 15..11 `x-2..x+2` | 10..4 `x-3..x+3` | 3..0 | 4, 10, 11, 15 |
/// | 1 | 13 | 12..9 `x-1..x+2`  | 8..3 `x-2..x+3`  | 2..0 | 3 |
/// | 2 | 10 | 9..7 `x-1..x+1`   | 6..2 `x-2..x+2`  | 1..0 | 2 |
/// | 3 | 10 | —                 | 9..4 `x-3..x+2`  | 3..0 | 4 |
///
/// so moving one pixel right is a shift of the whole context plus one
/// incoming bit per row window (`x+1+r` lands in the window's lowest bit) and
/// the decoded pixel in bit 0.
struct Geometry {
    /// Context width in bits.
    bits: u32,
    /// Whether the template reads row `y-2`.
    has_p2: bool,
    /// Lowest context bit of the `y-2` window, and its right reach `r2`.
    p2_lsb: u32,
    r2: u32,
    /// Lowest context bit of the `y-1` window, and its right reach `r1`.
    p1_lsb: u32,
    r1: u32,
    /// Context bit of each AT slot, in `at[]` order (unused slots zero).
    at_bits: [u32; 4],
    /// Number of AT pixels the template has.
    at_count: usize,
    /// The SLTP pseudo-pixel context for TPGDON.
    sltp: usize,
}

const fn geometry(template: u8) -> Geometry {
    match template {
        0 => Geometry {
            bits: 16,
            has_p2: true,
            p2_lsb: 11,
            r2: 2,
            p1_lsb: 4,
            r1: 3,
            at_bits: [4, 10, 11, 15],
            at_count: 4,
            sltp: SLTP_CTX_T0,
        },
        1 => Geometry {
            bits: 13,
            has_p2: true,
            p2_lsb: 9,
            r2: 2,
            p1_lsb: 3,
            r1: 3,
            at_bits: [3, 0, 0, 0],
            at_count: 1,
            sltp: SLTP_CTX_T1,
        },
        2 => Geometry {
            bits: 10,
            has_p2: true,
            p2_lsb: 7,
            r2: 1,
            p1_lsb: 2,
            r1: 2,
            at_bits: [2, 0, 0, 0],
            at_count: 1,
            sltp: SLTP_CTX_T2,
        },
        _ => Geometry {
            bits: 10,
            has_p2: false,
            p2_lsb: 0,
            r2: 0,
            p1_lsb: 4,
            r1: 2,
            at_bits: [4, 0, 0, 0],
            at_count: 1,
            sltp: SLTP_CTX_T3,
        },
    }
}

/// Where one AT pixel reads from for the row being decoded.
#[derive(Clone, Copy)]
enum AtSource {
    /// Always 0: above the top edge, below the current row (`Ay > 0`), or at
    /// or right of the current pixel on the current row.
    Zero,
    /// Row `y + Ay` (`Ay < 0`), read through a 32-pixel window per block.
    Row(u32),
    /// The current row, `-Ax` (1..=128) pixels to the left.
    Current(u32),
}

/// Word `index` of a packed row with the padding bits past `width` cleared,
/// or 0 past the row's end. Decoded rows already have zero padding; masking
/// keeps that an invariant of this function rather than of every caller.
#[inline(always)]
fn row_word(row: &[u32], index: usize, tail_mask: u32) -> u32 {
    match row.get(index) {
        Some(&w) if index + 1 == row.len() => w & tail_mask,
        Some(&w) => w,
        None => 0,
    }
}

/// 32 pixels of `row` starting at signed pixel position `start`, MSB first,
/// with pixels outside `0..width` reading 0.
#[inline(always)]
fn row_window(row: &[u32], start: i64, tail_mask: u32) -> u32 {
    let wi = start.div_euclid(32);
    let off = start.rem_euclid(32) as u32;
    let get = |i: i64| -> u32 {
        if i < 0 {
            0
        } else {
            row_word(row, i as usize, tail_mask)
        }
    };
    let hi = get(wi);
    if off == 0 {
        hi
    } else {
        (hi << off) | (get(wi + 1) >> (32 - off))
    }
}

/// Windowed generic-region decoder for template `T` (T.88 §6.2.5).
///
/// The context is kept as one integer and updated incrementally per pixel
/// (see [`Geometry`]); the incoming row-window bits come from two 64-bit
/// look-ahead registers loaded once per 32-pixel block, so the pixel loop has
/// no per-pixel row indexing, shift count or bounds check. The arithmetic
/// decoder's registers live in a local copy for the whole region.
///
/// With nominal AT pixels (`GENERAL == false`) a block whose incoming
/// row-window bits are all equal to `v`, entered with the all-`v` context
/// (`0` or all ones), decodes its leading run of MPS `v` pixels with
/// [`ArithmeticDecoder::decode_mps_run`]: inside such a run the context does
/// not change, so the arithmetic steps fold into one subtraction. This is the
/// common case of page-sized masks, whose background is uniform.
///
/// With other AT positions (`GENERAL == true`) the AT slots are masked out of
/// the window context and the AT pixels are read from their own per-block
/// windows (rows above) or from a history of the current row.
fn decode_windowed<const T: u8, const GENERAL: bool>(
    bitmap: &mut MonoBitmap,
    decoder: &mut ArithmeticDecoder<'_>,
    contexts: &mut [MqContext],
    at: [(i8, i8); 4],
    tpgdon: bool,
    scratch: &mut GenericScratch,
) {
    let g = const { geometry(T) };
    let full: u32 = (1u32 << g.bits) - 1;
    let keep: u32 =
        full & !1 & !(1u32 << g.p1_lsb) & if g.has_p2 { !(1u32 << g.p2_lsb) } else { !0 };
    let mut at_mask = 0u32;
    for &bit in g.at_bits.iter().take(g.at_count) {
        at_mask |= 1u32 << bit;
    }

    let width = bitmap.width() as usize;
    let height = bitmap.height();
    let stride = bitmap.stride_words() as usize;
    if width == 0 || height == 0 {
        return;
    }
    let tail_mask = match width & 31 {
        0 => u32::MAX,
        bits => u32::MAX << (32 - bits),
    };
    let (zero_row, cur) = scratch.rows(stride);
    // Every context index is masked to 16 bits, so this fixed-size view lets
    // the indexing compile without bounds checks (`len >= 1 << 16` is checked
    // by every public entry point).
    let ctxs: &mut [MqContext] = &mut contexts[..1 << 16];
    let mut dec = decoder.clone();
    let mut ltp = false;

    for y in 0..height {
        if tpgdon {
            let sltp = dec.decode_bit(&mut ctxs[g.sltp & 0xFFFF]);
            ltp ^= sltp;
            if ltp {
                copy_prev_row(bitmap, y, cur);
                continue;
            }
        }
        let prev1: &[u32] = if y >= 1 { bitmap.row(y - 1) } else { zero_row };
        let prev2: &[u32] = if g.has_p2 && y >= 2 {
            bitmap.row(y - 2)
        } else {
            zero_row
        };

        // AT sources for this row (general instantiation only).
        let mut at_src = [AtSource::Zero; 4];
        let mut any_current = false;
        if GENERAL {
            for (k, src) in at_src.iter_mut().enumerate().take(g.at_count) {
                let (dx, dy) = (at[k].0 as i64, at[k].1 as i64);
                *src = if dy < 0 {
                    let yy = y as i64 + dy;
                    if yy < 0 {
                        AtSource::Zero
                    } else {
                        AtSource::Row(yy as u32)
                    }
                } else if dy == 0 && dx < 0 {
                    any_current = true;
                    AtSource::Current((-dx) as u32)
                } else {
                    AtSource::Zero
                };
            }
        }
        // Current-row history for `AtSource::Current` (bit 0 = pixel x-1).
        let mut hist: u128 = 0;

        let w1_first = row_word(prev1, 0, tail_mask);
        let w2_first = row_word(prev2, 0, tail_mask);
        let mut ctx: u32 = ((w1_first >> (31 - g.r1)) << g.p1_lsb)
            | if g.has_p2 {
                (w2_first >> (31 - g.r2)) << g.p2_lsb
            } else {
                0
            };
        let (mut w1, mut w2) = (w1_first, w2_first);

        for block in 0..stride {
            let count = if block + 1 == stride {
                (width - block * 32) as u32
            } else {
                32
            };
            let w1n = row_word(prev1, block + 1, tail_mask);
            let w2n = row_word(prev2, block + 1, tail_mask);
            let mut above1 = (((w1 as u64) << 32) | w1n as u64) << (g.r1 + 1);
            let mut above2 = if g.has_p2 {
                (((w2 as u64) << 32) | w2n as u64) << (g.r2 + 1)
            } else {
                0
            };
            (w1, w2) = (w1n, w2n);

            let mut at_win = [0u32; 4];
            if GENERAL {
                for k in 0..g.at_count {
                    if let AtSource::Row(yy) = at_src[k] {
                        let start = (block * 32) as i64 + at[k].0 as i64;
                        at_win[k] = row_window(bitmap.row(yy), start, tail_mask);
                    }
                }
            }

            let mut pixels = 0u32;
            let mut i = 0u32;
            while i < count {
                if !GENERAL && (ctx == 0 || ctx == full) {
                    // Uniform-run entry: the context is all `v`, and so are
                    // the next `rest` incoming window bits, so the context
                    // stays `ctx` for `rest` pixels while they decode as `v`.
                    let v = ctx != 0;
                    let fill = if v { u64::MAX } else { 0 };
                    let diff = (above1 ^ fill) | if g.has_p2 { above2 ^ fill } else { 0 };
                    let rest = (count - i).min(diff.leading_zeros());
                    if rest != 0 {
                        let slot = &mut ctxs[ctx as usize & 0xFFFF];
                        let mut k = 0u32;
                        let mut differs = false;
                        loop {
                            k += dec.decode_mps_run(*slot, v, rest - k);
                            if k == rest {
                                break;
                            }
                            let bit = dec.decode_bit(slot);
                            k += 1;
                            if bit != v {
                                differs = true;
                                break;
                            }
                            if k == rest {
                                break;
                            }
                        }
                        // `k` is 1..=32.
                        let run = if v { (1u64 << k) - 1 } else { 0 } ^ differs as u64;
                        pixels = (((pixels as u64) << k) | run) as u32;
                        ctx ^= differs as u32;
                        above1 <<= k;
                        above2 <<= k;
                        i += k;
                        continue;
                    }
                }
                // The next context is `base | bit`.
                let base = ((ctx << 1) & keep)
                    | (((above1 >> 63) as u32) << g.p1_lsb)
                    | if g.has_p2 {
                        ((above2 >> 63) as u32) << g.p2_lsb
                    } else {
                        0
                    };
                let bit = if GENERAL {
                    let mut a = 0u32;
                    for k in 0..g.at_count {
                        let bit = match at_src[k] {
                            AtSource::Zero => 0,
                            AtSource::Row(_) => at_win[k] >> 31,
                            AtSource::Current(sh) => ((hist >> (sh - 1)) & 1) as u32,
                        };
                        a |= bit << g.at_bits[k];
                    }
                    let idx = (ctx & !at_mask) | a;
                    dec.decode_bit(&mut ctxs[idx as usize & 0xFFFF]) as u32
                } else {
                    dec.decode_bit(&mut ctxs[ctx as usize & 0xFFFF]) as u32
                };
                pixels = (pixels << 1) | bit;
                ctx = base | bit;
                above1 <<= 1;
                above2 <<= 1;
                if GENERAL {
                    for w in at_win.iter_mut().take(g.at_count) {
                        *w <<= 1;
                    }
                    if any_current {
                        hist = (hist << 1) | bit as u128;
                    }
                }
                i += 1;
            }
            // `count` is 1..=32, so the shift is 0..=31.
            cur[block] = pixels << (32 - count);
        }
        bitmap.row_mut(y).copy_from_slice(cur);
    }
    *decoder = dec;
}

/// Copy row `y-1` into row `y` (a TPGDON duplicated row). `scratch` is a stride
/// sized buffer used to break the aliasing borrow. For `y == 0` the copied row
/// is all-zero.
#[inline]
fn copy_prev_row(bitmap: &mut MonoBitmap, y: u32, scratch: &mut [u32]) {
    if y >= 1 {
        scratch.copy_from_slice(bitmap.row(y - 1));
    } else {
        for w in scratch.iter_mut() {
            *w = 0;
        }
    }
    bitmap.row_mut(y).copy_from_slice(scratch);
}

/// Sample an adaptive-template pixel at `(x+dx, y+dy)` relative to the pixel
/// being decoded at row `y`, reading only causal (already-decoded) data.
///
/// A generic-region AT pixel may reference any earlier row (`Ay` down to −128,
/// T.88 §7.4.6.3), so — unlike the fixed template pixels, which never reach past
/// `y−2` — this reads the destination bitmap directly rather than a pair of
/// cached `prev` rows. `dy == 0` is the current row (only columns `< x` are
/// decoded); `dy < 0` reads row `y+dy` (all-zero above the top edge); `dy > 0`
/// (never emitted for a valid AT pixel) reads nothing.
#[inline(always)]
fn at_pixel(bitmap: &MonoBitmap, cur: &[u32], width: u32, y: u32, x: i64, dx: i64, dy: i64) -> u32 {
    let xx = x + dx;
    if xx < 0 || xx >= width as i64 {
        return 0;
    }
    if dy == 0 {
        if xx >= x { 0 } else { sample(cur, width, xx) }
    } else {
        let yy = y as i64 + dy;
        if yy < 0 || dy > 0 {
            0
        } else {
            sample(bitmap.row(yy as u32), width, xx)
        }
    }
}

/// The pre-optimisation generic decoders, kept verbatim (with the reference
/// MQ `DECODE`) as the bit-exactness oracle for the windowed decoder.
#[cfg(all(test, feature = "encode"))]
pub(crate) mod reference {
    use super::*;

    /// Dispatch to the correct template decoder. `template` selects the pixel
    /// neighbourhood (0–3); values outside that range are clamped to 3's behaviour
    /// by the parser (the flags field is only 2 bits wide, so `template <= 3`
    /// always holds).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn decode_into(
        bitmap: &mut MonoBitmap,
        decoder: &mut ArithmeticDecoder<'_>,
        template: u8,
        at: [(i8, i8); 4],
        tpgdon: bool,
        contexts: &mut [MqContext],
        scratch: &mut GenericScratch,
    ) {
        match template {
            0 if at == NOMINAL_AT0 => {
                decode_template0_nominal(bitmap, decoder, contexts, tpgdon, scratch)
            }
            0 => decode_template0_general(bitmap, decoder, contexts, at, tpgdon, scratch),
            1 => decode_template1(bitmap, decoder, contexts, at[0], tpgdon, scratch),
            2 => decode_template2(bitmap, decoder, contexts, at[0], tpgdon, scratch),
            _ => decode_template3(bitmap, decoder, contexts, at[0], tpgdon, scratch),
        }
    }

    /// Fast rolling path for nominal AT (mirrors the encoder exactly). `tpgdon`
    /// enables typical prediction (T.88 §6.2.5.7): an SLTP pseudo-pixel decoded
    /// before each row toggles the LTP flag; while LTP is set, the row is copied
    /// verbatim from the row above (all-zero for the first row).
    pub(crate) fn decode_template0_nominal(
        bitmap: &mut MonoBitmap,
        decoder: &mut ArithmeticDecoder<'_>,
        contexts: &mut [MqContext],
        tpgdon: bool,
        scratch: &mut GenericScratch,
    ) {
        let width = bitmap.width();
        let height = bitmap.height();
        let stride = bitmap.stride_words() as usize;
        let (zero_row, cur) = scratch.rows(stride);
        let mut ltp = false;

        for y in 0..height {
            if tpgdon {
                let sltp = decoder.decode_bit_reference(&mut contexts[SLTP_CTX_T0]);
                ltp ^= sltp;
                if ltp {
                    copy_prev_row(bitmap, y, cur);
                    continue;
                }
            }
            for w in cur.iter_mut() {
                *w = 0;
            }
            // Immutable borrows of previously decoded rows.
            let prev1: &[u32] = if y >= 1 { bitmap.row(y - 1) } else { zero_row };
            let prev2: &[u32] = if y >= 2 { bitmap.row(y - 2) } else { zero_row };

            let mut c1 = (sample(prev2, width, 0) << 2)
                | (sample(prev2, width, 1) << 1)
                | sample(prev2, width, 2);
            let mut c2 = (sample(prev1, width, 0) << 3)
                | (sample(prev1, width, 1) << 2)
                | (sample(prev1, width, 2) << 1)
                | sample(prev1, width, 3);
            let mut c3 = 0u32;

            for x in 0..width {
                let tval = ((c1 << 11) | (c2 << 4) | c3) as usize;
                // tval < 2^16 <= contexts.len(); indexing is in range.
                let bit = decoder.decode_bit_reference(&mut contexts[tval]);
                if bit {
                    let xi = x as usize;
                    cur[xi >> 5] |= 1u32 << (31 - (xi & 31));
                }
                let xi = x as i64;
                c1 = ((c1 << 1) | sample(prev2, width, xi + 3)) & 31;
                c2 = ((c2 << 1) | sample(prev1, width, xi + 4)) & 127;
                c3 = ((c3 << 1) | bit as u32) & 15;
            }

            bitmap.row_mut(y).copy_from_slice(cur);
        }
    }

    /// General per-pixel path for arbitrary AT positions. Produces identical
    /// context values to the rolling path when `at == NOMINAL_AT0`.
    pub(crate) fn decode_template0_general(
        bitmap: &mut MonoBitmap,
        decoder: &mut ArithmeticDecoder<'_>,
        contexts: &mut [MqContext],
        at: [(i8, i8); 4],
        tpgdon: bool,
        scratch: &mut GenericScratch,
    ) {
        let width = bitmap.width();
        let height = bitmap.height();
        let stride = bitmap.stride_words() as usize;
        let (zero_row, cur) = scratch.rows(stride);
        let mut ltp = false;

        // Context bit layout (MSB..LSB), matching the encoder's tval arrangement.
        // Fixed pixels and the four AT slots (nominal offsets in comments):
        //   bit15 AT4 (-2,-2)   bit14 (-1,-2)  bit13 (0,-2)  bit12 (1,-2)
        //   bit11 AT3 (2,-2)    bit10 AT2 (-3,-1) bit9 (-2,-1) bit8 (-1,-1)
        //   bit7  (0,-1)        bit6  (1,-1)   bit5 (2,-1)   bit4 AT1 (3,-1)
        //   bit3  (-4,0)        bit2  (-3,0)   bit1 (-2,0)   bit0 (-1,0)
        let (a1x, a1y) = (at[0].0 as i64, at[0].1 as i64);
        let (a2x, a2y) = (at[1].0 as i64, at[1].1 as i64);
        let (a3x, a3y) = (at[2].0 as i64, at[2].1 as i64);
        let (a4x, a4y) = (at[3].0 as i64, at[3].1 as i64);

        for y in 0..height {
            if tpgdon {
                let sltp = decoder.decode_bit_reference(&mut contexts[SLTP_CTX_T0]);
                ltp ^= sltp;
                if ltp {
                    copy_prev_row(bitmap, y, &mut *cur);
                    continue;
                }
            }
            for w in cur.iter_mut() {
                *w = 0;
            }
            let prev1: &[u32] = if y >= 1 { bitmap.row(y - 1) } else { zero_row };
            let prev2: &[u32] = if y >= 2 { bitmap.row(y - 2) } else { zero_row };

            // Read a fixed template pixel (dy in {-2,-1,0}) from already-decoded
            // data. The four AT slots use `at_pixel`, which reads the bitmap so an
            // AT pixel may reference any earlier row, not just `y-1`/`y-2`.
            let px = |cur: &[u32], x: i64, dx: i64, dy: i64| -> u32 {
                let xx = x + dx;
                match dy {
                    0 => {
                        // Current row: only x' < x is decoded.
                        if xx < 0 || xx >= x || xx >= width as i64 {
                            0
                        } else {
                            sample(cur, width, xx)
                        }
                    }
                    -1 => sample(prev1, width, xx),
                    _ => sample(prev2, width, xx),
                }
            };

            for x in 0..width {
                let x = x as i64;
                let mut t = 0u32;
                t |= at_pixel(bitmap, &*cur, width, y, x, a4x, a4y) << 15;
                t |= px(&*cur, x, -1, -2) << 14;
                t |= px(&*cur, x, 0, -2) << 13;
                t |= px(&*cur, x, 1, -2) << 12;
                t |= at_pixel(bitmap, &*cur, width, y, x, a3x, a3y) << 11;
                t |= at_pixel(bitmap, &*cur, width, y, x, a2x, a2y) << 10;
                t |= px(&*cur, x, -2, -1) << 9;
                t |= px(&*cur, x, -1, -1) << 8;
                t |= px(&*cur, x, 0, -1) << 7;
                t |= px(&*cur, x, 1, -1) << 6;
                t |= px(&*cur, x, 2, -1) << 5;
                t |= at_pixel(bitmap, &*cur, width, y, x, a1x, a1y) << 4;
                t |= px(&*cur, x, -4, 0) << 3;
                t |= px(&*cur, x, -3, 0) << 2;
                t |= px(&*cur, x, -2, 0) << 1;
                t |= px(&*cur, x, -1, 0);

                let bit = decoder.decode_bit_reference(&mut contexts[t as usize]);
                if bit {
                    let xi = x as usize;
                    cur[xi >> 5] |= 1u32 << (31 - (xi & 31));
                }
            }
            bitmap.row_mut(y).copy_from_slice(cur);
        }
    }

    /// Generic template 1 (T.88 §6.2.5.3, Figure 5): 13-bit context, one AT pixel.
    /// Context numbering matches T.88/jbig2dec so the SLTP constant is the literal
    /// spec value.
    fn decode_template1(
        bitmap: &mut MonoBitmap,
        decoder: &mut ArithmeticDecoder<'_>,
        contexts: &mut [MqContext],
        at1: (i8, i8),
        tpgdon: bool,
        scratch: &mut GenericScratch,
    ) {
        let width = bitmap.width();
        let height = bitmap.height();
        let stride = bitmap.stride_words() as usize;
        let (zero_row, cur) = scratch.rows(stride);
        let mut ltp = false;
        let (a1x, a1y) = (at1.0 as i64, at1.1 as i64);

        for y in 0..height {
            if tpgdon {
                let sltp = decoder.decode_bit_reference(&mut contexts[SLTP_CTX_T1]);
                ltp ^= sltp;
                if ltp {
                    copy_prev_row(bitmap, y, &mut *cur);
                    continue;
                }
            }
            for w in cur.iter_mut() {
                *w = 0;
            }
            let prev1: &[u32] = if y >= 1 { bitmap.row(y - 1) } else { zero_row };
            let prev2: &[u32] = if y >= 2 { bitmap.row(y - 2) } else { zero_row };

            for x in 0..width {
                let xi = x as i64;
                let mut t = 0u32;
                t |= sample(&*cur, width, xi - 1);
                t |= sample(&*cur, width, xi - 2) << 1;
                t |= sample(&*cur, width, xi - 3) << 2;
                t |= at_pixel(bitmap, &*cur, width, y, xi, a1x, a1y) << 3;
                t |= sample(prev1, width, xi + 2) << 4;
                t |= sample(prev1, width, xi + 1) << 5;
                t |= sample(prev1, width, xi) << 6;
                t |= sample(prev1, width, xi - 1) << 7;
                t |= sample(prev1, width, xi - 2) << 8;
                t |= sample(prev2, width, xi + 2) << 9;
                t |= sample(prev2, width, xi + 1) << 10;
                t |= sample(prev2, width, xi) << 11;
                t |= sample(prev2, width, xi - 1) << 12;

                let bit = decoder.decode_bit_reference(&mut contexts[t as usize]);
                if bit {
                    let xu = x as usize;
                    cur[xu >> 5] |= 1u32 << (31 - (xu & 31));
                }
            }
            bitmap.row_mut(y).copy_from_slice(cur);
        }
    }

    /// Generic template 2 (T.88 §6.2.5.3, Figure 6): 10-bit context, one AT pixel.
    fn decode_template2(
        bitmap: &mut MonoBitmap,
        decoder: &mut ArithmeticDecoder<'_>,
        contexts: &mut [MqContext],
        at1: (i8, i8),
        tpgdon: bool,
        scratch: &mut GenericScratch,
    ) {
        let width = bitmap.width();
        let height = bitmap.height();
        let stride = bitmap.stride_words() as usize;
        let (zero_row, cur) = scratch.rows(stride);
        let mut ltp = false;
        let (a1x, a1y) = (at1.0 as i64, at1.1 as i64);

        for y in 0..height {
            if tpgdon {
                let sltp = decoder.decode_bit_reference(&mut contexts[SLTP_CTX_T2]);
                ltp ^= sltp;
                if ltp {
                    copy_prev_row(bitmap, y, &mut *cur);
                    continue;
                }
            }
            for w in cur.iter_mut() {
                *w = 0;
            }
            let prev1: &[u32] = if y >= 1 { bitmap.row(y - 1) } else { zero_row };
            let prev2: &[u32] = if y >= 2 { bitmap.row(y - 2) } else { zero_row };

            for x in 0..width {
                let xi = x as i64;
                let mut t = 0u32;
                t |= sample(&*cur, width, xi - 1);
                t |= sample(&*cur, width, xi - 2) << 1;
                t |= at_pixel(bitmap, &*cur, width, y, xi, a1x, a1y) << 2;
                t |= sample(prev1, width, xi + 1) << 3;
                t |= sample(prev1, width, xi) << 4;
                t |= sample(prev1, width, xi - 1) << 5;
                t |= sample(prev1, width, xi - 2) << 6;
                t |= sample(prev2, width, xi + 1) << 7;
                t |= sample(prev2, width, xi) << 8;
                t |= sample(prev2, width, xi - 1) << 9;

                let bit = decoder.decode_bit_reference(&mut contexts[t as usize]);
                if bit {
                    let xu = x as usize;
                    cur[xu >> 5] |= 1u32 << (31 - (xu & 31));
                }
            }
            bitmap.row_mut(y).copy_from_slice(cur);
        }
    }

    /// Generic template 3 (T.88 §6.2.5.3, Figure 7): 10-bit context, one AT pixel,
    /// two rows only (no `y-2` line).
    fn decode_template3(
        bitmap: &mut MonoBitmap,
        decoder: &mut ArithmeticDecoder<'_>,
        contexts: &mut [MqContext],
        at1: (i8, i8),
        tpgdon: bool,
        scratch: &mut GenericScratch,
    ) {
        let width = bitmap.width();
        let height = bitmap.height();
        let stride = bitmap.stride_words() as usize;
        let (zero_row, cur) = scratch.rows(stride);
        let mut ltp = false;
        let (a1x, a1y) = (at1.0 as i64, at1.1 as i64);

        for y in 0..height {
            if tpgdon {
                let sltp = decoder.decode_bit_reference(&mut contexts[SLTP_CTX_T3]);
                ltp ^= sltp;
                if ltp {
                    copy_prev_row(bitmap, y, &mut *cur);
                    continue;
                }
            }
            for w in cur.iter_mut() {
                *w = 0;
            }
            let prev1: &[u32] = if y >= 1 { bitmap.row(y - 1) } else { zero_row };
            // Template 3's fixed pixels use only rows y and y-1. Its AT pixel may
            // still reference an earlier row, so `at_pixel` reads the bitmap.

            for x in 0..width {
                let xi = x as i64;
                let mut t = 0u32;
                t |= sample(&*cur, width, xi - 1);
                t |= sample(&*cur, width, xi - 2) << 1;
                t |= sample(&*cur, width, xi - 3) << 2;
                t |= sample(&*cur, width, xi - 4) << 3;
                t |= at_pixel(bitmap, &*cur, width, y, xi, a1x, a1y) << 4;
                t |= sample(prev1, width, xi + 1) << 5;
                t |= sample(prev1, width, xi) << 6;
                t |= sample(prev1, width, xi - 1) << 7;
                t |= sample(prev1, width, xi - 2) << 8;
                t |= sample(prev1, width, xi - 3) << 9;

                let bit = decoder.decode_bit_reference(&mut contexts[t as usize]);
                if bit {
                    let xu = x as usize;
                    cur[xu >> 5] |= 1u32 << (31 - (xu & 31));
                }
            }
            bitmap.row_mut(y).copy_from_slice(cur);
        }
    }
}

#[cfg(all(test, feature = "encode"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::decode::context::GENERIC_CONTEXT_COUNT;
    use crate::encode::arith::Jbig2ArithCoder;
    use crate::encode::sym::BitImage;

    fn contexts() -> Vec<MqContext> {
        vec![MqContext::default(); GENERIC_CONTEXT_COUNT]
    }

    /// Encode `img` as a raw generic payload and decode it back, asserting a
    /// pixel-exact round-trip.
    fn roundtrip_bitimage(img: &BitImage) {
        let data = Jbig2ArithCoder::encode_generic_payload(img, 0, &nominal_at0_i8()).unwrap();
        let (w, h) = (img.width as u32, img.height as u32);
        let region = GenericRegion {
            width: w,
            height: h,
            x: 0,
            y: 0,
            comb_operator: 0,
            mmr: false,
            template: 0,
            tpgdon: false,
            at: NOMINAL_AT0,
            data: &data,
        };
        let limits = DecodeLimits::default();
        let mut ctx = contexts();
        let mut scratch = GenericScratch::default();
        let bm = decode_generic_region(&region, &limits, &mut ctx, &mut scratch).unwrap();
        for y in 0..h {
            for x in 0..w {
                assert_eq!(bm.get(x, y), img.get(x, y), "pixel ({x},{y}) w={w} h={h}");
            }
        }
    }

    fn nominal_at0_i8() -> Vec<(i8, i8)> {
        NOMINAL_AT0.to_vec()
    }

    fn make_image(w: u32, h: u32, seed: u32) -> BitImage {
        let mut img = BitImage::new(w, h).unwrap();
        let mut s = seed.wrapping_add(1);
        for y in 0..h {
            for x in 0..w {
                s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                if (s >> 16) & 1 == 1 {
                    img.set(x, y, true);
                }
            }
        }
        img
    }

    #[test]
    fn roundtrip_odd_widths() {
        for &w in &[1u32, 2, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65] {
            for &h in &[1u32, 3, 8, 17] {
                roundtrip_bitimage(&make_image(w, h, w * 31 + h));
            }
        }
    }

    #[test]
    fn nominal_and_general_paths_agree() {
        // Decode the same stream with both paths and compare.
        let img = make_image(37, 20, 7);
        let data = Jbig2ArithCoder::encode_generic_payload(&img, 0, &nominal_at0_i8()).unwrap();
        let limits = DecodeLimits::default();

        let mut scratch = GenericScratch::default();
        let mut bm_fast = MonoBitmap::new(37, 20, false, &limits).unwrap();
        let mut d1 = ArithmeticDecoder::new(&data);
        let mut c1 = contexts();
        reference::decode_template0_nominal(&mut bm_fast, &mut d1, &mut c1, false, &mut scratch);

        let mut bm_gen = MonoBitmap::new(37, 20, false, &limits).unwrap();
        let mut d2 = ArithmeticDecoder::new(&data);
        let mut c2 = contexts();
        reference::decode_template0_general(
            &mut bm_gen,
            &mut d2,
            &mut c2,
            NOMINAL_AT0,
            false,
            &mut scratch,
        );

        assert_eq!(bm_fast, bm_gen);
    }

    // ---- Windowed decoder against the pre-optimisation reference ----------

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }

        fn below(&mut self, n: u32) -> u32 {
            self.next() % n
        }
    }

    /// A test image: a uniform background of either polarity with filled
    /// rectangles of the other, sparse noise, and runs of repeated rows (for
    /// TPGDON), or pure noise, or a blank page of either polarity.
    fn mixed_image(w: u32, h: u32, rng: &mut Rng) -> MonoBitmap {
        let limits = DecodeLimits::default();
        let kind = rng.below(6);
        let bg = rng.below(2) == 1;
        let mut img = MonoBitmap::new(w, h, bg, &limits).unwrap();
        match kind {
            0 => {
                for y in 0..h {
                    for x in 0..w {
                        img.set(x, y, rng.below(2) == 1);
                    }
                }
            }
            1 => {} // blank page of polarity `bg`
            _ => {
                for _ in 0..rng.below(8) {
                    let (x0, y0) = (rng.below(w), rng.below(h));
                    let (rw, rh) = (1 + rng.below(w), 1 + rng.below(h.min(12)));
                    for y in y0..(y0 + rh).min(h) {
                        for x in x0..(x0 + rw).min(w) {
                            img.set(x, y, !bg);
                        }
                    }
                }
                let noise = rng.below(4) * 16 + 1;
                for y in 0..h {
                    for x in 0..w {
                        if rng.below(1000) < noise {
                            img.set(x, y, !img.get(x, y));
                        }
                    }
                }
                // Duplicate some rows downwards.
                for y in 1..h {
                    if rng.below(4) == 0 {
                        let prev = img.row(y - 1).to_vec();
                        img.row_mut(y).copy_from_slice(&prev);
                    }
                }
            }
        }
        img
    }

    /// Encode `img` as a generic-region arithmetic payload for any template,
    /// AT set and TPGDON setting, with contexts formed by the reference
    /// `pixel_context` (the layout every decoder path shares).
    fn encode_any(img: &MonoBitmap, template: u8, at: [(i8, i8); 4], tpgdon: bool) -> Vec<u8> {
        let mut enc = Jbig2ArithCoder::new();
        let (w, h) = (img.width(), img.height());
        let zero = vec![0u32; img.stride_words() as usize];
        let sltp = [SLTP_CTX_T0, SLTP_CTX_T1, SLTP_CTX_T2, SLTP_CTX_T3][template as usize];
        let mut ltp = false;
        for y in 0..h {
            if tpgdon {
                let above: &[u32] = if y >= 1 { img.row(y - 1) } else { &zero };
                let same = img.row(y) == above;
                enc.encode_bit(sltp, same != ltp);
                ltp = same;
                if same {
                    continue;
                }
            }
            let prev1: &[u32] = if y >= 1 { img.row(y - 1) } else { &zero };
            let prev2: &[u32] = if y >= 2 { img.row(y - 2) } else { &zero };
            for x in 0..w {
                let ctx =
                    pixel_context(template, img, img.row(y), prev1, prev2, w, y, x as i64, &at);
                enc.encode_bit(ctx, img.get(x, y));
            }
        }
        enc.into_vec()
    }

    /// Decode a sequence of `sizes` bitmaps from one stream with one shared
    /// context bank (the symbol-dictionary shape) through both the windowed
    /// decoder and the reference, asserting identical bitmaps, decoder
    /// registers and context states after every bitmap.
    #[allow(clippy::too_many_arguments)]
    fn assert_equivalent(
        data: &[u8],
        sizes: &[(u32, u32)],
        template: u8,
        at: [(i8, i8); 4],
        tpgdon: bool,
        initial: &[MqContext],
        what: &str,
    ) {
        let limits = DecodeLimits::default();
        let mut scratch = GenericScratch::default();
        let (mut c_new, mut c_ref) = (initial.to_vec(), initial.to_vec());
        let mut d_new = ArithmeticDecoder::new(data);
        let mut d_ref = ArithmeticDecoder::new(data);
        for (n, &(w, h)) in sizes.iter().enumerate() {
            let mut bm_new = MonoBitmap::new(w, h, false, &limits).unwrap();
            let mut bm_ref = MonoBitmap::new(w, h, false, &limits).unwrap();
            decode_into(
                &mut bm_new,
                &mut d_new,
                template,
                at,
                tpgdon,
                &mut c_new,
                &mut scratch,
            );
            reference::decode_into(
                &mut bm_ref,
                &mut d_ref,
                template,
                at,
                tpgdon,
                &mut c_ref,
                &mut scratch,
            );
            let ctx =
                format!("{what}: bitmap {n} {w}x{h} template {template} at {at:?} tpgdon {tpgdon}");
            assert!(bm_new == bm_ref, "bitmap differs ({ctx})");
            assert_eq!(
                d_new.registers(),
                d_ref.registers(),
                "registers differ ({ctx})"
            );
            assert!(c_new == c_ref, "context states differ ({ctx})");
        }
    }

    fn random_at(rng: &mut Rng, template: u8) -> [(i8, i8); 4] {
        let nominal = if template == 0 {
            NOMINAL_AT0
        } else {
            [NOMINAL_AT1[template as usize - 1], (0, 0), (0, 0), (0, 0)]
        };
        let mut at = nominal;
        let count = if template == 0 { 4 } else { 1 };
        match rng.below(4) {
            // Nominal: the uniform-run fast path.
            0 => {}
            // Valid AT pixels (causal), including the extremes of the range.
            1 | 2 => {
                for slot in at.iter_mut().take(count) {
                    let dy = -(rng.below(4) as i32) - if rng.below(8) == 0 { 120 } else { 0 };
                    let dx = if dy == 0 {
                        {
                            let reach = if rng.below(4) == 0 { 128 } else { 6 };
                            -1 - rng.below(reach) as i32
                        }
                    } else {
                        rng.below(256) as i32 - 128
                    };
                    *slot = (dx.clamp(-128, 127) as i8, dy.clamp(-128, 0) as i8);
                }
            }
            // Arbitrary bytes, including non-causal positions the parser
            // accepts (dy > 0, or dy == 0 with dx >= 0).
            _ => {
                for slot in at.iter_mut().take(count) {
                    *slot = (rng.next() as i8, rng.next() as i8);
                }
            }
        }
        at
    }

    fn random_size(rng: &mut Rng) -> (u32, u32) {
        const WIDTHS: [u32; 17] = [1, 2, 3, 4, 5, 7, 8, 31, 32, 33, 63, 64, 65, 95, 96, 97, 130];
        let w = if rng.below(2) == 0 {
            WIDTHS[rng.below(WIDTHS.len() as u32) as usize]
        } else {
            1 + rng.below(200)
        };
        (w, 1 + rng.below(40))
    }

    /// The windowed decoder is bit-identical to the reference for every
    /// template, nominal and non-nominal (including non-causal) AT pixels,
    /// TPGDON on and off, odd and word-aligned widths, shared context banks
    /// across several bitmaps, pre-trained and invalid context states, and
    /// both well-formed streams and arbitrary bytes.
    #[test]
    fn windowed_decoder_matches_reference() {
        let mut rng = Rng(0x5eed_1234_abcd_0001);
        for case in 0..480u32 {
            let template = (case % 4) as u8;
            let at = random_at(&mut rng, template);
            let tpgdon = rng.below(2) == 1;
            let sizes: Vec<(u32, u32)> = (0..1 + rng.below(3))
                .map(|_| random_size(&mut rng))
                .collect();

            // Initial context bank: fresh, trained (valid states), or with
            // some out-of-table states, which every path treats as "0, no
            // register change".
            let mut initial = contexts();
            match rng.below(4) {
                0 => {
                    for c in initial.iter_mut() {
                        *c = MqContext(rng.below(94) as u8);
                    }
                }
                1 => {
                    for c in initial.iter_mut() {
                        if rng.below(64) == 0 {
                            *c = MqContext(94 + rng.below(162) as u8);
                        }
                    }
                }
                _ => {}
            }

            // A well-formed stream for the first bitmap (realistic statistics
            // and long uniform runs), then arbitrary bytes for the rest of
            // the decoder's consumption.
            let img = mixed_image(sizes[0].0, sizes[0].1, &mut rng);
            let mut data = encode_any(&img, template, at, tpgdon);
            for _ in 0..rng.below(64) {
                data.push(if rng.below(5) == 0 {
                    0xFF
                } else {
                    rng.next() as u8
                });
            }
            assert_equivalent(&data, &sizes, template, at, tpgdon, &initial, "encoded");

            // Pure arbitrary bytes, with 0xFF-heavy runs for marker handling.
            let len = rng.below(600) as usize;
            let junk: Vec<u8> = (0..len)
                .map(|_| {
                    if rng.below(7) == 0 {
                        0xFF
                    } else {
                        rng.next() as u8
                    }
                })
                .collect();
            assert_equivalent(&junk, &sizes, template, at, tpgdon, &initial, "arbitrary");
        }
    }

    /// The well-formed encoded streams also round-trip to their source image
    /// through the windowed decoder (the encoder side of the harness agrees
    /// with the decoder, so the comparison above is not vacuous).
    #[test]
    fn windowed_decoder_roundtrips_any_template() {
        let mut rng = Rng(0x0dd_ba11_5eed_0002);
        let limits = DecodeLimits::default();
        for case in 0..96u32 {
            let template = (case % 4) as u8;
            let at = random_at(&mut rng, template);
            let tpgdon = rng.below(2) == 1;
            let (w, h) = random_size(&mut rng);
            let img = mixed_image(w, h, &mut rng);
            let data = encode_any(&img, template, at, tpgdon);
            let mut bm = MonoBitmap::new(w, h, false, &limits).unwrap();
            let mut dec = ArithmeticDecoder::new(&data);
            let mut ctx = contexts();
            let mut scratch = GenericScratch::default();
            decode_into(
                &mut bm,
                &mut dec,
                template,
                at,
                tpgdon,
                &mut ctx,
                &mut scratch,
            );
            assert!(
                bm == img,
                "round-trip {w}x{h} template {template} at {at:?} tpgdon {tpgdon}"
            );
        }
    }

    /// Real generic-region streams: every arithmetic generic region of every
    /// `*.jb2` file (a bare PDF `/JBIG2Decode` stream) in the directory named
    /// by `JBIG2_EQUIV_STREAMS` decodes identically through both paths. The
    /// streams are not distributed with the crate; without the variable the
    /// test passes vacuously.
    #[test]
    fn windowed_decoder_matches_reference_on_real_streams() {
        let Some(dir) = std::env::var_os("JBIG2_EQUIV_STREAMS") else {
            return;
        };
        let limits = DecodeLimits::default();
        let mut regions = 0usize;
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "jb2"))
            .collect();
        paths.sort();
        for path in paths {
            let bytes = std::fs::read(&path).unwrap();
            let doc = crate::decode::file::parse_embedded_with(
                &bytes,
                &limits,
                crate::decode::DecodeStrictness::Compatible,
            )
            .unwrap();
            for seg in &doc.segments {
                use crate::shared::segment::SegmentType;
                if !matches!(
                    seg.header.segment_type(),
                    Some(
                        SegmentType::ImmediateGenericRegion
                            | SegmentType::ImmediateLosslessGenericRegion
                            | SegmentType::IntermediateGenericRegion
                    )
                ) {
                    continue;
                }
                let region = parse_generic_region(seg.data).unwrap();
                if region.mmr {
                    continue;
                }
                let sizes = [(region.width, region.height)];
                assert_equivalent(
                    region.data,
                    &sizes,
                    region.template,
                    region.at,
                    region.tpgdon,
                    &contexts(),
                    &path.display().to_string(),
                );
                regions += 1;
            }
        }
        eprintln!("compared {regions} real generic regions");
    }
}
