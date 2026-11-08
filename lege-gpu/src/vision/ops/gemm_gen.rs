//! WGSL generators for the tiled GEMM family (MatMul, Gemm, FusedMatMul).
//!
//! Two kernel kinds share one binding layout and one params block with
//! `matmul_tiled::MATMUL_TILED_WGSL` (a, b, c = bias, res = residual, out,
//! params; see that file for the 25-word params layout), so a step can switch
//! kernels without touching its buffers:
//!
//! * [`simt_wgsl`]: f32 register-tiled kernel. Each thread owns `RM×RN` groups
//!   of 4×4 outputs, spread `4·TR` rows / `4·TC` columns apart so shared-memory
//!   vec4 reads are conflict-free. The code is emitted fully unrolled: every
//!   accumulator is a named `vec4<f32>`, never a dynamically indexed array,
//!   which shader compilers would otherwise spill to local memory.
//! * [`coop_wgsl`]: tensor-core kernel (`wgpu_cooperative_matrix`, Vulkan): f32
//!   operands are rounded to f16 while staging into shared memory, multiplied
//!   as 16×16×16 tiles, and accumulated in f32. Needs subgroups of 32 lanes.
//!
//! Self-contained (no crate paths) so `bin/bench_gemm.rs` can include it.

use std::fmt::Write;

/// f32 SIMT tile configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SimtCfg {
    pub bm: u32,
    pub bn: u32,
    pub bk: u32,
    /// Thread rows and columns; the workgroup has `tr·tc` threads.
    pub tr: u32,
    pub tc: u32,
}

impl SimtCfg {
    pub fn threads(&self) -> u32 {
        self.tr * self.tc
    }
    fn rm(&self) -> u32 {
        self.bm / (4 * self.tr)
    }
    fn rn(&self) -> u32 {
        self.bn / (4 * self.tc)
    }
    pub fn valid(&self) -> bool {
        let t = self.threads();
        self.bm.is_multiple_of(4 * self.tr)
            && self.bn.is_multiple_of(4 * self.tc)
            && self.rm() >= 1
            && self.rn() >= 1
            && (self.bm * self.bk / 4).is_multiple_of(t)
            && (self.bk * self.bn / 4).is_multiple_of(t)
            && t <= 256
    }
    #[allow(dead_code)]
    pub fn label(&self) -> String {
        format!(
            "simt {}x{}x{} t{}x{} ({}x{}/thr)",
            self.bm,
            self.bn,
            self.bk,
            self.tr,
            self.tc,
            4 * self.rm(),
            4 * self.rn()
        )
    }
}

/// Tensor-core tile configuration: `wm×wn` subgroups of 32 lanes, each owning
/// `fm×fn_` 16×16 accumulator tiles; the K step is `bk` (a multiple of 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CoopCfg {
    pub wm: u32,
    pub wn: u32,
    pub fm: u32,
    pub fn_: u32,
    pub bk: u32,
}

impl CoopCfg {
    pub fn bm(&self) -> u32 {
        self.wm * self.fm * 16
    }
    pub fn bn(&self) -> u32 {
        self.wn * self.fn_ * 16
    }
    pub fn threads(&self) -> u32 {
        self.wm * self.wn * 32
    }
    pub fn valid(&self) -> bool {
        let t = self.threads();
        self.bk.is_multiple_of(16)
            && t <= 256
            && (self.bm() * self.bk / 4).is_multiple_of(t)
            && (self.bk * self.bn() / 4).is_multiple_of(t)
    }
    #[allow(dead_code)]
    pub fn label(&self) -> String {
        format!(
            "coop {}x{}x{} w{}x{} ({}x{} tiles/warp)",
            self.bm(),
            self.bn(),
            self.bk,
            self.wm,
            self.wn,
            self.fm,
            self.fn_
        )
    }
}

const BINDINGS: &str = r#"
@group(0) @binding(0) var<storage, read>       a      : array<f32>;
@group(0) @binding(1) var<storage, read>       b      : array<f32>;
@group(0) @binding(2) var<storage, read>       c      : array<f32>;
@group(0) @binding(3) var<storage, read>       res    : array<f32>;
@group(0) @binding(4) var<storage, read_write> out    : array<f32>;
@group(0) @binding(5) var<storage, read>       params : array<u32>;

// Epilogue activation; `reference::activation` mirrors it term for term
// (erf is Abramowitz & Stegun 7.1.26, as ERF_WGSL).
fn erf_as(x: f32) -> f32 {
    let t = 1.0 / (1.0 + 0.3275911 * abs(x));
    let poly = t * (0.254829592 + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
    let m = 1.0 - poly * exp(-x * x);
    return select(-m, m, x >= 0.0);
}
fn act(code: u32, y: f32) -> f32 {
    if code == 1u { return 0.5 * y * (1.0 + erf_as(y * 0.70710678118654752)); }
    if code == 2u { return max(y, 0.0); }
    return y;
}

// One output element through the epilogue: alpha, bias, activation, residual.
fn emit(out_off: u32, row: u32, col: u32, v: f32) {
    let N = params[1];
    let bias_mode = params[20];
    let beta = bitcast<f32>(params[22]);
    var y = bitcast<f32>(params[21]) * v;
    if bias_mode == 1u {
        y = y + beta * c[0u];
    } else if bias_mode == 2u {
        y = y + beta * c[col];
    } else if bias_mode == 3u {
        y = y + beta * c[row];
    } else if bias_mode == 4u {
        y = y + beta * c[row * N + col];
    }
    y = act(params[23], y);
    let o = out_off + row * N + col;
    if params[24] == 1u {
        y = y + res[o];
    }
    out[o] = y;
}
"#;

/// Shared prologue: params, batch offsets (as the original tiled kernel).
const PROLOGUE: &str = r#"
    let M = params[0];
    let N = params[1];
    let K = params[2];
    let a_rs = params[3];
    let a_ks = params[4];
    let b_ks = params[5];
    let b_cs = params[6];
    let batch_rank = params[7];
    let z = wg.z;
    var rem = z;
    var lhs_off = 0u;
    var rhs_off = 0u;
    for (var d = 0u; d < batch_rank; d = d + 1u) {
        let dim = batch_rank - 1u - d;
        let size = params[8u + dim];
        let coord = rem % size;
        rem = rem / size;
        lhs_off = lhs_off + coord * params[12u + dim];
        rhs_off = rhs_off + coord * params[16u + dim];
    }
    let out_off = z * M * N;
"#;

pub fn simt_wgsl(cfg: &SimtCfg) -> String {
    assert!(cfg.valid(), "invalid SIMT config {cfg:?}");
    let (bm, bn, bk, tr, tc) = (cfg.bm, cfg.bn, cfg.bk, cfg.tr, cfg.tc);
    let nt = cfg.threads();
    let (rm, rn) = (cfg.rm(), cfg.rn());
    let mut s = String::new();
    let _ = writeln!(
        s,
        "const BM: u32 = {bm}u;\nconst BN: u32 = {bn}u;\nconst BK: u32 = {bk}u;\n\
         const TR: u32 = {tr}u;\nconst TC: u32 = {tc}u;\nconst NT: u32 = {nt}u;\n\
         var<workgroup> As: array<vec4<f32>, {}>; // As[k*(BM/4) + m/4]\n\
         var<workgroup> Bs: array<vec4<f32>, {}>; // Bs[k*(BN/4) + n/4]",
        bk * bm / 4,
        bk * bn / 4
    );
    s.push_str(BINDINGS);
    s.push_str(
        "\n@compute @workgroup_size(NT)\nfn main(\n    @builtin(workgroup_id) wg : vec3<u32>,\n    @builtin(local_invocation_index) tid : u32,\n) {",
    );
    s.push_str(PROLOGUE);
    s.push_str(
        "    let bm0 = wg.x * BM;\n    let bn0 = wg.y * BN;\n    let tr = tid / TC;\n    let tc = tid % TC;\n",
    );
    for g in 0..rm {
        for e in 0..4 {
            for h in 0..rn {
                let _ = writeln!(s, "    var acc_{g}_{e}_{h} = vec4<f32>(0.0);");
            }
        }
    }
    let _ = write!(
        s,
        r#"
    for (var k0 = 0u; k0 < K; k0 = k0 + BK) {{
        for (var i = 0u; i < {la}u; i = i + 1u) {{
            let idx = tid + i * NT;
            let kk = idx % BK;
            let m4 = idx / BK;
            let gm = bm0 + m4 * 4u;
            let gk = k0 + kk;
            var v = vec4<f32>(0.0);
            if gk < K {{
                let base = lhs_off + gm * a_rs + gk * a_ks;
                if gm + 3u < M {{
                    v = vec4<f32>(a[base], a[base + a_rs], a[base + 2u * a_rs], a[base + 3u * a_rs]);
                }} else {{
                    if gm < M {{ v.x = a[base]; }}
                    if gm + 1u < M {{ v.y = a[base + a_rs]; }}
                    if gm + 2u < M {{ v.z = a[base + 2u * a_rs]; }}
                }}
            }}
            As[kk * (BM / 4u) + m4] = v;
        }}
        for (var i = 0u; i < {lb}u; i = i + 1u) {{
            let idx = tid + i * NT;
            let n4 = idx % (BN / 4u);
            let kk = idx / (BN / 4u);
            let gn = bn0 + n4 * 4u;
            let gk = k0 + kk;
            var v = vec4<f32>(0.0);
            if gk < K {{
                let base = rhs_off + gk * b_ks + gn * b_cs;
                if gn + 3u < N {{
                    v = vec4<f32>(b[base], b[base + b_cs], b[base + 2u * b_cs], b[base + 3u * b_cs]);
                }} else {{
                    if gn < N {{ v.x = b[base]; }}
                    if gn + 1u < N {{ v.y = b[base + b_cs]; }}
                    if gn + 2u < N {{ v.z = b[base + 2u * b_cs]; }}
                }}
            }}
            Bs[kk * (BN / 4u) + n4] = v;
        }}
        workgroupBarrier();
        for (var k = 0u; k < BK; k = k + 1u) {{
"#,
        la = bm * bk / 4 / nt,
        lb = bk * bn / 4 / nt
    );
    for g in 0..rm {
        let _ = writeln!(
            s,
            "            let a{g} = As[k * (BM / 4u) + {}u + tr];",
            g * tr
        );
    }
    for h in 0..rn {
        let _ = writeln!(
            s,
            "            let b{h} = Bs[k * (BN / 4u) + {}u + tc];",
            h * tc
        );
    }
    let comp = ["x", "y", "z", "w"];
    for g in 0..rm {
        for (e, ce) in comp.iter().enumerate() {
            for h in 0..rn {
                let _ = writeln!(
                    s,
                    "            acc_{g}_{e}_{h} = fma(vec4<f32>(a{g}.{ce}), b{h}, acc_{g}_{e}_{h});"
                );
            }
        }
    }
    s.push_str("        }\n        workgroupBarrier();\n    }\n");
    for g in 0..rm {
        for e in 0..4 {
            let _ = writeln!(
                s,
                "    {{\n        let row = bm0 + {}u + tr * 4u;\n        if row < M {{",
                g * 4 * tr + e
            );
            for h in 0..rn {
                let _ = writeln!(
                    s,
                    "            let col{h} = bn0 + {}u + tc * 4u;",
                    h * 4 * tc
                );
                for (j, cj) in comp.iter().enumerate() {
                    let _ = writeln!(
                        s,
                        "            if col{h} + {j}u < N {{ emit(out_off, row, col{h} + {j}u, acc_{g}_{e}_{h}.{cj}); }}"
                    );
                }
            }
            s.push_str("        }\n    }\n");
        }
    }
    s.push_str("}\n");
    s
}

pub fn coop_wgsl(cfg: &CoopCfg) -> String {
    assert!(cfg.valid(), "invalid coop config {cfg:?}");
    let (bm, bn, bk) = (cfg.bm(), cfg.bn(), cfg.bk);
    let nt = cfg.threads();
    let warps = cfg.wm * cfg.wn;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "enable f16;\nenable wgpu_cooperative_matrix;\n\
         const BM: u32 = {bm}u;\nconst BN: u32 = {bn}u;\nconst BK: u32 = {bk}u;\n\
         const WN: u32 = {wn}u;\nconst NT: u32 = {nt}u;\n\
         const SA: u32 = {sa}u; // A row stride (BK + 8: rows 16 B apart in banks)\n\
         const SB: u32 = {sb}u; // B row stride (BN + 8)\n\
         var<workgroup> As: array<f16, {}>; // row-major [BM][SA]\n\
         var<workgroup> Bs: array<f16, {}>; // row-major [BK][SB]\n\
         var<workgroup> Cs: array<f32, {}>; // one 16x16 staging tile per subgroup",
        bm * (bk + 8),
        bk * (bn + 8),
        warps * 256,
        wn = cfg.wn,
        sa = bk + 8,
        sb = bn + 8
    );
    s.push_str(BINDINGS);
    s.push_str(
        "\n@compute @workgroup_size(NT)\nfn main(\n    @builtin(workgroup_id) wg : vec3<u32>,\n    @builtin(local_invocation_index) tid : u32,\n) {",
    );
    s.push_str(PROLOGUE);
    let _ = write!(
        s,
        r#"    let bm0 = wg.x * BM;
    let bn0 = wg.y * BN;
    let warp = tid / 32u;
    let lane = tid % 32u;
    let wm0 = (warp / WN) * {wtm}u;
    let wn0 = (warp % WN) * {wtn}u;
"#,
        wtm = cfg.fm * 16,
        wtn = cfg.fn_ * 16
    );
    for i in 0..cfg.fm {
        for j in 0..cfg.fn_ {
            let _ = writeln!(s, "    var acc_{i}_{j}: coop_mat16x16<f32, C>;");
        }
    }
    let _ = write!(
        s,
        r#"
    for (var k0 = 0u; k0 < K; k0 = k0 + BK) {{
        for (var i = 0u; i < {la}u; i = i + 1u) {{
            let idx = tid + i * NT;
            let k4 = idx % (BK / 4u);
            let mm = idx / (BK / 4u);
            let gm = bm0 + mm;
            let gk = k0 + k4 * 4u;
            var v = vec4<f32>(0.0);
            if gm < M {{
                let base = lhs_off + gm * a_rs + gk * a_ks;
                if gk + 3u < K {{
                    v = vec4<f32>(a[base], a[base + a_ks], a[base + 2u * a_ks], a[base + 3u * a_ks]);
                }} else {{
                    if gk < K {{ v.x = a[base]; }}
                    if gk + 1u < K {{ v.y = a[base + a_ks]; }}
                    if gk + 2u < K {{ v.z = a[base + 2u * a_ks]; }}
                }}
            }}
            let h = vec4<f16>(v);
            let o = mm * SA + k4 * 4u;
            As[o] = h.x;
            As[o + 1u] = h.y;
            As[o + 2u] = h.z;
            As[o + 3u] = h.w;
        }}
        for (var i = 0u; i < {lb}u; i = i + 1u) {{
            let idx = tid + i * NT;
            let n4 = idx % (BN / 4u);
            let kk = idx / (BN / 4u);
            let gn = bn0 + n4 * 4u;
            let gk = k0 + kk;
            var v = vec4<f32>(0.0);
            if gk < K {{
                let base = rhs_off + gk * b_ks + gn * b_cs;
                if gn + 3u < N {{
                    v = vec4<f32>(b[base], b[base + b_cs], b[base + 2u * b_cs], b[base + 3u * b_cs]);
                }} else {{
                    if gn < N {{ v.x = b[base]; }}
                    if gn + 1u < N {{ v.y = b[base + b_cs]; }}
                    if gn + 2u < N {{ v.z = b[base + 2u * b_cs]; }}
                }}
            }}
            let h = vec4<f16>(v);
            let o = kk * SB + n4 * 4u;
            Bs[o] = h.x;
            Bs[o + 1u] = h.y;
            Bs[o + 2u] = h.z;
            Bs[o + 3u] = h.w;
        }}
        workgroupBarrier();
        for (var kk = 0u; kk < BK; kk = kk + 16u) {{
"#,
        la = bm * bk / 4 / nt,
        lb = bk * bn / 4 / nt
    );
    for i in 0..cfg.fm {
        let _ = writeln!(
            s,
            "            let ta{i} = coopLoadT<coop_mat16x16<f16, A>>(&As[(wm0 + {}u) * SA + kk], SA);",
            i * 16
        );
    }
    for j in 0..cfg.fn_ {
        let _ = writeln!(
            s,
            "            let tb{j} = coopLoadT<coop_mat16x16<f16, B>>(&Bs[kk * SB + wn0 + {}u], SB);",
            j * 16
        );
    }
    for i in 0..cfg.fm {
        for j in 0..cfg.fn_ {
            let _ = writeln!(
                s,
                "            acc_{i}_{j} = coopMultiplyAdd(ta{i}, tb{j}, acc_{i}_{j});"
            );
        }
    }
    s.push_str("        }\n        workgroupBarrier();\n    }\n    let stage = warp * 256u;\n");
    for i in 0..cfg.fm {
        for j in 0..cfg.fn_ {
            let _ = write!(
                s,
                r#"    coopStoreT(acc_{i}_{j}, &Cs[stage], 16u);
    workgroupBarrier();
    for (var e = lane; e < 256u; e = e + 32u) {{
        let row = bm0 + wm0 + {ro}u + e / 16u;
        let col = bn0 + wn0 + {co}u + e % 16u;
        if row < M && col < N {{
            emit(out_off, row, col, Cs[stage + e]);
        }}
    }}
    workgroupBarrier();
"#,
                ro = i * 16,
                co = j * 16
            );
        }
    }
    s.push_str("}\n");
    s
}

/// The f32 kernel lege-gpu dispatches when tensor cores are unavailable:
/// 64x64x16 tiles, 256 threads with 4x4 outputs each (the `matmul_tiled`
/// blocking with vectorised staging and shared-memory reads).
pub(crate) fn simt_gemm_wgsl() -> &'static str {
    static WGSL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    WGSL.get_or_init(|| {
        simt_wgsl(&SimtCfg {
            bm: 64,
            bn: 64,
            bk: 16,
            tr: 16,
            tc: 16,
        })
    })
}

/// The tensor-core kernel lege-gpu dispatches: 64x64 output tiles (the same
/// grid as `matmul_tiled`), 2x2 subgroups each owning 2x2 accumulator tiles,
/// K step 32. Chosen with `bin/bench_gemm.rs` on an RTX 4060 Laptop (Vulkan).
#[allow(dead_code)]
pub(crate) fn coop_gemm_wgsl() -> &'static str {
    static WGSL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    WGSL.get_or_init(|| {
        coop_wgsl(&CoopCfg {
            wm: 2,
            wn: 2,
            fm: 2,
            fn_: 2,
            bk: 32,
        })
    })
}
