//! Shared GPU-versus-CPU-reference harness for op kernel tests.
#![cfg(test)]

use std::collections::{BTreeMap, HashMap};

use crate::vision::onnx::graph::PreparedGraph;
use crate::vision::onnx::types::{PlannedOp, PlannedOpKind};
use crate::vision::reference::Tensor;
use crate::vision::runtime::compiled::CompiledGraph;

pub(crate) fn values(n: usize, seed: u32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(2654435761).wrapping_add(12345);
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            ((s >> 8) as f32 / (1u32 << 24) as f32) - 0.5
        })
        .collect()
}

/// `values` rounded to multiples of 2^-10: exact in f16, so a GEMM on the
/// tensor-core path (f16 operands, f32 accumulation) matches the f32 reference
/// up to summation order.
pub(crate) fn values_f16(n: usize, seed: u32) -> Vec<f32> {
    values(n, seed)
        .into_iter()
        .map(|v| (v * 1024.0).round() / 1024.0)
        .collect()
}

/// Whether the shared test device dispatches the tensor-core GEMM
/// (Vulkan with cooperative matrices, e.g. `WGPU_BACKEND=vulkan` on NVIDIA).
pub(crate) fn coop_gemm_active() -> bool {
    pollster::block_on(crate::vision::runtime::device::GpuContext::shared())
        .map(|ctx| ctx.supports_coop_gemm)
        .unwrap_or(false)
}

/// One-op graph: `input` is the runtime tensor; `consts` are extra constant
/// inputs (name, shape, data) appended in order.
pub(crate) fn one_op_graph(
    kind: PlannedOpKind,
    in_shape: &[usize],
    consts: Vec<(&str, Vec<usize>, Vec<f32>)>,
    out_shape: &[usize],
) -> PreparedGraph {
    let i64s = |s: &[usize]| s.iter().map(|&d| d as i64).collect::<Vec<_>>();
    let mut inputs = vec!["input".to_owned()];
    let mut input_shapes = vec![i64s(in_shape)];
    let mut known = BTreeMap::from([
        ("input".to_owned(), i64s(in_shape)),
        ("output".to_owned(), i64s(out_shape)),
    ]);
    let mut constants = HashMap::new();
    for (name, shape, data) in consts {
        inputs.push(name.to_owned());
        input_shapes.push(i64s(&shape));
        known.insert(name.to_owned(), i64s(&shape));
        constants.insert(name.to_owned(), Tensor::new(shape, data).unwrap());
    }
    PreparedGraph {
        inputs: vec!["input".to_owned()],
        outputs: vec!["output".to_owned()],
        known_shapes: known,
        planned_ops: vec![PlannedOp {
            name: "op".to_owned(),
            inputs,
            outputs: vec!["output".to_owned()],
            input_shapes,
            output_shapes: vec![i64s(out_shape)],
            kind,
        }],
        constants,
    }
}

/// Runs `graph` on the CPU reference and the GPU and asserts they agree.
pub(crate) fn assert_gpu_matches_cpu_with(graph: &PreparedGraph, input: Tensor, tol: f32) {
    let cpu = graph.run_cpu(&input).expect("cpu reference");
    let compiled = pollster::block_on(CompiledGraph::build(graph)).expect("compile");
    let gpu = pollster::block_on(compiled.run(&input)).expect("gpu run");
    let name = &graph.outputs[0];
    let (c, g) = (&cpu[name], &gpu[name]);
    assert_eq!(c.shape, g.shape, "shape mismatch");
    let worst = c
        .data
        .iter()
        .zip(&g.data)
        .map(|(a, b)| {
            if a == b {
                0.0
            } else {
                (a - b).abs() / (1.0 + a.abs())
            }
        })
        .fold(0.0f32, f32::max);
    assert!(worst <= tol, "GPU differs from CPU reference: {worst}");
}

pub(crate) fn assert_gpu_matches_cpu(graph: &PreparedGraph, in_shape: &[usize]) {
    let input = Tensor::new(in_shape.to_vec(), values(in_shape.iter().product(), 3)).unwrap();
    assert_gpu_matches_cpu_with(graph, input, 1e-4);
}

/// As [`assert_gpu_matches_cpu`] with an f16-exact input (see [`values_f16`]).
pub(crate) fn assert_gpu_matches_cpu_f16(graph: &PreparedGraph, in_shape: &[usize]) {
    let input = Tensor::new(in_shape.to_vec(), values_f16(in_shape.iter().product(), 3)).unwrap();
    assert_gpu_matches_cpu_with(graph, input, 1e-4);
}
