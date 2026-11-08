//! Rewrites "patchify" convolutions (square kernel b×b, stride b, no padding, no
//! dilation, group 1, H and W divisible by b; the ViT/DETR patch embedding) into
//! SpaceToDepth(b) + a 1×1 convolution, so they run on the tiled 1×1 GEMM instead
//! of the naive fallback conv kernel.
//!
//! SpaceToDepth (DCR) puts input pixel (c, oh·b+bh, ow·b+bw) at channel
//! (bh·b + bw)·C + c, so the 1×1 weight is the original weight with its
//! (c, bh, bw) axes reordered to (bh, bw, c): W'[o, (bh·b+bw)·C + c] = W[o, c, bh, bw].

use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};

use crate::vision::onnx::attrs::shape_i64_to_usize;
use crate::vision::onnx::types::{Conv2dPlan, PlannedOp, PlannedOpKind};
use crate::vision::reference::Tensor;

pub(crate) fn rewrite_patchify_convs(
    ops: Vec<PlannedOp>,
    known_shapes: &mut BTreeMap<String, Vec<i64>>,
    constants: &mut HashMap<String, Tensor>,
) -> Result<Vec<PlannedOp>> {
    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        match eligible_block(&op, constants) {
            Some(b) => out.extend(rewrite_one(op, b, known_shapes, constants)?),
            None => out.push(op),
        }
    }
    Ok(out)
}

fn eligible_block(op: &PlannedOp, constants: &HashMap<String, Tensor>) -> Option<usize> {
    let PlannedOpKind::Conv2d(plan) = &op.kind else {
        return None;
    };
    let [kh, kw] = plan.kernel_shape;
    let x = op.input_shapes.first()?;
    let ok = kh == kw
        && kh > 1
        && plan.strides == [kh, kw]
        && plan.pads == [0, 0, 0, 0]
        && plan.dilations == [1, 1]
        && plan.group == 1
        && x.len() == 4
        && x[0] == 1
        && x[2] % kh == 0
        && x[3] % kw == 0
        && op.outputs.len() == 1
        && op.inputs.len() >= 2
        && constants.contains_key(&op.inputs[1]);
    ok.then_some(kh as usize)
}

fn rewrite_one(
    op: PlannedOp,
    b: usize,
    known_shapes: &mut BTreeMap<String, Vec<i64>>,
    constants: &mut HashMap<String, Tensor>,
) -> Result<Vec<PlannedOp>> {
    let x = shape_i64_to_usize(&op.input_shapes[0])?;
    let w = constants[&op.inputs[1]].clone();
    if w.shape.len() != 4 || w.shape[2] != b || w.shape[3] != b || w.shape[1] != x[1] {
        bail!(
            "patchify rewrite: weight shape {:?} does not match",
            w.shape
        );
    }
    let (cout, cin) = (w.shape[0], w.shape[1]);
    let k = cin * b * b;
    let mut w2 = vec![0.0f32; cout * k];
    for o in 0..cout {
        for c in 0..cin {
            for bh in 0..b {
                for bw in 0..b {
                    w2[o * k + (bh * b + bw) * cin + c] = w.data[((o * cin + c) * b + bh) * b + bw];
                }
            }
        }
    }
    let s2d_name = format!("{}__patchify_s2d", op.outputs[0]);
    let w_name = format!("{}__patchify_w", op.inputs[1]);
    let s2d_shape = vec![1, k as i64, (x[2] / b) as i64, (x[3] / b) as i64];
    known_shapes.insert(s2d_name.clone(), s2d_shape.clone());
    known_shapes.insert(w_name.clone(), vec![cout as i64, k as i64, 1, 1]);
    constants.insert(w_name.clone(), Tensor::new(vec![cout, k, 1, 1], w2)?);

    let s2d = PlannedOp {
        name: format!("{}/patchify_s2d", op.name),
        inputs: vec![op.inputs[0].clone()],
        outputs: vec![s2d_name.clone()],
        input_shapes: vec![op.input_shapes[0].clone()],
        output_shapes: vec![s2d_shape.clone()],
        kind: PlannedOpKind::SpaceToDepth { blocksize: b },
    };
    let mut inputs = vec![s2d_name, w_name];
    let mut input_shapes = vec![s2d_shape, vec![cout as i64, k as i64, 1, 1]];
    if let (Some(bias), Some(bias_shape)) = (op.inputs.get(2), op.input_shapes.get(2)) {
        inputs.push(bias.clone());
        input_shapes.push(bias_shape.clone());
    }
    let conv = PlannedOp {
        name: format!("{}/patchify_1x1", op.name),
        inputs,
        outputs: op.outputs.clone(),
        input_shapes,
        output_shapes: op.output_shapes.clone(),
        kind: PlannedOpKind::Conv2d(Conv2dPlan {
            group: 1,
            pads: [0, 0, 0, 0],
            strides: [1, 1],
            dilations: [1, 1],
            kernel_shape: [1, 1],
        }),
    };
    Ok(vec![s2d, conv])
}

#[cfg(test)]
mod tests {
    use crate::vision::onnx::types::{Conv2dPlan, PlannedOpKind};
    use crate::vision::ops::test_util::{assert_gpu_matches_cpu_with, one_op_graph, values};
    use crate::vision::reference::Tensor;

    /// The rewritten graph must equal the original strided conv, on CPU and GPU.
    #[test]
    fn patchify_rewrite_matches_strided_conv() {
        let (cin, cout, b, h, w) = (3usize, 24usize, 16usize, 64usize, 48usize);
        let kind = PlannedOpKind::Conv2d(Conv2dPlan {
            group: 1,
            pads: [0, 0, 0, 0],
            strides: [b as i64, b as i64],
            dilations: [1, 1],
            kernel_shape: [b as i64, b as i64],
        });
        let wt = values(cout * cin * b * b, 9);
        let bias = values(cout, 13);
        let original = one_op_graph(
            kind,
            &[1, cin, h, w],
            vec![("w", vec![cout, cin, b, b], wt), ("bias", vec![cout], bias)],
            &[1, cout, h / b, w / b],
        );
        let input = Tensor::new(vec![1, cin, h, w], values(cin * h * w, 3)).unwrap();
        let expected = original.run_cpu(&input).unwrap();

        let mut rewritten = original;
        let ops = std::mem::take(&mut rewritten.planned_ops);
        rewritten.planned_ops = super::rewrite_patchify_convs(
            ops,
            &mut rewritten.known_shapes,
            &mut rewritten.constants,
        )
        .unwrap();
        assert_eq!(
            rewritten.planned_ops.len(),
            2,
            "conv should become SpaceToDepth + 1x1"
        );
        let got = rewritten.run_cpu(&input).unwrap();
        let worst = expected["output"]
            .data
            .iter()
            .zip(&got["output"].data)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "rewrite changes the result on CPU: {worst}");
        assert_gpu_matches_cpu_with(&rewritten, input, 1e-4);
    }
}
