// vec4 variant of MATMUL_TILED_WGSL for the common contiguous layout: A row-major
// with k contiguous (a_ks == 1), B row-major with n contiguous (b_cs == 1), and K,
// N, a_rs, b_ks and every batch offset multiples of 4 (see `vec4_eligible`).
// 128-bit loads and a vec4 B tile, as in conv::CONV1X1_GEMM_S1_VEC4_WGSL.
// Same params and dispatch as the scalar kernel.
const BM: u32 = 64u;
const BN: u32 = 64u;
const BK: u32 = 16u;

var<workgroup> As: array<f32, 1024>;        // As[k*BM + m]
var<workgroup> Bs: array<vec4<f32>, 256>;   // Bs[k*16 + n4]

@group(0) @binding(0) var<storage, read>       a      : array<vec4<f32>>;
@group(0) @binding(1) var<storage, read>       b      : array<vec4<f32>>;
@group(0) @binding(2) var<storage, read>       c      : array<f32>;
@group(0) @binding(3) var<storage, read>       res    : array<f32>;
@group(0) @binding(4) var<storage, read_write> out    : array<vec4<f32>>;
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

@compute @workgroup_size(256)
fn main(
    @builtin(workgroup_id)           wg  : vec3<u32>,
    @builtin(local_invocation_index) tid : u32,
) {
    let M = params[0];
    let N = params[1];
    let K = params[2];
    let a_rs4 = params[3] / 4u;
    let b_ks4 = params[5] / 4u;
    let batch_rank = params[7];
    let bias_mode = params[20];
    let alpha = bitcast<f32>(params[21]);
    let beta  = bitcast<f32>(params[22]);
    let K4 = K / 4u;
    let N4 = N / 4u;

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
    let lhs4 = lhs_off / 4u;
    let rhs4 = rhs_off / 4u;
    let out4 = z * M * N4;

    let bm  = wg.x * BM;
    let bn  = wg.y * BN;
    let bn4 = bn / 4u;
    let thread_row = tid / 16u;
    let thread_col = tid % 16u;
    let a_m  = tid / 4u;
    let a_k4 = tid % 4u;
    let b_k  = tid / 16u;
    let b_n4 = tid % 16u;

    var acc: array<f32, 16>;
    for (var i = 0u; i < 16u; i = i + 1u) { acc[i] = 0.0; }

    var bk = 0u;
    loop {
        if bk >= K { break; }
        let bk4 = bk / 4u;
        let gm_a = bm + a_m;
        let gk4_a = bk4 + a_k4;
        var av = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        if gm_a < M && gk4_a < K4 {
            av = a[lhs4 + gm_a * a_rs4 + gk4_a];
        }
        let kbase = a_k4 * 4u;
        As[(kbase + 0u) * BM + a_m] = av.x;
        As[(kbase + 1u) * BM + a_m] = av.y;
        As[(kbase + 2u) * BM + a_m] = av.z;
        As[(kbase + 3u) * BM + a_m] = av.w;

        let gk_b = bk + b_k;
        let gn4_b = bn4 + b_n4;
        var bv = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        if gk_b < K && gn4_b < N4 {
            bv = b[rhs4 + gk_b * b_ks4 + gn4_b];
        }
        Bs[b_k * 16u + b_n4] = bv;
        workgroupBarrier();

        for (var k = 0u; k < BK; k = k + 1u) {
            let a0 = As[k * BM + thread_row * 4u + 0u];
            let a1 = As[k * BM + thread_row * 4u + 1u];
            let a2 = As[k * BM + thread_row * 4u + 2u];
            let a3 = As[k * BM + thread_row * 4u + 3u];
            let bb = Bs[k * 16u + thread_col];
            acc[0u]  = acc[0u]  + a0 * bb.x;
            acc[1u]  = acc[1u]  + a0 * bb.y;
            acc[2u]  = acc[2u]  + a0 * bb.z;
            acc[3u]  = acc[3u]  + a0 * bb.w;
            acc[4u]  = acc[4u]  + a1 * bb.x;
            acc[5u]  = acc[5u]  + a1 * bb.y;
            acc[6u]  = acc[6u]  + a1 * bb.z;
            acc[7u]  = acc[7u]  + a1 * bb.w;
            acc[8u]  = acc[8u]  + a2 * bb.x;
            acc[9u]  = acc[9u]  + a2 * bb.y;
            acc[10u] = acc[10u] + a2 * bb.z;
            acc[11u] = acc[11u] + a2 * bb.w;
            acc[12u] = acc[12u] + a3 * bb.x;
            acc[13u] = acc[13u] + a3 * bb.y;
            acc[14u] = acc[14u] + a3 * bb.z;
            acc[15u] = acc[15u] + a3 * bb.w;
        }
        workgroupBarrier();
        bk = bk + BK;
    }

    let n4 = bn4 + thread_col;
    if n4 >= N4 { return; }
    let gn = n4 * 4u;
    for (var i = 0u; i < 4u; i = i + 1u) {
        let gm = bm + thread_row * 4u + i;
        if gm >= M { continue; }
        var r = alpha * vec4<f32>(acc[i * 4u + 0u], acc[i * 4u + 1u], acc[i * 4u + 2u], acc[i * 4u + 3u]);
        if bias_mode == 1u {
            r = r + beta * vec4<f32>(c[0u], c[0u], c[0u], c[0u]);
        } else if bias_mode == 2u {
            r = r + beta * vec4<f32>(c[gn], c[gn + 1u], c[gn + 2u], c[gn + 3u]);
        } else if bias_mode == 3u {
            r = r + beta * vec4<f32>(c[gm], c[gm], c[gm], c[gm]);
        } else if bias_mode == 4u {
            let o = gm * N + gn;
            r = r + beta * vec4<f32>(c[o], c[o + 1u], c[o + 2u], c[o + 3u]);
        }
        let code = params[23];
        if code != 0u {
            r = vec4<f32>(act(code, r.x), act(code, r.y), act(code, r.z), act(code, r.w));
        }
        if params[24] == 1u {
            let ro = (out4 + gm * N4 + n4) * 4u;
            r = r + vec4<f32>(res[ro], res[ro + 1u], res[ro + 2u], res[ro + 3u]);
        }
        out[out4 + gm * N4 + n4] = r;
    }
}
