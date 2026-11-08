//! Kernels for the detector-head ops added for DETR-family layout models:
//! Exp, Sin, Cos, ReduceMax, Tile/Expand (one index-map kernel), Where, Gather
//! (runtime data, constant indices), GatherElements and TopK.
//!
//! The executor is f32-only, so integer tensors (TopK indices, the indices fed
//! to GatherElements) travel as exactly-representable integers in f32 and
//! booleans (Where's condition) as 0.0 / 1.0. Every kernel uses the 2-D grid
//! idiom (`gid.y * num_workgroups.x * 256 + gid.x`) so large tensors never hit
//! the 65,535 workgroups-per-dimension limit.
//!
//! Algorithms follow the usual GPU formulations (as in burn/cubecl); TopK uses
//! rank selection rather than a sort: each element counts the elements that
//! beat it (ties broken by lower index, the ONNX rule) and, if its rank is
//! below k, writes itself to that slot. This yields exactly the sorted ONNX
//! output in one pass and is cheap for detector-sized rows (a few thousand).

macro_rules! unary_wgsl {
    ($expr:literal) => {
        concat!(
            r#"
@group(0) @binding(0) var<storage, read>       inp    : array<f32>;
@group(0) @binding(1) var<storage, read_write> out    : array<f32>;
@group(0) @binding(2) var<storage, read>       params : array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>,
        @builtin(num_workgroups) ng: vec3<u32>) {
    let i = gid.y * ng.x * 256u + gid.x;
    if i >= params[0] { return; }
    let x = inp[i];
    out[i] = "#,
            $expr,
            r#";
}
"#
        )
    };
}

pub(crate) const EXP_WGSL: &str = unary_wgsl!("exp(x)");
pub(crate) const SIN_WGSL: &str = unary_wgsl!("sin(x)");
pub(crate) const COS_WGSL: &str = unary_wgsl!("cos(x)");

// params: [0] num_out [1] axis_dim [2] inner
pub(crate) const REDUCEMAX_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       inp    : array<f32>;
@group(0) @binding(1) var<storage, read_write> out    : array<f32>;
@group(0) @binding(2) var<storage, read>       params : array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>,
        @builtin(num_workgroups) ng: vec3<u32>) {
    let i = gid.y * ng.x * 256u + gid.x;
    if i >= params[0] { return; }
    let axis_dim = params[1];
    let inner = params[2];
    let o = i / inner;
    let n = i % inner;
    var m = -3.4028234663852886e38f;
    for (var k = 0u; k < axis_dim; k = k + 1u) {
        m = max(m, inp[(o * axis_dim + k) * inner + n]);
    }
    out[i] = m;
}
"#;

// Wide-row ReduceSum/ReduceMean: one 256-thread workgroup per output with a tree
// reduction (the per-thread kernel walks each row serially, which made the
// decomposed LayerNorm means of a ViT cost as much as its softmax).
// Same params as sauvola::REDUCESUM_WGSL: [0] num_out [1] axis_dim [2] inner [3] mean.
// Dispatch: linear_grid(num_out) workgroups.
pub(crate) const REDUCESUM_ROW_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       inp    : array<f32>;
@group(0) @binding(1) var<storage, read_write> out    : array<f32>;
@group(0) @binding(2) var<storage, read>       params : array<u32>;

var<workgroup> red : array<f32, 256>;

@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) wg: vec3<u32>,
        @builtin(num_workgroups) ng: vec3<u32>,
        @builtin(local_invocation_index) tid: u32) {
    let t = wg.y * ng.x + wg.x;
    if t >= params[0] { return; }
    let axis_dim = params[1];
    let inner = params[2];
    let base = (t / inner) * axis_dim * inner + (t % inner);
    var acc = 0.0f;
    for (var k = tid; k < axis_dim; k = k + 256u) {
        acc = acc + inp[base + k * inner];
    }
    red[tid] = acc;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if tid < s { red[tid] = red[tid] + red[tid + s]; }
        workgroupBarrier();
    }
    if tid == 0u {
        if params[3] == 1u { out[t] = red[0] / f32(axis_dim); } else { out[t] = red[0]; }
    }
}
"#;

// Tile and Expand: out coord c_d maps to input coord c_d % in_dim_d (a size-1
// input dim broadcasts, an equal dim copies, a smaller dim tiles).
// params: [0] total [1] rank [2..8] out_strides [8..14] in_dims [14..20] in_strides
pub(crate) const INDEX_MAP_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       inp    : array<f32>;
@group(0) @binding(1) var<storage, read_write> out    : array<f32>;
@group(0) @binding(2) var<storage, read>       params : array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>,
        @builtin(num_workgroups) ng: vec3<u32>) {
    let i = gid.y * ng.x * 256u + gid.x;
    if i >= params[0] { return; }
    let rank = params[1];
    var rem = i;
    var src = 0u;
    for (var d = 0u; d < rank; d = d + 1u) {
        let c = rem / params[2u + d];
        rem = rem % params[2u + d];
        src = src + (c % params[8u + d]) * params[14u + d];
    }
    out[i] = inp[src];
}
"#;

// params: [0] total [1] rank [2..8] out_strides [8..14] cond [14..20] x [20..26] y
// (input strides are broadcast strides: 0 on broadcast dims)
pub(crate) const WHERE_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       cond   : array<f32>;
@group(0) @binding(1) var<storage, read>       xv     : array<f32>;
@group(0) @binding(2) var<storage, read>       yv     : array<f32>;
@group(0) @binding(3) var<storage, read_write> out    : array<f32>;
@group(0) @binding(4) var<storage, read>       params : array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>,
        @builtin(num_workgroups) ng: vec3<u32>) {
    let i = gid.y * ng.x * 256u + gid.x;
    if i >= params[0] { return; }
    let rank = params[1];
    var rem = i;
    var ci = 0u;
    var xi = 0u;
    var yi = 0u;
    for (var d = 0u; d < rank; d = d + 1u) {
        let c = rem / params[2u + d];
        rem = rem % params[2u + d];
        ci = ci + c * params[8u + d];
        xi = xi + c * params[14u + d];
        yi = yi + c * params[20u + d];
    }
    if cond[ci] != 0.0 { out[i] = xv[xi]; } else { out[i] = yv[yi]; }
}
"#;

// Gather along `axis` with constant, already-normalised indices carried in params.
// params: [0] total [1] axis_dim [2] inner [3] n_idx [4..4+n_idx] indices
pub(crate) const GATHER_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       inp    : array<f32>;
@group(0) @binding(1) var<storage, read_write> out    : array<f32>;
@group(0) @binding(2) var<storage, read>       params : array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>,
        @builtin(num_workgroups) ng: vec3<u32>) {
    let i = gid.y * ng.x * 256u + gid.x;
    if i >= params[0] { return; }
    let axis_dim = params[1];
    let inner = params[2];
    let n_idx = params[3];
    let n = i % inner;
    let j = (i / inner) % n_idx;
    let o = i / (inner * n_idx);
    out[i] = inp[(o * axis_dim + params[4u + j]) * inner + n];
}
"#;

// params: [0] total [1] rank [2] axis [3] data_axis_dim
//         [4..10] out (= indices) strides [10..16] data strides
pub(crate) const GATHER_ELEMENTS_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       data   : array<f32>;
@group(0) @binding(1) var<storage, read>       idx    : array<f32>;
@group(0) @binding(2) var<storage, read_write> out    : array<f32>;
@group(0) @binding(3) var<storage, read>       params : array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>,
        @builtin(num_workgroups) ng: vec3<u32>) {
    let i = gid.y * ng.x * 256u + gid.x;
    if i >= params[0] { return; }
    let rank = params[1];
    let axis = params[2];
    let axis_dim = i32(params[3]);
    var k = i32(round(idx[i]));
    if k < 0 { k = k + axis_dim; }
    k = clamp(k, 0, axis_dim - 1);
    var rem = i;
    var src = 0u;
    for (var d = 0u; d < rank; d = d + 1u) {
        var c = rem / params[4u + d];
        rem = rem % params[4u + d];
        if d == axis { c = u32(k); }
        src = src + c * params[10u + d];
    }
    out[i] = data[src];
}
"#;

// Rank-selection TopK. One thread per input element.
// params: [0] total (outer*dim*inner) [1] dim [2] inner [3] k [4] largest
//         [5] write_indices (0 = values output, 1 = indices output)
pub(crate) const TOPK_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       inp    : array<f32>;
@group(0) @binding(1) var<storage, read_write> out    : array<f32>;
@group(0) @binding(2) var<storage, read>       params : array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>,
        @builtin(num_workgroups) ng: vec3<u32>) {
    let i = gid.y * ng.x * 256u + gid.x;
    if i >= params[0] { return; }
    let dim = params[1];
    let inner = params[2];
    let k = params[3];
    let largest = params[4] != 0u;
    let n = i % inner;
    let d = (i / inner) % dim;
    let o = i / (inner * dim);
    let base = o * dim * inner + n;
    let x = inp[base + d * inner];
    var rank = 0u;
    for (var j = 0u; j < dim; j = j + 1u) {
        let y = inp[base + j * inner];
        var beats = false;
        if largest { beats = y > x; } else { beats = y < x; }
        if beats || (y == x && j < d) { rank = rank + 1u; }
    }
    if rank < k {
        let dst = (o * k + rank) * inner + n;
        if params[5] != 0u { out[dst] = f32(d); } else { out[dst] = x; }
    }
}
"#;

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use crate::vision::onnx::graph::PreparedGraph;
    use crate::vision::onnx::types::{PlannedOp, PlannedOpKind, UnaryKind};
    use crate::vision::ops::test_util::{assert_gpu_matches_cpu, one_op_graph, values};
    use crate::vision::reference::Tensor;
    use crate::vision::runtime::compiled::CompiledGraph;

    #[test]
    fn exp_sin_cos_match_reference() {
        for kind in [UnaryKind::Exp, UnaryKind::Sin, UnaryKind::Cos] {
            let g = one_op_graph(
                PlannedOpKind::Unary(kind),
                &[3, 300, 64],
                vec![],
                &[3, 300, 64],
            );
            assert_gpu_matches_cpu(&g, &[3, 300, 64]);
        }
    }

    #[test]
    fn wide_reduce_sum_and_mean_match_reference() {
        for (shape, axis, mean) in [
            (vec![1usize, 1600, 384], 2usize, true),
            (vec![3, 300, 7], 1, false),
        ] {
            let mut out = shape.clone();
            out[axis] = 1;
            let g = one_op_graph(
                PlannedOpKind::ReduceSum {
                    axis,
                    keepdims: true,
                    mean,
                },
                &shape,
                vec![],
                &out,
            );
            assert_gpu_matches_cpu(&g, &shape);
        }
    }

    #[test]
    fn reduce_max_matches_reference_on_each_axis() {
        for (axis, out) in [(0usize, vec![1, 1024, 91]), (2, vec![2, 1024])] {
            let keepdims = axis == 0;
            let g = one_op_graph(
                PlannedOpKind::ReduceMax { axis, keepdims },
                &[2, 1024, 91],
                vec![],
                &out,
            );
            assert_gpu_matches_cpu(&g, &[2, 1024, 91]);
        }
    }

    #[test]
    fn tile_and_expand_match_reference() {
        let tile = PlannedOpKind::Tile {
            out_shape: vec![1, 300, 1, 2, 4],
        };
        assert_gpu_matches_cpu(
            &one_op_graph(tile, &[1, 300, 1, 1, 4], vec![], &[1, 300, 1, 2, 4]),
            &[1, 300, 1, 1, 4],
        );
        let tile = PlannedOpKind::Tile {
            out_shape: vec![4, 3, 384],
        };
        assert_gpu_matches_cpu(
            &one_op_graph(tile, &[1, 3, 384], vec![], &[4, 3, 384]),
            &[1, 3, 384],
        );
        let expand = PlannedOpKind::Expand {
            out_shape: vec![1, 300, 256],
        };
        assert_gpu_matches_cpu(
            &one_op_graph(expand, &[1, 300, 1], vec![], &[1, 300, 256]),
            &[1, 300, 1],
        );
        // lower-rank input right-aligns
        let expand = PlannedOpKind::Expand {
            out_shape: vec![2, 5, 7],
        };
        assert_gpu_matches_cpu(&one_op_graph(expand, &[7], vec![], &[2, 5, 7]), &[7]);
    }

    #[test]
    fn where_matches_reference_with_broadcast_condition_and_scalar() {
        // input is `y` here: Where(cond, x, y) with cond [1,64,1], x scalar.
        let cond: Vec<f32> = (0..64)
            .map(|i| if i % 3 == 0 { 1.0 } else { 0.0 })
            .collect();
        let mut g = one_op_graph(
            PlannedOpKind::Where,
            &[1, 64, 32],
            vec![("cond", vec![1, 64, 1], cond), ("x", vec![], vec![-7.5])],
            &[1, 64, 32],
        );
        // reorder to (cond, x, y) as ONNX expects
        let op = &mut g.planned_ops[0];
        op.inputs = vec!["cond".into(), "x".into(), "input".into()];
        op.input_shapes = vec![vec![1, 64, 1], vec![], vec![1, 64, 32]];
        assert_gpu_matches_cpu(&g, &[1, 64, 32]);
    }

    #[test]
    fn gather_with_scalar_and_vector_indices_matches_reference() {
        let g = one_op_graph(
            PlannedOpKind::Gather {
                axis: 2,
                indices: vec![3],
                out_shape: vec![1, 300],
            },
            &[1, 300, 4],
            vec![],
            &[1, 300],
        );
        assert_gpu_matches_cpu(&g, &[1, 300, 4]);
        let g = one_op_graph(
            PlannedOpKind::Gather {
                axis: 1,
                indices: vec![4, 0, 4, 2],
                out_shape: vec![3, 4, 6],
            },
            &[3, 5, 6],
            vec![],
            &[3, 4, 6],
        );
        assert_gpu_matches_cpu(&g, &[3, 5, 6]);
    }

    #[test]
    fn gather_elements_matches_reference_including_negative_indices() {
        let idx: Vec<f32> = (0..300 * 8)
            .map(|i| ((i * 37) % 1024) as f32 - if i % 5 == 0 { 1024.0 } else { 0.0 })
            .collect();
        let g = one_op_graph(
            PlannedOpKind::GatherElements { axis: 1 },
            &[1, 1024, 8],
            vec![("idx", vec![1, 300, 8], idx)],
            &[1, 300, 8],
        );
        assert_gpu_matches_cpu(&g, &[1, 1024, 8]);
    }

    fn topk_graph(shape: &[usize], axis: usize, k: usize, largest: bool) -> PreparedGraph {
        let i64s = |s: &[usize]| s.iter().map(|&d| d as i64).collect::<Vec<_>>();
        let mut out = shape.to_vec();
        out[axis] = k;
        PreparedGraph {
            inputs: vec!["input".into()],
            outputs: vec!["values".into(), "indices".into()],
            known_shapes: BTreeMap::from([
                ("input".into(), i64s(shape)),
                ("values".into(), i64s(&out)),
                ("indices".into(), i64s(&out)),
            ]),
            planned_ops: vec![PlannedOp {
                name: "topk".into(),
                inputs: vec!["input".into()],
                outputs: vec!["values".into(), "indices".into()],
                input_shapes: vec![i64s(shape)],
                output_shapes: vec![i64s(&out), i64s(&out)],
                kind: PlannedOpKind::TopK { axis, k, largest },
            }],
            constants: HashMap::new(),
        }
    }

    fn run_both(
        g: &PreparedGraph,
        input: &Tensor,
    ) -> (HashMap<String, Tensor>, HashMap<String, Tensor>) {
        let cpu = g.run_cpu(input).expect("cpu");
        let compiled = pollster::block_on(CompiledGraph::build(g)).expect("compile");
        let gpu = pollster::block_on(compiled.run(input)).expect("gpu");
        (cpu, gpu)
    }

    #[test]
    fn topk_hand_checked_with_ties() {
        let g = topk_graph(&[1, 6], 1, 4, true);
        let input = Tensor::new(vec![1, 6], vec![0.5, 2.0, -1.0, 2.0, 0.5, 9.0]).unwrap();
        let (cpu, gpu) = run_both(&g, &input);
        for out in [&cpu, &gpu] {
            assert_eq!(out["values"].data, vec![9.0, 2.0, 2.0, 0.5]);
            // ties keep the lower index first
            assert_eq!(out["indices"].data, vec![5.0, 1.0, 3.0, 0.0]);
        }
        let g = topk_graph(&[1, 6], 1, 2, false);
        let (cpu, gpu) = run_both(&g, &input);
        for out in [&cpu, &gpu] {
            assert_eq!(out["values"].data, vec![-1.0, 0.5]);
            assert_eq!(out["indices"].data, vec![2.0, 0.0]);
        }
    }

    #[test]
    fn topk_matches_reference_on_detector_shapes_and_inner_axis() {
        for (shape, axis, k) in [
            (vec![1usize, 1024], 1usize, 300usize),
            (vec![2, 37, 5], 1, 10),
        ] {
            let g = topk_graph(&shape, axis, k, true);
            let input = Tensor::new(shape.clone(), values(shape.iter().product(), 5)).unwrap();
            let (cpu, gpu) = run_both(&g, &input);
            assert_eq!(cpu["values"].data, gpu["values"].data);
            assert_eq!(cpu["indices"].data, gpu["indices"].data);
        }
    }
}
