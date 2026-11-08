//! Register-blocked tiled GEMM shared by MatMul (batched, broadcast) and Gemm.
//!
//! Same blocking as `conv::CONV1X1_GEMM_S1_WGSL` (64×64 block tile, BK=16,
//! 4×4 micro-tile per thread, 256 threads), generalised with explicit strides so
//! one kernel covers transposed operands, broadcast batch dims and Gemm's
//! alpha/beta/bias. The previous one-thread-per-output kernels ran
//! transformer-sized products at about 1/13 of this kernel's throughput
//! (measured on an RTX 4060 Laptop, 2116×384×1536).
//!
//! C[z, m, n] = alpha · Σ_k A[z, m, k] · B[z, k, n]  (+ beta · bias)
//!   A element (m, k) of batch z at `lhs_off(z) + m*a_rs + k*a_ks`
//!   B element (k, n) of batch z at `rhs_off(z) + k*b_ks + n*b_cs`
//!   output is contiguous `[batch..., M, N]`.
//!
//! Dispatch: `(ceil(M/64), ceil(N/64), batch)`.
//!
//! params (25 × u32):
//!   (bindings: a, b, c = bias, res = residual, out, params)
//!   [0] M  [1] N  [2] K
//!   [3] a_rs  [4] a_ks  [5] b_ks  [6] b_cs
//!   [7] batch_rank (0..=4)
//!   [8..12]  output batch dims
//!   [12..16] lhs batch strides (elements, 0 = broadcast)
//!   [16..20] rhs batch strides
//!   [20] bias mode: 0 none, 1 scalar, 2 per column (len N), 3 per row (len M), 4 full M×N
//!   [21] alpha bits  [22] beta bits
//!   [23] activation (0 none, 1 GELU, 2 ReLU), applied after the bias
//!   [24] residual: 1 adds `res` (same layout as the output) after the activation
pub(crate) const MATMUL_TILED_WGSL: &str = include_str!("matmul_tiled.wgsl");

/// vec4 variant for contiguous, 4-aligned operands (see the shader header).
pub(crate) const MATMUL_TILED_VEC4_WGSL: &str = include_str!("matmul_tiled_vec4.wgsl");

/// Whether the vec4 kernel applies: contiguous k in A and n in B, with every
/// stride, size and batch offset 4-aligned.
pub(crate) fn vec4_eligible(
    k: usize,
    n: usize,
    a_strides: (usize, usize),
    b_strides: (usize, usize),
    lhs_batch: &[u32],
    rhs_batch: &[u32],
) -> bool {
    a_strides.1 == 1
        && b_strides.1 == 1
        && k.is_multiple_of(4)
        && n.is_multiple_of(4)
        && a_strides.0.is_multiple_of(4)
        && b_strides.0.is_multiple_of(4)
        && lhs_batch
            .iter()
            .chain(rhs_batch)
            .all(|s| s.is_multiple_of(4))
}

/// Packs the params block for `MATMUL_TILED_WGSL`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tiled_params(
    m: usize,
    n: usize,
    k: usize,
    a_strides: (usize, usize),
    b_strides: (usize, usize),
    batch_dims: &[usize],
    lhs_batch: &[u32],
    rhs_batch: &[u32],
    bias_mode: u32,
    alpha: f32,
    beta: f32,
    act: u32,
    residual: bool,
) -> [u32; 25] {
    let mut p = [0u32; 25];
    p[0] = m as u32;
    p[1] = n as u32;
    p[2] = k as u32;
    p[3] = a_strides.0 as u32;
    p[4] = a_strides.1 as u32;
    p[5] = b_strides.0 as u32;
    p[6] = b_strides.1 as u32;
    p[7] = batch_dims.len() as u32;
    for (d, &size) in batch_dims.iter().enumerate() {
        p[8 + d] = size as u32;
        p[12 + d] = lhs_batch[d];
        p[16 + d] = rhs_batch[d];
    }
    p[20] = bias_mode;
    p[21] = alpha.to_bits();
    p[22] = beta.to_bits();
    p[23] = act;
    p[24] = residual as u32;
    p
}

/// The tiled kernel handles at most 4 batch dims and one dispatch row of batches.
pub(crate) fn tiled_dispatch(m: usize, n: usize, batch: usize) -> Option<[u32; 3]> {
    let gx = m.div_ceil(64);
    let gy = n.div_ceil(64);
    if gx > 65535 || gy > 65535 || batch > 65535 || batch == 0 {
        return None;
    }
    Some([gx as u32, gy as u32, batch as u32])
}

#[cfg(test)]
mod tests {
    use crate::vision::onnx::types::{Activation, PlannedOpKind};
    use crate::vision::ops::test_util::{assert_gpu_matches_cpu_f16, one_op_graph, values_f16};

    // f16-exact data throughout, so these hold to 1e-4 on the tensor-core path
    // (`WGPU_BACKEND=vulkan` on NVIDIA) as well as on the f32 kernels.
    fn mm(in_shape: &[usize], w: &[usize], out: &[usize]) {
        let g = one_op_graph(
            PlannedOpKind::MatMul,
            in_shape,
            vec![("w", w.to_vec(), values_f16(w.iter().product(), 7))],
            out,
        );
        assert_gpu_matches_cpu_f16(&g, in_shape);
    }

    #[test]
    fn fused_matmul_epilogue_matches_reference() {
        for &(m, k, n) in &[(70, 48, 132), (37, 64, 66)] {
            for act in [Activation::None, Activation::Gelu, Activation::Relu] {
                for &(bias, residual) in
                    &[(false, false), (true, false), (true, true), (false, true)]
                {
                    let mut consts = vec![("w", vec![k, n], values_f16(k * n, 7))];
                    if bias {
                        consts.push(("bias", vec![n], values_f16(n, 11)));
                    }
                    if residual {
                        consts.push(("res", vec![1, m, n], values_f16(m * n, 13)));
                    }
                    let kind = PlannedOpKind::FusedMatMul {
                        alpha: 0.5,
                        trans_b: false,
                        has_bias: bias,
                        has_residual: residual,
                        act,
                        out_shape: vec![1, m, n],
                    };
                    let g = one_op_graph(kind, &[m, k], consts, &[1, m, n]);
                    assert_gpu_matches_cpu_f16(&g, &[m, k]);
                }
            }
        }
    }

    #[test]
    fn tiled_matmul_matches_reference_for_odd_2d_shapes() {
        for &(m, k, n) in &[
            (1, 1, 1),
            (3, 5, 7),
            (65, 17, 130),
            (200, 384, 70),
            (130, 64, 196),
            (260, 384, 516),
        ] {
            mm(&[m, k], &[k, n], &[m, n]);
        }
    }

    #[test]
    fn tiled_matmul_matches_reference_with_batch_broadcast() {
        mm(&[2, 3, 70, 33], &[33, 66], &[2, 3, 70, 66]);
        mm(&[2, 3, 70, 33], &[1, 3, 33, 66], &[2, 3, 70, 66]);
        // vec4 path with a broadcast batch dim
        mm(&[2, 6, 130, 64], &[1, 6, 64, 132], &[2, 6, 130, 132]);
    }

    #[test]
    fn tiled_gemm_matches_reference_for_transposes_and_bias_shapes() {
        for &(m, k, n) in &[(67, 45, 90), (68, 48, 92)] {
            for &(ta, tb) in &[(false, false), (true, false), (false, true), (true, true)] {
                for bias in [
                    None,
                    Some(vec![n]),
                    Some(vec![m, n]),
                    Some(vec![1]),
                    Some(vec![m, 1]),
                ] {
                    let a = if ta { vec![k, m] } else { vec![m, k] };
                    let b = if tb { vec![n, k] } else { vec![k, n] };
                    let mut consts = vec![("w", b.clone(), values_f16(b.iter().product(), 7))];
                    if let Some(cs) = bias {
                        let len = cs.iter().product();
                        consts.push(("c", cs, values_f16(len, 11)));
                    }
                    let kind = PlannedOpKind::Gemm {
                        alpha: 0.75,
                        beta: 1.5,
                        trans_a: ta,
                        trans_b: tb,
                    };
                    let g = one_op_graph(kind, &a, consts, &[m, n]);
                    assert_gpu_matches_cpu_f16(&g, &a);
                }
            }
        }
    }
}
