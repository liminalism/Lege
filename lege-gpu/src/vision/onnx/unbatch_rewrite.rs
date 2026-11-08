//! Splits batched convolutions (`N > 1`) into `N` batch-1 convolutions:
//! `Split(axis 0) → Conv ×N → Concat(axis 0)`.
//!
//! The fast conv paths (tiled 1×1 GEMM, Winograd F(2,3), the patchify rewrite)
//! assume batch 1, and a batched Conv otherwise falls back to the naive kernel.
//! Weights and bias are shared; only the activations are split and rejoined.
//! Runs before the patchify and Winograd rewrites so they see batch-1 convs.

use std::collections::BTreeMap;

use crate::vision::onnx::types::{PlannedOp, PlannedOpKind};

pub(crate) fn unbatch_convs(
    ops: Vec<PlannedOp>,
    known_shapes: &mut BTreeMap<String, Vec<i64>>,
) -> Vec<PlannedOp> {
    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        let batch = match (&op.kind, op.input_shapes.first(), op.output_shapes.first()) {
            (PlannedOpKind::Conv2d(_), Some(x), Some(y))
                if x.len() == 4 && y.len() == 4 && x[0] > 1 && y[0] == x[0] =>
            {
                x[0] as usize
            }
            _ => {
                out.push(op);
                continue;
            }
        };
        let PlannedOpKind::Conv2d(plan) = &op.kind else {
            unreachable!()
        };
        let mut x1 = op.input_shapes[0].clone();
        x1[0] = 1;
        let mut y1 = op.output_shapes[0].clone();
        y1[0] = 1;
        let pieces: Vec<String> = (0..batch)
            .map(|i| format!("{}__unbatch_x{i}", op.outputs[0]))
            .collect();
        let results: Vec<String> = (0..batch)
            .map(|i| format!("{}__unbatch_y{i}", op.outputs[0]))
            .collect();
        for (p, r) in pieces.iter().zip(&results) {
            known_shapes.insert(p.clone(), x1.clone());
            known_shapes.insert(r.clone(), y1.clone());
        }
        out.push(PlannedOp {
            name: format!("{}/unbatch_split", op.name),
            inputs: vec![op.inputs[0].clone()],
            outputs: pieces.clone(),
            input_shapes: vec![op.input_shapes[0].clone()],
            output_shapes: vec![x1.clone(); batch],
            kind: PlannedOpKind::Split {
                axis: 0,
                sizes: vec![1; batch],
            },
        });
        for (i, (p, r)) in pieces.iter().zip(&results).enumerate() {
            let mut inputs = op.inputs.clone();
            inputs[0] = p.clone();
            let mut input_shapes = op.input_shapes.clone();
            input_shapes[0] = x1.clone();
            out.push(PlannedOp {
                name: format!("{}/unbatch{i}", op.name),
                inputs,
                outputs: vec![r.clone()],
                input_shapes,
                output_shapes: vec![y1.clone()],
                kind: PlannedOpKind::Conv2d(plan.clone()),
            });
        }
        out.push(PlannedOp {
            name: format!("{}/unbatch_concat", op.name),
            inputs: results,
            outputs: op.outputs.clone(),
            input_shapes: vec![y1.clone(); batch],
            output_shapes: op.output_shapes.clone(),
            kind: PlannedOpKind::Concat { axis: 0 },
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::unbatch_convs;
    use crate::vision::onnx::types::{Conv2dPlan, PlannedOpKind};
    use crate::vision::ops::test_util::{assert_gpu_matches_cpu_with, one_op_graph, values};
    use crate::vision::reference::Tensor;

    #[test]
    fn batched_conv_splits_into_batch_one_convs_with_identical_output() {
        let (n, cin, cout, h, w) = (3, 4, 8, 9, 7);
        let make = || {
            one_op_graph(
                PlannedOpKind::Conv2d(Conv2dPlan {
                    group: 1,
                    pads: [1, 1, 1, 1],
                    strides: [1, 1],
                    dilations: [1, 1],
                    kernel_shape: [3, 3],
                }),
                &[n, cin, h, w],
                vec![
                    ("w", vec![cout, cin, 3, 3], values(cout * cin * 9, 7)),
                    ("b", vec![cout], values(cout, 8)),
                ],
                &[n, cout, h, w],
            )
        };
        let input = Tensor::new(vec![n, cin, h, w], values(n * cin * h * w, 3)).unwrap();
        let want = make().run_cpu(&input).unwrap()["output"].clone();
        let mut g = make();
        g.planned_ops = unbatch_convs(std::mem::take(&mut g.planned_ops), &mut g.known_shapes);
        assert_eq!(g.planned_ops.len(), n + 2);
        let got = g.run_cpu(&input).unwrap()["output"].clone();
        assert_eq!(want, got);
        assert_gpu_matches_cpu_with(&g, input, 1e-4);
    }
}
