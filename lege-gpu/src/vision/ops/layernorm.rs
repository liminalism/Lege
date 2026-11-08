//! LayerNorm over one axis (produced by `onnx::fuse_rewrite` from the nine-op
//! ReduceMean/Sub/Pow/ReduceMean/Add/Sqrt/Div/Mul/Add chain that PyTorch exports).
//!
//! One 64-thread workgroup per normalised row; two-pass mean then variance, both
//! reduced in f32 shared memory, matching `reference::layer_norm`.
//! params: [0] row_count [1] dim [2] inner [3] epsilon bits
pub(crate) const LAYERNORM_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       inp    : array<f32>;
@group(0) @binding(1) var<storage, read>       gamma  : array<f32>;
@group(0) @binding(2) var<storage, read>       beta   : array<f32>;
@group(0) @binding(3) var<storage, read_write> out    : array<f32>;
@group(0) @binding(4) var<storage, read>       params : array<u32>;

var<workgroup> red : array<f32, 64>;

fn reduce(tid: u32, v: f32) -> f32 {
    red[tid] = v;
    workgroupBarrier();
    for (var s = 32u; s > 0u; s = s >> 1u) {
        if tid < s { red[tid] = red[tid] + red[tid + s]; }
        workgroupBarrier();
    }
    let total = red[0];
    workgroupBarrier();
    return total;
}

@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wg: vec3<u32>,
        @builtin(num_workgroups) ng: vec3<u32>,
        @builtin(local_invocation_index) tid: u32) {
    let row = wg.y * ng.x + wg.x;
    if row >= params[0] { return; }
    let dim = params[1];
    let inner = params[2];
    let eps = bitcast<f32>(params[3]);
    let base = (row / inner) * dim * inner + (row % inner);
    let fdim = f32(dim);

    var s = 0.0f;
    for (var d = tid; d < dim; d = d + 64u) {
        s = s + inp[base + d * inner];
    }
    let mean = reduce(tid, s) / fdim;

    var q = 0.0f;
    for (var d = tid; d < dim; d = d + 64u) {
        let c = inp[base + d * inner] - mean;
        q = q + c * c;
    }
    let inv = 1.0 / sqrt(reduce(tid, q) / fdim + eps);

    for (var d = tid; d < dim; d = d + 64u) {
        let i = base + d * inner;
        out[i] = (inp[i] - mean) * inv * gamma[d] + beta[d];
    }
}
"#;

#[cfg(test)]
mod tests {
    use crate::vision::onnx::types::PlannedOpKind;
    use crate::vision::ops::test_util::{assert_gpu_matches_cpu, one_op_graph, values};

    fn ln(shape: &[usize], axis: usize) {
        let dim = shape[axis];
        let g = one_op_graph(
            PlannedOpKind::LayerNorm {
                axis,
                epsilon: 1e-6,
            },
            shape,
            vec![
                ("gamma", vec![dim], values(dim, 5)),
                ("beta", vec![dim], values(dim, 9)),
            ],
            shape,
        );
        assert_gpu_matches_cpu(&g, shape);
    }

    #[test]
    fn layernorm_matches_reference_on_last_and_inner_axes() {
        ln(&[1, 37, 384], 2);
        ln(&[3, 5, 70], 2);
        ln(&[1, 32, 6, 7], 1); // channel LayerNorm (NCHW)
    }
}
