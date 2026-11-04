//! The MQ arithmetic *decoder* (jbig2decplan.md §10, T.88 Annex E).
//!
//! This is the inverse of the encoder's [`crate::encode::arith::Jbig2ArithCoder`]
//! register machine. It shares only the probability table
//! ([`crate::shared::mq_table`]); the decoder registers live here.
//!
//! The software conventions follow T.88 Annex E (INITDEC / DECODE / BYTEIN /
//! RENORMD, Figures E.15–E.20). Running past the end of the coded data feeds
//! `0xFF` bytes — the spec's "marker not found" convention — which produces a
//! stream of MPS/1-bits. Every consumer (generic region, integer decoder, …)
//! decodes a *bounded* number of bits, so a truncated stream always terminates;
//! it never loops or panics.
//!
//! The [`MqContext`] handle indexes [`MQ_STATES`], whose next-state fields
//! already fold in the conditional exchange (`SWITCH`) and encode the current
//! MPS in the index (`>= 47`). `decode_bit` therefore returns an absolute `0/1`
//! bit and rewrites the context index in place — no separate MPS/switch state.

use crate::shared::mq_table::{MQ_STATES, MqContext};

/// MQ arithmetic decoder over a borrowed coded-data slice.
///
/// `c` and `a` are the standard 32-bit code and interval registers (`a` carries
/// its value in the low 16 bits, with `0x8000` the renormalisation threshold);
/// `ct` is the bit-countdown; `bp` indexes the *current* byte `b = data[bp]`.
#[derive(Clone, Debug)]
pub struct ArithmeticDecoder<'a> {
    data: &'a [u8],
    /// Index of the current byte `B`. May advance one past `data.len()`, after
    /// which reads yield the `0xFF` past-end convention.
    bp: usize,
    c: u32,
    a: u32,
    ct: i32,
}

impl<'a> ArithmeticDecoder<'a> {
    /// Initialise the decoder over `data` (T.88 INITDEC, Figure E.20).
    pub fn new(data: &'a [u8]) -> Self {
        let mut dec = ArithmeticDecoder {
            data,
            bp: 0,
            c: 0,
            a: 0,
            ct: 0,
        };
        // C = B << 16 (B is the first byte, or 0xFF past the end).
        let b0 = dec.byte_at(0);
        dec.c = (b0 as u32) << 16;
        dec.byte_in();
        dec.c <<= 7;
        dec.ct -= 7;
        dec.a = 0x8000;
        dec
    }

    /// The byte at absolute index `i`, or `0xFF` past the end (T.88 marker
    /// convention for running past the coded data).
    #[inline(always)]
    fn byte_at(&self, i: usize) -> u8 {
        // `copied().unwrap_or(0xFF)` is not an input-reachable unwrap.
        match self.data.get(i) {
            Some(&b) => b,
            None => 0xFF,
        }
    }

    /// BYTEIN (T.88 Figure E.19, software-conventions form with byte stuffing
    /// after `0xFF`).
    #[inline(always)]
    fn byte_in(&mut self) {
        if self.byte_at(self.bp) == 0xFF {
            let b1 = self.byte_at(self.bp + 1);
            if b1 > 0x8F {
                // Marker (or past-end): feed 1-bits without consuming the byte.
                self.c = self.c.wrapping_add(0xFF00);
                self.ct = 8;
            } else {
                self.bp += 1;
                self.c = self.c.wrapping_add((b1 as u32) << 9);
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            let b1 = self.byte_at(self.bp);
            self.c = self.c.wrapping_add((b1 as u32) << 8);
            self.ct = 8;
        }
    }

    /// RENORMD (T.88 Figure E.18).
    #[inline(always)]
    fn renorm(&mut self) {
        loop {
            if self.ct == 0 {
                self.byte_in();
            }
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.a & 0x8000 != 0 {
                break;
            }
        }
    }

    /// Decode one bit under `context`, updating the context state in place
    /// (T.88 DECODE / MPS_EXCHANGE / LPS_EXCHANGE, Figures E.15–E.17).
    ///
    /// The common case, an MPS that leaves `A` normalised, is tested first
    /// and needs no context write and no renormalisation; every other case
    /// takes [`Self::decode_exchange`], which is exactly the T.88 flow.
    #[inline(always)]
    pub fn decode_bit(&mut self, context: &mut MqContext) -> bool {
        let s = context.0 as usize;
        // Bounds: state indices are always < 94 (table invariant, checked in
        // mq_table tests); use get to stay panic-free regardless.
        let entry = match MQ_STATES.get(s) {
            Some(e) => *e,
            None => return false,
        };
        let qe = entry.qe as u32;
        let a = self.a.wrapping_sub(qe);
        if (self.c >> 16) >= qe && a & 0x8000 != 0 {
            self.a = a;
            self.c = self.c.wrapping_sub(qe << 16);
            return s >= 47;
        }
        self.decode_exchange(context, entry, s >= 47, a)
    }

    /// The exchange half of DECODE: an LPS sub-interval, or an MPS that needs
    /// the conditional exchange and renormalisation. `a` is `A - Qe`.
    #[inline(always)]
    fn decode_exchange(
        &mut self,
        context: &mut MqContext,
        entry: crate::shared::mq_table::MqEntry,
        mps: bool,
        a: u32,
    ) -> bool {
        let qe = entry.qe as u32;
        self.a = a;
        let d;
        if (self.c >> 16) < qe {
            // LPS exchange.
            if self.a < qe {
                d = mps;
                context.0 = entry.next_mps;
            } else {
                d = !mps;
                context.0 = entry.next_lps;
            }
            self.a = qe;
        } else {
            // MPS exchange (`A` dropped below 0x8000).
            self.c = self.c.wrapping_sub(qe << 16);
            if self.a < qe {
                d = !mps;
                context.0 = entry.next_lps;
            } else {
                d = mps;
                context.0 = entry.next_mps;
            }
        }
        self.renorm();
        d
    }

    /// Decode a run of up to `count` symbols under `context` that are each the
    /// context's MPS, equal to `value`, *without* renormalisation, and return
    /// how many were decoded (all of them equal `value`).
    ///
    /// Each such step of [`Self::decode_bit`] subtracts the same `Qe` from `A`
    /// and from the high half of `C` and leaves the context state unchanged,
    /// so `n` steps fold into one subtraction of `n * Qe`. The run stops before
    /// the first symbol that would take the exchange path (an LPS, or `A`
    /// falling below 0x8000) or when the context's MPS is not `value`; the
    /// caller decodes that symbol with [`Self::decode_bit`]. The registers and
    /// the context afterwards are exactly those of `n` `decode_bit` calls.
    #[inline(always)]
    pub(crate) fn decode_mps_run(&mut self, context: MqContext, value: bool, count: u32) -> u32 {
        let s = context.0 as usize;
        let qe = match MQ_STATES.get(s) {
            Some(e) => e.qe as u32,
            None => return 0,
        };
        if (s >= 47) != value || self.a < 0x8000 || qe == 0 {
            return 0;
        }
        let chigh = self.c >> 16;
        // count <= 32 and Qe < 0x5602, so the product cannot overflow.
        let total = qe * count;
        // Step i (1-based) stays on the fast path iff C_high >= i * Qe and
        // A - i * Qe >= 0x8000 (A < 0x10000 always holds after INITDEC/RENORMD).
        let n = if chigh >= total && self.a - 0x8000 >= total {
            count
        } else {
            (chigh / qe).min((self.a - 0x8000) / qe).min(count)
        };
        let d = n * qe;
        self.a -= d;
        self.c = self.c.wrapping_sub(d << 16);
        n
    }

    /// The pre-optimisation DECODE, kept verbatim as the equivalence reference.
    #[cfg(all(test, feature = "encode"))]
    pub(crate) fn decode_bit_reference(&mut self, context: &mut MqContext) -> bool {
        let s = context.0 as usize;
        let entry = match MQ_STATES.get(s) {
            Some(e) => *e,
            None => return false,
        };
        let qe = entry.qe as u32;
        let mps = s >= 47;

        self.a = self.a.wrapping_sub(qe);

        let d;
        if (self.c >> 16) < qe {
            // LPS exchange.
            if self.a < qe {
                d = mps;
                context.0 = entry.next_mps;
            } else {
                d = !mps;
                context.0 = entry.next_lps;
            }
            self.a = qe;
            self.renorm();
        } else {
            self.c = self.c.wrapping_sub(qe << 16);
            if self.a & 0x8000 == 0 {
                // MPS exchange.
                if self.a < qe {
                    d = !mps;
                    context.0 = entry.next_lps;
                } else {
                    d = mps;
                    context.0 = entry.next_mps;
                }
                self.renorm();
            } else {
                d = mps;
            }
        }
        d
    }

    /// Bytes consumed so far (index of the current byte `B`). Informational;
    /// the generic region does not rely on exact terminator alignment.
    #[inline]
    pub fn bytes_consumed(&self) -> usize {
        self.bp
    }

    /// The full register state `(BP, C, A, CT)`, for equivalence tests.
    #[cfg(all(test, feature = "encode"))]
    pub(crate) fn registers(&self) -> (usize, u32, u32, i32) {
        (self.bp, self.c, self.a, self.ct)
    }
}

#[cfg(all(test, feature = "encode"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::encode::arith::Jbig2ArithCoder;

    /// Encode a sequence of (context-id, bit) decisions with the encoder, then
    /// decode them back and assert every bit matches. Contexts are a small
    /// dense array so the decoder and encoder agree on context identity.
    fn roundtrip(decisions: &[(usize, bool)], num_ctx: usize) {
        let mut enc = Jbig2ArithCoder::new();
        for &(ctx, bit) in decisions {
            enc.encode_bit(ctx, bit);
        }
        let data = enc.into_vec();

        let mut dec = ArithmeticDecoder::new(&data);
        let mut ctxs = vec![MqContext::default(); num_ctx];
        for (i, &(ctx, bit)) in decisions.iter().enumerate() {
            let got = dec.decode_bit(&mut ctxs[ctx]);
            assert_eq!(got, bit, "decision {i} (ctx {ctx}) mismatch");
        }
    }

    #[test]
    fn single_context_alternating() {
        let decisions: Vec<(usize, bool)> = (0..500).map(|i| (0usize, i % 2 == 0)).collect();
        roundtrip(&decisions, 1);
    }

    #[test]
    fn long_mps_run() {
        // A single context fed a long run of identical bits drives it deep into
        // the high-MPS states; the decoder must track the same trajectory.
        let decisions: Vec<(usize, bool)> = (0..4000).map(|_| (0usize, true)).collect();
        roundtrip(&decisions, 1);
        let decisions0: Vec<(usize, bool)> = (0..4000).map(|_| (0usize, false)).collect();
        roundtrip(&decisions0, 1);
    }

    #[test]
    fn lps_heavy_pseudorandom() {
        // Deterministic LCG producing an LPS-heavy, bursty pattern.
        let mut state: u32 = 0x1234_5678;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        };
        let decisions: Vec<(usize, bool)> = (0..8000)
            .map(|_| {
                let r = next();
                let ctx = (r >> 3) as usize % 16;
                // Skew towards the less-probable symbol occasionally.
                let bit = (r & 0x7) == 0;
                (ctx, bit)
            })
            .collect();
        roundtrip(&decisions, 16);
    }

    #[test]
    fn stuffing_around_0xff() {
        // Force many carries so the encoder emits 0xFF stuff bytes, exercising
        // the decoder's BYTEIN stuffing branch.
        let mut decisions = Vec::new();
        for i in 0..3000u32 {
            // Mix of contexts and a bias that produces long carry chains.
            decisions.push(((i % 4) as usize, (i.count_ones() & 1) == 0));
        }
        roundtrip(&decisions, 4);
    }

    #[test]
    fn many_contexts() {
        let mut decisions = Vec::new();
        let mut s: u32 = 99;
        for _ in 0..5000 {
            s = s.wrapping_mul(48271) % 0x7FFF_FFFF;
            decisions.push(((s as usize) % 512, (s & 1) == 0));
        }
        roundtrip(&decisions, 512);
    }

    #[test]
    fn truncated_input_does_not_panic() {
        // Decoding past the end of a (deliberately short) buffer must terminate
        // by feeding 1-bits, never panic or loop.
        let data = [0x00u8, 0x11];
        let mut dec = ArithmeticDecoder::new(&data);
        let mut ctx = MqContext::default();
        for _ in 0..100_000 {
            let _ = dec.decode_bit(&mut ctx);
        }
    }

    #[test]
    fn empty_input_does_not_panic() {
        let data: [u8; 0] = [];
        let mut dec = ArithmeticDecoder::new(&data);
        let mut ctx = MqContext::default();
        for _ in 0..1000 {
            let _ = dec.decode_bit(&mut ctx);
        }
    }
}
