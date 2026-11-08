// Scalar tiled GEMM; see matmul_tiled.rs for the params layout.
const BM: u32 = 64u;
const BN: u32 = 64u;
const BK: u32 = 16u;

var<workgroup> As: array<f32, 1024>; // As[k*BM + m]
var<workgroup> Bs: array<f32, 1024>; // Bs[k*BN + n]

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

@compute @workgroup_size(256)
fn main(
    @builtin(workgroup_id)           wg  : vec3<u32>,
    @builtin(local_invocation_index) tid : u32,
) {
    let M = params[0];
    let N = params[1];
    let K = params[2];
    let a_rs = params[3];
    let a_ks = params[4];
    let b_ks = params[5];
    let b_cs = params[6];
    let batch_rank = params[7];
    let bias_mode = params[20];
    let alpha = bitcast<f32>(params[21]);
    let beta  = bitcast<f32>(params[22]);

    // Decompose the batch index (row-major over the output batch dims).
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

    let bm = wg.x * BM;
    let bn = wg.y * BN;
    let thread_row = tid / 16u;
    let thread_col = tid % 16u;
    let inner_row_a = tid / BK;
    let inner_col_a = tid % BK;
    let inner_row_b = tid / BN;
    let inner_col_b = tid % BN;

    var acc: array<f32, 16>;
    for (var i = 0u; i < 16u; i = i + 1u) { acc[i] = 0.0; }

    var bk = 0u;
    loop {
        if bk >= K { break; }
        for (var off = 0u; off < BM; off = off + 16u) {
            let row = inner_row_a + off;
            let gm = bm + row;
            let gk = bk + inner_col_a;
            var v = 0.0;
            if gm < M && gk < K {
                v = a[lhs_off + gm * a_rs + gk * a_ks];
            }
            As[inner_col_a * BM + row] = v;
        }
        for (var off = 0u; off < BK; off = off + 4u) {
            let krow = inner_row_b + off;
            let gk = bk + krow;
            let gn = bn + inner_col_b;
            var v = 0.0;
            if gk < K && gn < N {
                v = b[rhs_off + gk * b_ks + gn * b_cs];
            }
            Bs[krow * BN + inner_col_b] = v;
        }
        workgroupBarrier();
        for (var k = 0u; k < BK; k = k + 1u) {
            let a0 = As[k * BM + thread_row * 4u + 0u];
            let a1 = As[k * BM + thread_row * 4u + 1u];
            let a2 = As[k * BM + thread_row * 4u + 2u];
            let a3 = As[k * BM + thread_row * 4u + 3u];
            let b0 = Bs[k * BN + thread_col * 4u + 0u];
            let b1 = Bs[k * BN + thread_col * 4u + 1u];
            let b2 = Bs[k * BN + thread_col * 4u + 2u];
            let b3 = Bs[k * BN + thread_col * 4u + 3u];
            acc[0u]  = acc[0u]  + a0 * b0;
            acc[1u]  = acc[1u]  + a0 * b1;
            acc[2u]  = acc[2u]  + a0 * b2;
            acc[3u]  = acc[3u]  + a0 * b3;
            acc[4u]  = acc[4u]  + a1 * b0;
            acc[5u]  = acc[5u]  + a1 * b1;
            acc[6u]  = acc[6u]  + a1 * b2;
            acc[7u]  = acc[7u]  + a1 * b3;
            acc[8u]  = acc[8u]  + a2 * b0;
            acc[9u]  = acc[9u]  + a2 * b1;
            acc[10u] = acc[10u] + a2 * b2;
            acc[11u] = acc[11u] + a2 * b3;
            acc[12u] = acc[12u] + a3 * b0;
            acc[13u] = acc[13u] + a3 * b1;
            acc[14u] = acc[14u] + a3 * b2;
            acc[15u] = acc[15u] + a3 * b3;
        }
        workgroupBarrier();
        bk = bk + BK;
    }

    for (var i = 0u; i < 4u; i = i + 1u) {
        let gm = bm + thread_row * 4u + i;
        if gm >= M { continue; }
        for (var j = 0u; j < 4u; j = j + 1u) {
            let gn = bn + thread_col * 4u + j;
            if gn >= N { continue; }
            var y = alpha * acc[i * 4u + j];
            if bias_mode == 1u {
                y = y + beta * c[0u];
            } else if bias_mode == 2u {
                y = y + beta * c[gn];
            } else if bias_mode == 3u {
                y = y + beta * c[gm];
            } else if bias_mode == 4u {
                y = y + beta * c[gm * N + gn];
            }
            y = act(params[23], y);
            if params[24] == 1u {
                y = y + res[out_off + gm * N + gn];
            }
            out[out_off + gm * N + gn] = y;
        }
    }
}
