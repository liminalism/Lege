// params: [0] row_count [1] dim [2] inner
pub(crate) const SOFTMAX_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       inp    : array<f32>;
@group(0) @binding(1) var<storage, read_write> out    : array<f32>;
@group(0) @binding(2) var<storage, read>       params : array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>,
        @builtin(num_workgroups) ng: vec3<u32>) {
    let row_index = gid.y * ng.x * 256u + gid.x;
    if row_index >= params[0] { return; }

    let dim = params[1];
    let inner = params[2];
    let outer_index = row_index / inner;
    let inner_index = row_index % inner;
    let base = outer_index * dim * inner + inner_index;

    var max_value = -3.4028234663852886e38f;
    for (var d: u32 = 0u; d < dim; d = d + 1u) {
        max_value = max(max_value, inp[base + d * inner]);
    }

    var sum = 0.0f;
    for (var d: u32 = 0u; d < dim; d = d + 1u) {
        let value = exp(inp[base + d * inner] - max_value);
        out[base + d * inner] = value;
        sum = sum + value;
    }
    for (var d: u32 = 0u; d < dim; d = d + 1u) {
        out[base + d * inner] = out[base + d * inner] / sum;
    }
}
"#;

// Wide-row softmax: one 256-thread workgroup per row with tree reductions for the
// max and the sum (the per-thread kernel above walks a row serially, which made
// attention softmax over ~2k-wide rows the single largest cost in a ViT).
// Same params as SOFTMAX_WGSL. Dispatch: linear_grid(row_count) workgroups.
pub(crate) const SOFTMAX_ROW_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       inp    : array<f32>;
@group(0) @binding(1) var<storage, read_write> out    : array<f32>;
@group(0) @binding(2) var<storage, read>       params : array<u32>;

var<workgroup> red : array<f32, 256>;

@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) wg: vec3<u32>,
        @builtin(num_workgroups) ng: vec3<u32>,
        @builtin(local_invocation_index) tid: u32) {
    let row_index = wg.y * ng.x + wg.x;
    if row_index >= params[0] { return; }
    let dim = params[1];
    let inner = params[2];
    let base = (row_index / inner) * dim * inner + (row_index % inner);

    var m = -3.4028234663852886e38f;
    for (var d = tid; d < dim; d = d + 256u) {
        m = max(m, inp[base + d * inner]);
    }
    red[tid] = m;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if tid < s { red[tid] = max(red[tid], red[tid + s]); }
        workgroupBarrier();
    }
    let row_max = red[0];
    workgroupBarrier();

    var acc = 0.0f;
    for (var d = tid; d < dim; d = d + 256u) {
        let v = exp(inp[base + d * inner] - row_max);
        out[base + d * inner] = v;
        acc = acc + v;
    }
    red[tid] = acc;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if tid < s { red[tid] = red[tid] + red[tid + s]; }
        workgroupBarrier();
    }
    let inv = 1.0 / red[0];
    for (var d = tid; d < dim; d = d + 256u) {
        out[base + d * inner] = out[base + d * inner] * inv;
    }
}
"#;

#[cfg(test)]
mod tests {
    use crate::vision::onnx::types::PlannedOpKind;
    use crate::vision::ops::test_util::{assert_gpu_matches_cpu, one_op_graph};

    #[test]
    fn softmax_matches_reference_for_narrow_wide_and_strided_rows() {
        for (shape, axis) in [
            (vec![4, 7, 5], 2usize),    // narrow rows, per-thread kernel
            (vec![2, 6, 300, 2116], 3), // attention-width rows, workgroup kernel
            (vec![3, 513, 5], 1),       // wide rows with inner stride 5
        ] {
            let g = one_op_graph(PlannedOpKind::Softmax { axis }, &shape, vec![], &shape);
            assert_gpu_matches_cpu(&g, &shape);
        }
    }
}
