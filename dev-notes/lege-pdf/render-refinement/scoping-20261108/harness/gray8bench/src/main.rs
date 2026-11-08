//! P4 Gray8 output-conversion microbenchmark (standalone, no repo deps).
//!
//! `orig_*` is a verbatim copy of the Gray8 branch of
//! lege-pdf/render/crates/pdf-render-cpu/src/surface.rs
//! (`Surface::into_output_recycling`, lines 256-289 at HEAD 5b24f8e), minus
//! the pool parking. The input is the surface's premultiplied RGBA; alpha
//! (px[3]) is never read, so the result is a function of (r, g, b) only.
//!
//! Exact alternatives:
//!   lut3_round     three 256-entry f32 product tables, summed in the same
//!                  order, then the original `round().clamp() as u8`.
//!   lut3_fc        tables + floor/compare rounding: t = y as i32;
//!                  t + ((y - t as f32) >= 0.5) as i32.
//!   arith_fc       original per-pixel arithmetic + floor/compare rounding.
//!   arith_fc_u32   same, reading each pixel as one little-endian u32 (helps
//!                  the auto-vectorizer).
//!   sse2           explicit SSE2 (x86-64 baseline) version of arith_fc:
//!                  identical IEEE ops in identical order, 16 px/iteration.
//!
//! Usage:
//!   gray8bench verify                 exhaustive 2^24 (r,g,b) check (+alpha)
//!   gray8bench dump-orig FILE         write orig output over the 2^24 domain
//!   gray8bench bench W H ROUNDS [rgb.ppm ...]   timing (ns/pixel)
//!   gray8bench apply IN.ppm OUT.pgm   orig conversion of a PPM (alpha=255)

use std::hint::black_box;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Verbatim copy of the renderer's Gray8 conversion.
// ---------------------------------------------------------------------------

fn unique_arc_mut<T>(data: &mut Arc<[T]>) -> &mut [T] {
    let Some(data) = Arc::get_mut(data) else {
        unreachable!("a final buffer is never shared while it is mutable")
    };
    data
}

/// Full `into_output(Gray8)` replica: allocation + loop + tail fill.
#[inline(never)]
#[allow(unsafe_code)]
fn orig_full(data: &Arc<[u8]>, width: usize, height: usize) -> (usize, Arc<[u8]>) {
    let pixels = width * height;
    debug_assert_eq!(data.len(), pixels * 4);
    let mut out = Arc::<[u8]>::new_uninit_slice(pixels);
    let out_mut = unique_arc_mut(&mut out);
    let mut written = 0usize;
    for (dst, px) in out_mut.iter_mut().zip(data.chunks_exact(4)) {
        // Rec. 709 luma of the premultiplied (composited-over-black)
        // color.
        let y = 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32;
        dst.write(y.round().clamp(0.0, 255.0) as u8);
        written += 1;
    }
    for dst in &mut out_mut[written..] {
        dst.write(0);
    }
    // SAFETY: every element initialized above.
    let gray = unsafe { out.assume_init() };
    (width, gray)
}

/// Loop-only form of the same body (preallocated destination).
#[inline(never)]
fn orig_loop(out_mut: &mut [MaybeUninit<u8>], data: &[u8]) -> usize {
    let mut written = 0usize;
    for (dst, px) in out_mut.iter_mut().zip(data.chunks_exact(4)) {
        let y = 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32;
        dst.write(y.round().clamp(0.0, 255.0) as u8);
        written += 1;
    }
    written
}

// ---------------------------------------------------------------------------
// Exact alternatives.
// ---------------------------------------------------------------------------

struct Lut {
    t0: [f32; 256],
    t1: [f32; 256],
    t2: [f32; 256],
}

fn build_lut() -> Lut {
    let mut l = Lut { t0: [0.0; 256], t1: [0.0; 256], t2: [0.0; 256] };
    for i in 0..256usize {
        // Same single IEEE multiplication as `0.2126 * px[0] as f32`.
        l.t0[i] = 0.2126 * i as u8 as f32;
        l.t1[i] = 0.7152 * i as u8 as f32;
        l.t2[i] = 0.0722 * i as u8 as f32;
    }
    l
}

#[inline(always)]
fn round_fc(y: f32) -> u8 {
    // y in [0, 255.5): `y as i32` is floor(y); y - floor(y) is exact (the
    // fractional bits of y), so `>= 0.5` is exact round-half-away-from-zero.
    let t = y as i32;
    (t + ((y - t as f32) >= 0.5) as i32) as u8
}

#[inline(never)]
fn lut3_round(out_mut: &mut [MaybeUninit<u8>], data: &[u8], l: &Lut) -> usize {
    let mut written = 0usize;
    for (dst, px) in out_mut.iter_mut().zip(data.chunks_exact(4)) {
        let y = l.t0[px[0] as usize] + l.t1[px[1] as usize] + l.t2[px[2] as usize];
        dst.write(y.round().clamp(0.0, 255.0) as u8);
        written += 1;
    }
    written
}

#[inline(never)]
fn lut3_fc(out_mut: &mut [MaybeUninit<u8>], data: &[u8], l: &Lut) -> usize {
    let mut written = 0usize;
    for (dst, px) in out_mut.iter_mut().zip(data.chunks_exact(4)) {
        let y = l.t0[px[0] as usize] + l.t1[px[1] as usize] + l.t2[px[2] as usize];
        dst.write(round_fc(y));
        written += 1;
    }
    written
}

#[inline(never)]
fn arith_fc(out_mut: &mut [MaybeUninit<u8>], data: &[u8]) -> usize {
    let mut written = 0usize;
    for (dst, px) in out_mut.iter_mut().zip(data.chunks_exact(4)) {
        let y = 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32;
        dst.write(round_fc(y));
        written += 1;
    }
    written
}

#[inline(never)]
fn arith_fc_u32(out_mut: &mut [MaybeUninit<u8>], data: &[u8]) -> usize {
    let n = out_mut.len().min(data.len() / 4);
    let (out_mut, data) = (&mut out_mut[..n], &data[..n * 4]);
    for (dst, px) in out_mut.iter_mut().zip(data.chunks_exact(4)) {
        let w = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
        let r = (w & 0xff) as f32;
        let g = ((w >> 8) & 0xff) as f32;
        let b = ((w >> 16) & 0xff) as f32;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        dst.write(round_fc(y));
    }
    n
}

#[cfg(target_arch = "x86_64")]
#[inline(never)]
#[allow(unsafe_code)]
fn sse2(out_mut: &mut [MaybeUninit<u8>], data: &[u8]) -> usize {
    use std::arch::x86_64::*;
    let n = out_mut.len().min(data.len() / 4);
    let mut i = 0usize;
    // SAFETY: SSE2 is part of the x86-64 baseline; all loads/stores are
    // unaligned and in bounds (i + 16 <= n, src has 4n bytes, dst has n).
    unsafe {
        let c0 = _mm_set1_ps(0.2126);
        let c1 = _mm_set1_ps(0.7152);
        let c2 = _mm_set1_ps(0.0722);
        let half = _mm_set1_ps(0.5);
        let m = _mm_set1_epi32(0xff);
        let src = data.as_ptr();
        let dst = out_mut.as_mut_ptr() as *mut u8;
        #[inline(always)]
        unsafe fn four(v: __m128i, m: __m128i, c0: __m128, c1: __m128, c2: __m128, half: __m128) -> __m128i {
            unsafe {
                let r = _mm_cvtepi32_ps(_mm_and_si128(v, m));
                let g = _mm_cvtepi32_ps(_mm_and_si128(_mm_srli_epi32(v, 8), m));
                let b = _mm_cvtepi32_ps(_mm_and_si128(_mm_srli_epi32(v, 16), m));
                // (c0*r + c1*g) + c2*b: same products, same association.
                let y = _mm_add_ps(_mm_add_ps(_mm_mul_ps(c0, r), _mm_mul_ps(c1, g)), _mm_mul_ps(c2, b));
                let t = _mm_cvttps_epi32(y);
                let frac = _mm_sub_ps(y, _mm_cvtepi32_ps(t));
                let ge = _mm_castps_si128(_mm_cmpge_ps(frac, half));
                _mm_sub_epi32(t, ge)
            }
        }
        while i + 16 <= n {
            let p = src.add(i * 4) as *const __m128i;
            let a = four(_mm_loadu_si128(p), m, c0, c1, c2, half);
            let b = four(_mm_loadu_si128(p.add(1)), m, c0, c1, c2, half);
            let c = four(_mm_loadu_si128(p.add(2)), m, c0, c1, c2, half);
            let d = four(_mm_loadu_si128(p.add(3)), m, c0, c1, c2, half);
            let packed = _mm_packus_epi16(_mm_packs_epi32(a, b), _mm_packs_epi32(c, d));
            _mm_storeu_si128(dst.add(i) as *mut __m128i, packed);
            i += 16;
        }
    }
    for (dst, px) in out_mut[i..n].iter_mut().zip(data[i * 4..n * 4].chunks_exact(4)) {
        let y = 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32;
        dst.write(round_fc(y));
    }
    n
}

/// Full replica with the best alternative (sse2) as the loop.
#[inline(never)]
#[allow(unsafe_code)]
fn sse2_full(data: &Arc<[u8]>, width: usize, height: usize) -> (usize, Arc<[u8]>) {
    let pixels = width * height;
    let mut out = Arc::<[u8]>::new_uninit_slice(pixels);
    let out_mut = unique_arc_mut(&mut out);
    let written = sse2(out_mut, data);
    for dst in &mut out_mut[written..] {
        dst.write(0);
    }
    // SAFETY: every element initialized above.
    let gray = unsafe { out.assume_init() };
    (width, gray)
}

/// Full replica with lut3_fc as the loop.
#[inline(never)]
#[allow(unsafe_code)]
fn lut3_fc_full(data: &Arc<[u8]>, width: usize, height: usize, l: &Lut) -> (usize, Arc<[u8]>) {
    let pixels = width * height;
    let mut out = Arc::<[u8]>::new_uninit_slice(pixels);
    let out_mut = unique_arc_mut(&mut out);
    let written = lut3_fc(out_mut, data, l);
    for dst in &mut out_mut[written..] {
        dst.write(0);
    }
    // SAFETY: every element initialized above.
    let gray = unsafe { out.assume_init() };
    (width, gray)
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

#[allow(unsafe_code)]
fn init_vec(v: Vec<MaybeUninit<u8>>, written: usize) -> Vec<u8> {
    assert_eq!(written, v.len());
    let mut v = std::mem::ManuallyDrop::new(v);
    // SAFETY: all `written == len` elements were initialized.
    unsafe { Vec::from_raw_parts(v.as_mut_ptr() as *mut u8, v.len(), v.capacity()) }
}

fn run_variant(name: &str, src: &[u8], l: &Lut) -> Vec<u8> {
    let n = src.len() / 4;
    let mut out: Vec<MaybeUninit<u8>> = Vec::with_capacity(n);
    // SAFETY-free: MaybeUninit needs no init.
    out.resize_with(n, MaybeUninit::uninit);
    let w = match name {
        "orig" => orig_loop(&mut out, src),
        "lut3_round" => lut3_round(&mut out, src, l),
        "lut3_fc" => lut3_fc(&mut out, src, l),
        "arith_fc" => arith_fc(&mut out, src),
        "arith_fc_u32" => arith_fc_u32(&mut out, src),
        "sse2" => sse2(&mut out, src),
        _ => panic!("variant {name}"),
    };
    init_vec(out, w)
}

const VARIANTS: [&str; 6] = ["orig", "lut3_round", "lut3_fc", "arith_fc", "arith_fc_u32", "sse2"];

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn domain(alpha_mode: u32) -> Vec<u8> {
    let mut v = vec![0u8; (1usize << 24) * 4];
    for (i, px) in v.chunks_exact_mut(4).enumerate() {
        let r = (i & 0xff) as u8;
        let g = ((i >> 8) & 0xff) as u8;
        let b = ((i >> 16) & 0xff) as u8;
        let a = match alpha_mode {
            0 => 255u8,
            1 => 0u8,
            _ => (i as u32).wrapping_mul(2654435761).rotate_right(13) as u8,
        };
        px.copy_from_slice(&[r, g, b, a]);
    }
    v
}

fn verify() {
    let l = build_lut();
    // Sanity: the table entries are bit-identical to the inline products.
    for i in 0..256u32 {
        let x = i as u8;
        assert_eq!(l.t0[i as usize].to_bits(), (0.2126 * x as f32).to_bits());
        assert_eq!(l.t1[i as usize].to_bits(), (0.7152 * x as f32).to_bits());
        assert_eq!(l.t2[i as usize].to_bits(), (0.0722 * x as f32).to_bits());
    }
    let mut reference: Option<Vec<u8>> = None;
    for alpha_mode in [0u32, 1, 2] {
        let src = domain(alpha_mode);
        let base = run_variant("orig", &src, &l);
        println!(
            "alpha_mode={alpha_mode} orig fnv1a={:016x} (alpha ignored check vs mode0: {})",
            fnv1a(&base),
            match &reference {
                None => "n/a (this is mode0)".to_string(),
                Some(r) => format!("{}", if *r == base { "identical" } else { "DIFFERENT" }),
            }
        );
        for name in VARIANTS.iter().skip(1) {
            let out = run_variant(name, &src, &l);
            let mism: Vec<usize> =
                out.iter().zip(&base).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
            println!(
                "alpha_mode={alpha_mode} {name:>12}: mismatches={} of {} fnv1a={:016x}{}",
                mism.len(),
                base.len(),
                fnv1a(&out),
                mism.first()
                    .map(|&i| format!(" first at r={} g={} b={}", i & 0xff, (i >> 8) & 0xff, (i >> 16) & 0xff))
                    .unwrap_or_default()
            );
        }
        if reference.is_none() {
            // Extra stats about the reference function over the domain.
            let mut exact_ties = 0u64;
            let mut max_y = 0f32;
            let mut near = 0u64;
            for i in 0..(1usize << 24) {
                let (r, g, b) = ((i & 0xff) as u8, ((i >> 8) & 0xff) as u8, ((i >> 16) & 0xff) as u8);
                let y = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
                if y > max_y {
                    max_y = y;
                }
                let f = y - y.floor();
                if f == 0.5 {
                    exact_ties += 1;
                }
                if (f - 0.5).abs() < 1e-4 {
                    near += 1;
                }
            }
            println!(
                "domain stats: max_y={max_y:.7} ({:#010x}), exact f32 ties (frac==0.5)={exact_ties}, |frac-0.5|<1e-4: {near}",
                max_y.to_bits()
            );
            reference = Some(base);
        }
    }
    // Full (allocating) replicas agree with the loop-only form on a page-sized
    // slice of the domain.
    let src = domain(0);
    let w = 4096usize;
    let h = 4096usize;
    let arc: Arc<[u8]> = Arc::from(&src[..w * h * 4]);
    let (_, a) = orig_full(&arc, w, h);
    let (_, b) = sse2_full(&arc, w, h);
    let (_, c) = lut3_fc_full(&arc, w, h, &l);
    println!("full replicas over 2^24 px: orig==sse2_full {} orig==lut3_fc_full {}", *a == *b, *a == *c);
}

fn read_ppm(path: &str) -> (usize, usize, Vec<u8>) {
    let bytes = std::fs::read(path).expect("read ppm");
    // P6\nW H\n255\n
    let mut fields = Vec::new();
    let mut pos = 0usize;
    while fields.len() < 4 {
        while bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let start = pos;
        while !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        fields.push(String::from_utf8_lossy(&bytes[start..pos]).to_string());
    }
    pos += 1;
    assert_eq!(fields[0], "P6");
    let w: usize = fields[1].parse().unwrap();
    let h: usize = fields[2].parse().unwrap();
    let rgb = &bytes[pos..pos + w * h * 3];
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in rgb.chunks_exact(3) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    (w, h, rgba)
}

/// Tile/crop an RGBA image into a W x H buffer.
fn fit(w0: usize, h0: usize, src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let sx = x % w0;
            let sy = y % h0;
            out[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&src[(sy * w0 + sx) * 4..(sy * w0 + sx) * 4 + 4]);
        }
    }
    out
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { 0.5 * (v[n / 2 - 1] + v[n / 2]) }
}

fn bench(w: usize, h: usize, rounds: usize, ppms: &[String]) {
    let l = build_lut();
    let n = w * h;
    let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();
    // Random RGBA.
    let mut s = 0x9E3779B97F4A7C15u64;
    let mut rnd = vec![0u8; n * 4];
    for b in rnd.iter_mut() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *b = (s >> 32) as u8;
    }
    inputs.push(("random".into(), rnd));
    for p in ppms {
        let (w0, h0, rgba) = read_ppm(p);
        let name = std::path::Path::new(p).parent().unwrap().file_name().unwrap().to_string_lossy().to_string();
        inputs.push((name, fit(w0, h0, &rgba, w, h)));
    }
    let loop_names: Vec<&str> = VARIANTS.to_vec();
    let full_names = ["orig_full", "lut3_fc_full", "sse2_full"];
    let mut out: Vec<MaybeUninit<u8>> = Vec::with_capacity(n);
    out.resize_with(n, MaybeUninit::uninit);
    println!("#input\tvariant\tmedian_ns_per_px\tmin_ns_per_px\tmax_ns_per_px\tmedian_ms\trounds\tfnv1a");
    for (iname, src) in &inputs {
        let arc: Arc<[u8]> = Arc::from(&src[..]);
        let mut times: Vec<Vec<f64>> = vec![Vec::new(); loop_names.len() + full_names.len()];
        let mut hashes: Vec<u64> = vec![0; loop_names.len() + full_names.len()];
        // Warm-up pass over every variant.
        for (k, v) in loop_names.iter().enumerate() {
            let _ = k;
            black_box(run_variant(v, src, &l));
        }
        for r in 0..rounds {
            // Rotate the starting variant each round.
            let total = loop_names.len() + full_names.len();
            for j in 0..total {
                let k = (j + r) % total;
                let t0 = Instant::now();
                if k < loop_names.len() {
                    let written = match loop_names[k] {
                        "orig" => orig_loop(black_box(&mut out), black_box(src)),
                        "lut3_round" => lut3_round(black_box(&mut out), black_box(src), &l),
                        "lut3_fc" => lut3_fc(black_box(&mut out), black_box(src), &l),
                        "arith_fc" => arith_fc(black_box(&mut out), black_box(src)),
                        "arith_fc_u32" => arith_fc_u32(black_box(&mut out), black_box(src)),
                        "sse2" => sse2(black_box(&mut out), black_box(src)),
                        _ => unreachable!(),
                    };
                    let dt = t0.elapsed().as_secs_f64();
                    black_box(written);
                    times[k].push(dt);
                    if r == 0 {
                        #[allow(unsafe_code)]
                        let bytes = unsafe { std::slice::from_raw_parts(out.as_ptr() as *const u8, written) };
                        hashes[k] = fnv1a(bytes);
                    }
                } else {
                    let (_, g) = match full_names[k - loop_names.len()] {
                        "orig_full" => orig_full(black_box(&arc), w, h),
                        "lut3_fc_full" => lut3_fc_full(black_box(&arc), w, h, &l),
                        "sse2_full" => sse2_full(black_box(&arc), w, h),
                        _ => unreachable!(),
                    };
                    let dt = t0.elapsed().as_secs_f64();
                    if r == 0 {
                        hashes[k] = fnv1a(&g);
                    }
                    drop(black_box(g));
                    times[k].push(dt);
                }
            }
        }
        for (k, ts) in times.iter_mut().enumerate() {
            let name = if k < loop_names.len() { loop_names[k] } else { full_names[k - loop_names.len()] };
            let min = ts.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = ts.iter().cloned().fold(0.0, f64::max);
            let med = median(ts);
            println!(
                "{iname}\t{name}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{}\t{:016x}",
                med * 1e9 / n as f64,
                min * 1e9 / n as f64,
                max * 1e9 / n as f64,
                med * 1e3,
                ts.len(),
                hashes[k]
            );
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("verify") => verify(),
        Some("dump-orig") => {
            let l = build_lut();
            let src = domain(0);
            let out = run_variant("orig", &src, &l);
            std::fs::write(&args[1], &out).expect("write");
            let full = sse2_full(&Arc::from(&src[..]), 4096, 4096).1;
            println!("orig fnv1a={:016x} sse2_full fnv1a={:016x}", fnv1a(&out), fnv1a(&full));
        }
        Some("bench") => {
            let w: usize = args[1].parse().unwrap();
            let h: usize = args[2].parse().unwrap();
            let rounds: usize = args[3].parse().unwrap();
            bench(w, h, rounds, &args[4..]);
        }
        Some("apply") => {
            let (w, h, rgba) = read_ppm(&args[1]);
            let arc: Arc<[u8]> = Arc::from(&rgba[..]);
            let (_, g) = orig_full(&arc, w, h);
            let mut f = Vec::new();
            f.extend_from_slice(format!("P5\n{w} {h}\n255\n").as_bytes());
            f.extend_from_slice(&g);
            std::fs::write(&args[2], &f).expect("write pgm");
        }
        _ => {
            eprintln!("usage: gray8bench verify | dump-orig FILE | bench W H ROUNDS [ppm..] | apply IN.ppm OUT.pgm");
            std::process::exit(2);
        }
    }
}
