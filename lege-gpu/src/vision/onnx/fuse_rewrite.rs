//! Fuses the op chains PyTorch exports for transformer blocks into single steps:
//!
//! * LayerNorm: `ReduceMean → Sub → Pow(2) → ReduceMean → Add(eps) → Sqrt → Div
//!   [→ Mul(gamma) → Add(beta)]` becomes one `LayerNorm`.
//! * MatMul / Gemm epilogues become one `FusedMatMul`
//!   (`act(alpha · a@b + bias) + residual`). Absorbed: scalar `Mul`s on either
//!   input (attention's `q·s`, `k·s`); and on the output, scalar or per-column
//!   constant `Mul`s (layer scale) and `Add`s (bias), a following `Split` whose
//!   pieces each get a bias `Add` (fused QKV), the erf-GELU chain
//!   `Div(√2) → Erf → Add(1) → Mul(x) → Mul(0.5)`, `Relu`, and a residual `Add`.
//!   Reshapes between the product and an absorbed op are views and are dropped.
//!
//! A value is absorbed only when its producer is the sole reader and it is not
//! a graph output. The fused op takes the slot of the last op it absorbs, so
//! every input it reads (a residual included) is produced before it.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;

use crate::vision::onnx::types::{
    Activation, ElementwiseKind, PlannedOp, PlannedOpKind, UnaryKind,
};
use crate::vision::reference::Tensor;

pub(crate) fn fuse_transformer_ops(
    ops: Vec<PlannedOp>,
    graph_outputs: &[String],
    known_shapes: &mut BTreeMap<String, Vec<i64>>,
    constants: &mut HashMap<String, Tensor>,
) -> Result<Vec<PlannedOp>> {
    let mut g = Fuser {
        ops: ops.into_iter().map(Some).collect(),
        producer: HashMap::new(),
        readers: HashMap::new(),
        outputs: graph_outputs.iter().cloned().collect(),
        known_shapes,
        constants,
        fresh: 0,
    };
    g.index();
    for i in 0..g.ops.len() {
        if g.is_ln_start(i) {
            g.try_layer_norm(i)?;
        }
    }
    for i in 0..g.ops.len() {
        g.try_matmul(i)?;
    }
    Ok(g.ops.into_iter().flatten().collect())
}

struct Fuser<'a> {
    ops: Vec<Option<PlannedOp>>,
    producer: HashMap<String, usize>,
    readers: HashMap<String, Vec<usize>>,
    outputs: HashSet<String>,
    known_shapes: &'a mut BTreeMap<String, Vec<i64>>,
    constants: &'a mut HashMap<String, Tensor>,
    fresh: usize,
}

/// A matmul being grown into a `FusedMatMul`.
struct Mm {
    a: String,
    b: String,
    a_shape: Vec<i64>,
    b_shape: Vec<i64>,
    alpha: f32,
    trans_b: bool,
    n: usize,
    bias: Option<Vec<f32>>,
    residual: Option<(String, Vec<i64>)>,
    act: Activation,
    /// The value the product currently writes and its shape.
    out: String,
    out_shape: Vec<i64>,
    /// Slot the fused op will occupy.
    slot: usize,
}

impl Fuser<'_> {
    fn index(&mut self) {
        self.producer.clear();
        self.readers.clear();
        for (i, op) in self.ops.iter().enumerate() {
            let Some(op) = op else { continue };
            for o in &op.outputs {
                self.producer.insert(o.clone(), i);
            }
            for v in &op.inputs {
                let r = self.readers.entry(v.clone()).or_default();
                if r.last() != Some(&i) {
                    r.push(i);
                }
            }
        }
    }

    fn op(&self, i: usize) -> Option<&PlannedOp> {
        self.ops.get(i)?.as_ref()
    }

    /// The only op reading `value`, when it reads it once and `value` is not a
    /// graph output.
    fn sole_reader(&self, value: &str) -> Option<usize> {
        if self.outputs.contains(value) {
            return None;
        }
        match self.readers.get(value).map(Vec::as_slice) {
            Some([only]) => {
                let op = self.op(*only)?;
                (op.inputs.iter().filter(|v| *v == value).count() == 1).then_some(*only)
            }
            _ => None,
        }
    }

    fn constant(&self, name: &str) -> Option<&Tensor> {
        self.constants.get(name)
    }

    /// For a binary op reading `value`, the other operand if it is a constant.
    fn other_const(&self, op: &PlannedOp, value: &str) -> Option<Tensor> {
        if op.inputs.len() != 2 {
            return None;
        }
        let other = if op.inputs[0] == value {
            &op.inputs[1]
        } else if op.inputs[1] == value {
            &op.inputs[0]
        } else {
            return None;
        };
        self.constant(other).cloned()
    }

    fn other_input<'b>(op: &'b PlannedOp, value: &str) -> Option<&'b String> {
        match op.inputs.as_slice() {
            [x, y] if x == value => Some(y),
            [x, y] if y == value => Some(x),
            _ => None,
        }
    }

    fn add_constant(&mut self, stem: &str, shape: Vec<usize>, data: Vec<f32>) -> Result<String> {
        self.fresh += 1;
        let name = format!("{stem}__fused{}", self.fresh);
        self.known_shapes
            .insert(name.clone(), shape.iter().map(|&d| d as i64).collect());
        self.constants
            .insert(name.clone(), Tensor::new(shape, data)?);
        Ok(name)
    }

    fn remove(&mut self, i: usize) {
        self.ops[i] = None;
    }

    // ── LayerNorm ────────────────────────────────────────────────────────

    fn is_ln_start(&self, i: usize) -> bool {
        matches!(
            self.op(i).map(|op| &op.kind),
            Some(PlannedOpKind::ReduceSum {
                mean: true,
                keepdims: true,
                ..
            })
        )
    }

    fn reader_kind(&self, value: &str) -> Option<(usize, &PlannedOp)> {
        let i = self.sole_reader(value)?;
        Some((i, self.op(i)?))
    }

    fn try_layer_norm(&mut self, i: usize) -> Result<()> {
        let Some(plan) = self.match_layer_norm(i) else {
            return Ok(());
        };
        let LnMatch {
            x,
            x_shape,
            axis,
            epsilon,
            gamma,
            beta,
            chain,
            tail,
        } = plan;
        let dim = x_shape[axis] as usize;
        let tail_op = self.op(tail).expect("tail op");
        let (out, out_shape, name) = (
            tail_op.outputs[0].clone(),
            tail_op.output_shapes[0].clone(),
            tail_op.name.clone(),
        );
        let g = self.add_constant(&format!("{out}__ln_gamma"), vec![dim], gamma)?;
        let b = self.add_constant(&format!("{out}__ln_beta"), vec![dim], beta)?;
        for &c in &chain {
            self.remove(c);
        }
        self.ops[tail] = Some(PlannedOp {
            name: format!("{name}/layernorm"),
            inputs: vec![x, g, b],
            outputs: vec![out],
            input_shapes: vec![x_shape, vec![dim as i64], vec![dim as i64]],
            output_shapes: vec![out_shape],
            kind: PlannedOpKind::LayerNorm { axis, epsilon },
        });
        self.index();
        Ok(())
    }

    fn match_layer_norm(&self, i: usize) -> Option<LnMatch> {
        let r1 = self.op(i)?;
        let PlannedOpKind::ReduceSum { axis, .. } = r1.kind else {
            return None;
        };
        let x = r1.inputs.first()?.clone();
        let x_shape = r1.input_shapes.first()?.clone();
        let rank = x_shape.len();
        let mean = &r1.outputs[0];
        let (sub_i, sub) = self.reader_kind(mean)?;
        if !matches!(sub.kind, PlannedOpKind::Elementwise(ElementwiseKind::Sub))
            || sub.inputs != [x.clone(), mean.clone()]
        {
            return None;
        }
        let d = &sub.outputs[0];
        let readers = self.readers.get(d)?;
        if readers.len() != 2 || self.outputs.contains(d) {
            return None;
        }
        let (mut pow_i, mut div_i) = (None, None);
        for &r in readers {
            match &self.op(r)?.kind {
                PlannedOpKind::Unary(UnaryKind::Pow { exponent }) if *exponent == 2.0 => {
                    pow_i = Some(r)
                }
                PlannedOpKind::Elementwise(ElementwiseKind::Div) => div_i = Some(r),
                _ => return None,
            }
        }
        let (pow_i, div_i) = (pow_i?, div_i?);
        let (r2_i, r2) = self.reader_kind(&self.op(pow_i)?.outputs[0])?;
        match r2.kind {
            PlannedOpKind::ReduceSum {
                axis: a2,
                keepdims: true,
                mean: true,
            } if a2 == axis => {}
            _ => return None,
        }
        let (add_i, add) = self.reader_kind(&r2.outputs[0])?;
        let eps = self.other_const(add, &r2.outputs[0])?;
        if !matches!(add.kind, PlannedOpKind::Elementwise(ElementwiseKind::Add))
            || eps.data.len() != 1
        {
            return None;
        }
        let (sqrt_i, sqrt) = self.reader_kind(&add.outputs[0])?;
        if !matches!(sqrt.kind, PlannedOpKind::Unary(UnaryKind::Sqrt)) {
            return None;
        }
        let div = self.op(div_i)?;
        if self.sole_reader(&sqrt.outputs[0]) != Some(div_i)
            || div.inputs != [d.clone(), sqrt.outputs[0].clone()]
        {
            return None;
        }
        let dim = x_shape[axis] as usize;
        let mut chain = vec![i, sub_i, pow_i, r2_i, add_i, sqrt_i, div_i];
        let mut tail = div_i;
        let (mut gamma, mut beta) = (vec![1.0; dim], vec![0.0; dim]);
        if let Some((mul_i, mul)) = self.reader_kind(&div.outputs[0])
            && matches!(mul.kind, PlannedOpKind::Elementwise(ElementwiseKind::Mul))
            && let Some(g) = self
                .other_const(mul, &div.outputs[0])
                .and_then(|t| axis_vector(&t, rank, axis, dim))
        {
            gamma = g;
            chain.push(mul_i);
            tail = mul_i;
            if let Some((badd_i, badd)) = self.reader_kind(&mul.outputs[0])
                && matches!(badd.kind, PlannedOpKind::Elementwise(ElementwiseKind::Add))
                && let Some(b) = self
                    .other_const(badd, &mul.outputs[0])
                    .and_then(|t| axis_vector(&t, rank, axis, dim))
            {
                beta = b;
                chain.push(badd_i);
                tail = badd_i;
            }
        }
        if self.op(tail)?.output_shapes[0] != x_shape {
            return None;
        }
        chain.retain(|&c| c != tail);
        Some(LnMatch {
            x,
            x_shape,
            axis,
            epsilon: eps.data[0],
            gamma,
            beta,
            chain,
            tail,
        })
    }

    // ── MatMul epilogues ─────────────────────────────────────────────────

    fn start_mm(&self, i: usize) -> Option<Mm> {
        let op = self.op(i)?;
        let shape_n = |b: &[i64], trans_b: bool| -> Option<usize> {
            let r = b.len();
            if r < 2 {
                return None;
            }
            Some(if trans_b { b[r - 2] } else { b[r - 1] } as usize)
        };
        let (alpha, trans_b, bias) = match &op.kind {
            PlannedOpKind::MatMul => {
                if op.input_shapes[0].len() < 2 || op.input_shapes[1].len() < 2 {
                    return None;
                }
                (1.0, false, None)
            }
            PlannedOpKind::Gemm {
                alpha,
                beta,
                trans_a: false,
                trans_b,
            } => {
                let n = shape_n(&op.input_shapes[1], *trans_b)?;
                let bias = match op.inputs.get(2) {
                    None => None,
                    Some(c) => {
                        let c = self.constant(c)?;
                        let last = c.shape.last().copied().unwrap_or(1);
                        let per_col = c.data.len() == n && last == n;
                        if c.data.len() == 1 {
                            Some(vec![beta * c.data[0]; n])
                        } else if per_col {
                            Some(c.data.iter().map(|v| beta * v).collect())
                        } else {
                            return None;
                        }
                    }
                };
                (*alpha, *trans_b, bias)
            }
            _ => return None,
        };
        let n = shape_n(&op.input_shapes[1], trans_b)?;
        Some(Mm {
            a: op.inputs[0].clone(),
            b: op.inputs[1].clone(),
            a_shape: op.input_shapes[0].clone(),
            b_shape: op.input_shapes[1].clone(),
            alpha,
            trans_b,
            n,
            bias,
            residual: None,
            act: Activation::None,
            out: op.outputs[0].clone(),
            out_shape: op.output_shapes[0].clone(),
            slot: i,
        })
    }

    fn try_matmul(&mut self, i: usize) -> Result<()> {
        let Some(mut mm) = self.start_mm(i) else {
            return Ok(());
        };
        let mut removed = Vec::new();
        // Scalar Muls feeding either operand.
        for which in [0, 1] {
            let v = if which == 0 { &mm.a } else { &mm.b };
            let Some(&p) = self.producer.get(v) else {
                continue;
            };
            let Some(op) = self.op(p) else { continue };
            if !matches!(op.kind, PlannedOpKind::Elementwise(ElementwiseKind::Mul))
                || self.sole_reader(v) != Some(i)
            {
                continue;
            }
            let (x, s) = match op.inputs.as_slice() {
                [x, s] | [s, x] if self.constant(s).is_some_and(|t| t.data.len() == 1) => {
                    (x.clone(), self.constant(s).unwrap().data[0])
                }
                _ => continue,
            };
            let x_shape = op.input_shapes[op.inputs.iter().position(|n| *n == x).unwrap()].clone();
            if x_shape != op.output_shapes[0] {
                continue;
            }
            mm.alpha *= s;
            if which == 0 {
                mm.a = x;
                mm.a_shape = x_shape;
            } else {
                mm.b = x;
                mm.b_shape = x_shape;
            }
            removed.push(p);
        }
        while let Some(step) = self.absorb_step(&mut mm)? {
            removed.extend(step);
        }
        if removed.is_empty() {
            return Ok(());
        }
        self.emit(i, mm, &removed)
    }

    /// Absorbs the next op after the product; returns the op slots removed.
    fn absorb_step(&mut self, mm: &mut Mm) -> Result<Option<Vec<usize>>> {
        let plain = mm.act == Activation::None && mm.residual.is_none();
        if plain && let Some(gelu) = self.match_gelu(&mm.out, &mm.out_shape) {
            let last = *gelu.last().unwrap();
            let op = self.op(last).unwrap();
            mm.out = op.outputs[0].clone();
            mm.out_shape = op.output_shapes[0].clone();
            mm.act = Activation::Gelu;
            mm.slot = last;
            return Ok(Some(gelu));
        }
        let Some(mut r) = self.sole_reader(&mm.out) else {
            return Ok(None);
        };
        let mut through = Vec::new();
        let mut value = mm.out.clone();
        let mut value_shape = mm.out_shape.clone();
        if let Some(op) = self.op(r)
            && matches!(op.kind, PlannedOpKind::Reshape { .. })
        {
            value = op.outputs[0].clone();
            value_shape = op.output_shapes[0].clone();
            through.push(r);
            match self.sole_reader(&value) {
                Some(next) => r = next,
                None => return Ok(None),
            }
        }
        let (action, out, out_shape) = {
            let op = self.op(r).unwrap();
            let action = match &op.kind {
                PlannedOpKind::Unary(UnaryKind::Relu) if plain && through.is_empty() => Next::Relu,
                PlannedOpKind::Elementwise(ElementwiseKind::Mul) => {
                    match self.other_const(op, &value) {
                        Some(c) => Next::ConstMul(c),
                        None => return Ok(None),
                    }
                }
                PlannedOpKind::Elementwise(ElementwiseKind::Add) => {
                    match self.other_const(op, &value) {
                        Some(c) => Next::ConstAdd(c),
                        None => match Self::other_input(op, &value) {
                            Some(o)
                                if op.input_shapes.iter().all(|s| *s == op.output_shapes[0]) =>
                            {
                                Next::Residual(o.clone())
                            }
                            _ => return Ok(None),
                        },
                    }
                }
                PlannedOpKind::Split { axis, sizes }
                    if plain && through.is_empty() && *axis + 1 == op.input_shapes[0].len() =>
                {
                    Next::Split(sizes.clone())
                }
                _ => return Ok(None),
            };
            (action, op.outputs[0].clone(), op.output_shapes[0].clone())
        };
        match action {
            Next::Relu => mm.act = Activation::Relu,
            Next::ConstMul(c) | Next::ConstAdd(c) if !plain => {
                let _ = c;
                return Ok(None);
            }
            Next::ConstMul(c) => {
                let Some(col) = self.column_values(&c, &value_shape, &out_shape, mm.n) else {
                    return Ok(None);
                };
                if c.data.len() == 1 {
                    mm.alpha *= col[0];
                } else if !self.scale_columns(mm, &col)? {
                    return Ok(None);
                }
                if let Some(b) = &mut mm.bias {
                    b.iter_mut().zip(&col).for_each(|(b, s)| *b *= s);
                }
            }
            Next::ConstAdd(c) => {
                let Some(col) = self.column_values(&c, &value_shape, &out_shape, mm.n) else {
                    return Ok(None);
                };
                let b = mm.bias.get_or_insert_with(|| vec![0.0; mm.n]);
                b.iter_mut().zip(&col).for_each(|(b, c)| *b += c);
            }
            Next::Residual(other) => {
                if mm.residual.is_some() {
                    return Ok(None);
                }
                mm.residual = Some((other, out_shape.clone()));
            }
            Next::Split(sizes) => return self.absorb_split_biases(mm, r, sizes),
        }
        mm.out = out;
        mm.out_shape = out_shape;
        mm.slot = r;
        through.push(r);
        Ok(Some(through))
    }

    /// A scalar or per-last-axis constant as N column values, when applying it
    /// does not broadcast the product to a larger shape.
    fn column_values(
        &self,
        c: &Tensor,
        value_shape: &[i64],
        out_shape: &[i64],
        n: usize,
    ) -> Option<Vec<f32>> {
        if value_shape != out_shape || value_shape.last() != Some(&(n as i64)) {
            return None;
        }
        if c.data.len() == 1 {
            return Some(vec![c.data[0]; n]);
        }
        let rank = value_shape.len();
        axis_vector(c, rank, rank - 1, n)
    }

    /// Multiplies column n of the (constant, 2-D) weight by `col[n]`.
    fn scale_columns(&mut self, mm: &mut Mm, col: &[f32]) -> Result<bool> {
        let Some(w) = self.constant(&mm.b).cloned() else {
            return Ok(false);
        };
        if w.shape.len() != 2 {
            return Ok(false);
        }
        let (rows, cols) = (w.shape[0], w.shape[1]);
        let mut data = w.data.clone();
        for r in 0..rows {
            for c in 0..cols {
                data[r * cols + c] *= if mm.trans_b { col[r] } else { col[c] };
            }
        }
        mm.b = self.add_constant(&mm.b.clone(), w.shape.clone(), data)?;
        Ok(true)
    }

    /// `Split` on the last axis whose every piece feeds a constant bias `Add`:
    /// the biases join the product's bias and the `Add`s disappear (the `Split`
    /// now writes their outputs).
    fn absorb_split_biases(
        &mut self,
        mm: &mut Mm,
        split_i: usize,
        sizes: Vec<i64>,
    ) -> Result<Option<Vec<usize>>> {
        let split = self.op(split_i).unwrap();
        let rank = split.input_shapes[0].len();
        let mut bias = Vec::with_capacity(mm.n);
        let mut adds = Vec::new();
        for (piece, &size) in split.outputs.iter().zip(&sizes) {
            let Some((ai, add)) = self.reader_kind(piece) else {
                return Ok(None);
            };
            if !matches!(add.kind, PlannedOpKind::Elementwise(ElementwiseKind::Add)) {
                return Ok(None);
            }
            let Some(v) = self
                .other_const(add, piece)
                .and_then(|c| axis_vector(&c, rank, rank - 1, size as usize))
            else {
                return Ok(None);
            };
            if add.output_shapes[0] != split.output_shapes[adds.len()] {
                return Ok(None);
            }
            bias.extend(v);
            adds.push((ai, add.outputs[0].clone()));
        }
        if bias.len() != mm.n {
            return Ok(None);
        }
        let b = mm.bias.get_or_insert_with(|| vec![0.0; mm.n]);
        b.iter_mut().zip(&bias).for_each(|(b, c)| *b += c);
        let split = self.ops[split_i].as_mut().unwrap();
        for (k, (_, out)) in adds.iter().enumerate() {
            split.outputs[k] = out.clone();
        }
        let removed: Vec<usize> = adds.iter().map(|(ai, _)| *ai).collect();
        for &ai in &removed {
            self.ops[ai] = None;
        }
        self.index();
        // The Split stays after the product and now writes the Adds' outputs.
        Ok(Some(removed))
    }

    /// `Div(y, √2) → Erf → Add(1) → Mul(y, ·) → Mul(0.5)` (or `Mul(y, 1/√2)`
    /// first); returns the chain's slots in order.
    fn match_gelu(&self, y: &str, y_shape: &[i64]) -> Option<Vec<usize>> {
        if self.outputs.contains(y) {
            return None;
        }
        let readers = self.readers.get(y)?;
        if readers.len() != 2 {
            return None;
        }
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4 * b.abs().max(1.0);
        let is_scale_in = |i: usize| -> bool {
            let Some(op) = self.op(i) else { return false };
            let Some(c) = self.other_const(op, y) else {
                return false;
            };
            if c.data.len() != 1 || op.inputs[0] != y {
                return false;
            }
            match op.kind {
                PlannedOpKind::Elementwise(ElementwiseKind::Div) => {
                    near(c.data[0], std::f32::consts::SQRT_2)
                }
                PlannedOpKind::Elementwise(ElementwiseKind::Mul) => {
                    near(c.data[0], std::f32::consts::FRAC_1_SQRT_2)
                }
                _ => false,
            }
        };
        let (scale_i, mul_i) = match readers.as_slice() {
            [p, q] if is_scale_in(*p) => (*p, *q),
            [p, q] if is_scale_in(*q) => (*q, *p),
            _ => return None,
        };
        let (erf_i, erf) = self.reader_kind(&self.op(scale_i)?.outputs[0])?;
        if !matches!(erf.kind, PlannedOpKind::Unary(UnaryKind::Erf)) {
            return None;
        }
        let (one_i, one) = self.reader_kind(&erf.outputs[0])?;
        let c = self.other_const(one, &erf.outputs[0])?;
        if !matches!(one.kind, PlannedOpKind::Elementwise(ElementwiseKind::Add))
            || c.data.len() != 1
            || !near(c.data[0], 1.0)
        {
            return None;
        }
        let mul = self.op(mul_i)?;
        if !matches!(mul.kind, PlannedOpKind::Elementwise(ElementwiseKind::Mul))
            || self.sole_reader(&one.outputs[0]) != Some(mul_i)
            || Self::other_input(mul, y) != Some(&one.outputs[0])
        {
            return None;
        }
        let (half_i, half) = self.reader_kind(&mul.outputs[0])?;
        let c = self.other_const(half, &mul.outputs[0])?;
        if !matches!(half.kind, PlannedOpKind::Elementwise(ElementwiseKind::Mul))
            || c.data.len() != 1
            || !near(c.data[0], 0.5)
            || half.output_shapes[0] != y_shape
        {
            return None;
        }
        Some(vec![scale_i, erf_i, one_i, mul_i, half_i])
    }

    fn emit(&mut self, i: usize, mm: Mm, removed: &[usize]) -> Result<()> {
        let name = self.op(i).unwrap().name.clone();
        let mut inputs = vec![mm.a.clone(), mm.b.clone()];
        let mut input_shapes = vec![mm.a_shape.clone(), mm.b_shape.clone()];
        let has_bias = mm.bias.is_some();
        if let Some(bias) = mm.bias {
            let b = self.add_constant(&format!("{}__bias", mm.out), vec![mm.n], bias)?;
            inputs.push(b);
            input_shapes.push(vec![mm.n as i64]);
        }
        let has_residual = mm.residual.is_some();
        if let Some((r, shape)) = mm.residual {
            inputs.push(r);
            input_shapes.push(shape);
        }
        for &r in removed {
            self.ops[r] = None;
        }
        self.ops[i] = None;
        self.ops[mm.slot] = Some(PlannedOp {
            name: format!("{name}/fused"),
            inputs,
            outputs: vec![mm.out],
            input_shapes,
            output_shapes: vec![mm.out_shape.clone()],
            kind: PlannedOpKind::FusedMatMul {
                alpha: mm.alpha,
                trans_b: mm.trans_b,
                has_bias,
                has_residual,
                act: mm.act,
                out_shape: mm.out_shape.iter().map(|&d| d as usize).collect(),
            },
        });
        self.index();
        Ok(())
    }
}

/// What follows the product, decided before any mutation.
enum Next {
    Relu,
    ConstMul(Tensor),
    ConstAdd(Tensor),
    Residual(String),
    Split(Vec<i64>),
}

struct LnMatch {
    x: String,
    x_shape: Vec<i64>,
    axis: usize,
    epsilon: f32,
    gamma: Vec<f32>,
    beta: Vec<f32>,
    chain: Vec<usize>,
    tail: usize,
}

/// The values of a constant that broadcasts along `axis` only (all its other
/// dims are 1 once right-aligned to `rank`), when it has `dim` of them.
fn axis_vector(t: &Tensor, rank: usize, axis: usize, dim: usize) -> Option<Vec<f32>> {
    if t.shape.len() > rank || t.data.len() != dim {
        return None;
    }
    let pad = rank - t.shape.len();
    let ok = (0..rank).all(|d| {
        let size = if d < pad { 1 } else { t.shape[d - pad] };
        if d == axis { size == dim } else { size == 1 }
    });
    ok.then(|| t.data.clone())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::fuse_transformer_ops;
    use crate::vision::onnx::graph::PreparedGraph;
    use crate::vision::onnx::types::{ElementwiseKind, PlannedOp, PlannedOpKind, UnaryKind};
    use crate::vision::ops::test_util::{assert_gpu_matches_cpu_with, coop_gemm_active, values};
    use crate::vision::reference::Tensor;

    /// Small graph builder: runtime input `x`, ops appended in order.
    struct B {
        ops: Vec<PlannedOp>,
        shapes: BTreeMap<String, Vec<i64>>,
        consts: HashMap<String, Tensor>,
    }

    impl B {
        fn new(x: &[usize]) -> Self {
            let mut shapes = BTreeMap::new();
            shapes.insert("x".into(), x.iter().map(|&d| d as i64).collect());
            Self {
                ops: vec![],
                shapes,
                consts: HashMap::new(),
            }
        }
        fn c(&mut self, name: &str, shape: &[usize], data: Vec<f32>) -> String {
            self.shapes
                .insert(name.into(), shape.iter().map(|&d| d as i64).collect());
            self.consts
                .insert(name.into(), Tensor::new(shape.to_vec(), data).unwrap());
            name.into()
        }
        fn op(
            &mut self,
            kind: PlannedOpKind,
            ins: &[&str],
            out: &str,
            out_shape: &[usize],
        ) -> String {
            let os: Vec<i64> = out_shape.iter().map(|&d| d as i64).collect();
            self.shapes.insert(out.into(), os.clone());
            self.ops.push(PlannedOp {
                name: out.into(),
                inputs: ins.iter().map(|s| s.to_string()).collect(),
                outputs: vec![out.into()],
                input_shapes: ins.iter().map(|s| self.shapes[*s].clone()).collect(),
                output_shapes: vec![os],
                kind,
            });
            out.into()
        }
        fn graph(self, out: &str) -> PreparedGraph {
            PreparedGraph {
                inputs: vec!["x".into()],
                outputs: vec![out.into()],
                known_shapes: self.shapes,
                planned_ops: self.ops,
                constants: self.consts,
            }
        }
    }

    fn ew(k: ElementwiseKind) -> PlannedOpKind {
        PlannedOpKind::Elementwise(k)
    }

    fn reduce_mean(axis: usize) -> PlannedOpKind {
        PlannedOpKind::ReduceSum {
            axis,
            keepdims: true,
            mean: true,
        }
    }

    /// One ViT block in PyTorch-export form: LN → QKV MatMul → Split → bias Adds
    /// → attention with scaled q and k → Gemm → Reshape → layer scale → residual
    /// → LN → Gemm → GELU → Gemm → layer scale → residual.
    fn vit_block(t: usize, d: usize, h: usize) -> PreparedGraph {
        let hd = d / h;
        let mut b = B::new(&[1, t, d]);
        let ln = |b: &mut B, x: &str, p: &str| -> String {
            let m = b.op(reduce_mean(2), &[x], &format!("{p}m"), &[1, t, 1]);
            let s = b.op(
                ew(ElementwiseKind::Sub),
                &[x, &m],
                &format!("{p}s"),
                &[1, t, d],
            );
            let q = b.op(
                PlannedOpKind::Unary(UnaryKind::Pow { exponent: 2.0 }),
                &[&s],
                &format!("{p}q"),
                &[1, t, d],
            );
            let v = b.op(reduce_mean(2), &[&q], &format!("{p}v"), &[1, t, 1]);
            let e = b.c(&format!("{p}eps"), &[], vec![1e-6]);
            let ve = b.op(
                ew(ElementwiseKind::Add),
                &[&v, &e],
                &format!("{p}ve"),
                &[1, t, 1],
            );
            let sd = b.op(
                PlannedOpKind::Unary(UnaryKind::Sqrt),
                &[&ve],
                &format!("{p}sd"),
                &[1, t, 1],
            );
            let n = b.op(
                ew(ElementwiseKind::Div),
                &[&s, &sd],
                &format!("{p}n"),
                &[1, t, d],
            );
            let g = b.c(&format!("{p}g"), &[d], values(d, 21));
            let ng = b.op(
                ew(ElementwiseKind::Mul),
                &[&n, &g],
                &format!("{p}ng"),
                &[1, t, d],
            );
            let be = b.c(&format!("{p}be"), &[d], values(d, 22));
            b.op(
                ew(ElementwiseKind::Add),
                &[&ng, &be],
                &format!("{p}out"),
                &[1, t, d],
            )
        };
        let x1 = ln(&mut b, "x", "ln1");
        let wqkv = b.c("wqkv", &[d, 3 * d], values(d * 3 * d, 31));
        let qkv = b.op(PlannedOpKind::MatMul, &[&x1, &wqkv], "qkv", &[1, t, 3 * d]);
        b.shapes.insert("q0".into(), vec![1, t as i64, d as i64]);
        b.shapes.insert("k0".into(), vec![1, t as i64, d as i64]);
        b.shapes.insert("v0".into(), vec![1, t as i64, d as i64]);
        b.ops.push(PlannedOp {
            name: "split".into(),
            inputs: vec![qkv.clone()],
            outputs: vec!["q0".into(), "k0".into(), "v0".into()],
            input_shapes: vec![vec![1, t as i64, 3 * d as i64]],
            output_shapes: vec![vec![1, t as i64, d as i64]; 3],
            kind: PlannedOpKind::Split {
                axis: 2,
                sizes: vec![d as i64; 3],
            },
        });
        let mut qkvs = vec![];
        for (i, n) in ["q0", "k0", "v0"].iter().enumerate() {
            let bias = b.c(&format!("{n}b"), &[d], values(d, 40 + i as u32));
            let a = b.op(
                ew(ElementwiseKind::Add),
                &[&bias, n],
                &format!("{n}a"),
                &[1, t, d],
            );
            let r = b.op(
                PlannedOpKind::Reshape {
                    target: vec![1, t as i64, h as i64, hd as i64],
                },
                &[&a],
                &format!("{n}r"),
                &[1, t, h, hd],
            );
            let perm = if i == 1 {
                vec![0, 2, 3, 1]
            } else {
                vec![0, 2, 1, 3]
            };
            let os = if i == 1 {
                vec![1, h, hd, t]
            } else {
                vec![1, h, t, hd]
            };
            qkvs.push(b.op(
                PlannedOpKind::Transpose { perm },
                &[&r],
                &format!("{n}t"),
                &os,
            ));
        }
        let s = b.c("scale", &[], vec![(hd as f32).powf(-0.25)]);
        let qs = b.op(
            ew(ElementwiseKind::Mul),
            &[&qkvs[0], &s],
            "qs",
            &[1, h, t, hd],
        );
        let ks = b.op(
            ew(ElementwiseKind::Mul),
            &[&qkvs[1], &s],
            "ks",
            &[1, h, hd, t],
        );
        let att = b.op(PlannedOpKind::MatMul, &[&qs, &ks], "att", &[1, h, t, t]);
        let sm = b.op(
            PlannedOpKind::Softmax { axis: 3 },
            &[&att],
            "sm",
            &[1, h, t, t],
        );
        let av = b.op(
            PlannedOpKind::MatMul,
            &[&sm, &qkvs[2]],
            "av",
            &[1, h, t, hd],
        );
        let at = b.op(
            PlannedOpKind::Transpose {
                perm: vec![0, 2, 1, 3],
            },
            &[&av],
            "at",
            &[1, t, h, hd],
        );
        let a2 = b.op(
            PlannedOpKind::Reshape {
                target: vec![t as i64, d as i64],
            },
            &[&at],
            "a2",
            &[t, d],
        );
        let gemm = |b: &mut B, x: &str, k: usize, n: usize, p: &str| -> String {
            let w = b.c(&format!("{p}w"), &[k, n], values(k * n, 50 + n as u32));
            let c = b.c(&format!("{p}c"), &[n], values(n, 60 + n as u32));
            b.op(
                PlannedOpKind::Gemm {
                    alpha: 1.0,
                    beta: 1.0,
                    trans_a: false,
                    trans_b: false,
                },
                &[x, &w, &c],
                &format!("{p}y"),
                &[t, n],
            )
        };
        let o = gemm(&mut b, &a2, d, d, "proj");
        let o3 = b.op(
            PlannedOpKind::Reshape {
                target: vec![1, t as i64, d as i64],
            },
            &[&o],
            "o3",
            &[1, t, d],
        );
        let l1 = b.c("ls1", &[d], values(d, 70));
        let ol = b.op(ew(ElementwiseKind::Mul), &[&o3, &l1], "ol", &[1, t, d]);
        let r1 = b.op(ew(ElementwiseKind::Add), &[&ol, "x"], "r1", &[1, t, d]);
        let x2 = ln(&mut b, &r1, "ln2");
        let x2f = b.op(
            PlannedOpKind::Reshape {
                target: vec![t as i64, d as i64],
            },
            &[&x2],
            "x2f",
            &[t, d],
        );
        let f1 = gemm(&mut b, &x2f, d, 4 * d, "fc1");
        let k2 = b.c("sqrt2", &[], vec![std::f32::consts::SQRT_2]);
        let g1 = b.op(ew(ElementwiseKind::Div), &[&f1, &k2], "g1", &[t, 4 * d]);
        let g2 = b.op(
            PlannedOpKind::Unary(UnaryKind::Erf),
            &[&g1],
            "g2",
            &[t, 4 * d],
        );
        let one = b.c("one", &[], vec![1.0]);
        let g3 = b.op(ew(ElementwiseKind::Add), &[&g2, &one], "g3", &[t, 4 * d]);
        let g4 = b.op(ew(ElementwiseKind::Mul), &[&f1, &g3], "g4", &[t, 4 * d]);
        let half = b.c("half", &[], vec![0.5]);
        let g5 = b.op(ew(ElementwiseKind::Mul), &[&g4, &half], "g5", &[t, 4 * d]);
        let f2 = gemm(&mut b, &g5, 4 * d, d, "fc2");
        let f3 = b.op(
            PlannedOpKind::Reshape {
                target: vec![1, t as i64, d as i64],
            },
            &[&f2],
            "f3",
            &[1, t, d],
        );
        let l2 = b.c("ls2", &[d], values(d, 71));
        let fl = b.op(ew(ElementwiseKind::Mul), &[&f3, &l2], "fl", &[1, t, d]);
        b.op(ew(ElementwiseKind::Add), &[&fl, &r1], "out", &[1, t, d]);
        b.graph("out")
    }

    fn count(g: &PreparedGraph, label: &str) -> usize {
        g.planned_ops
            .iter()
            .filter(|o| o.kind.label() == label)
            .count()
    }

    #[test]
    fn vit_block_fuses_and_matches_unfused_reference() {
        let (t, d, h) = (37, 64, 4);
        let plain = vit_block(t, d, h);
        let input = Tensor::new(vec![1, t, d], values(t * d, 3)).unwrap();
        let want = plain.run_cpu(&input).unwrap()["out"].clone();

        let mut fused = vit_block(t, d, h);
        fused.planned_ops = fuse_transformer_ops(
            std::mem::take(&mut fused.planned_ops),
            &fused.outputs.clone(),
            &mut fused.known_shapes,
            &mut fused.constants,
        )
        .unwrap();
        assert_eq!(count(&fused, "LayerNorm"), 2);
        assert_eq!(count(&fused, "FusedMatMul"), 5);
        for gone in [
            "Erf",
            "Pow",
            "Sqrt",
            "Add",
            "Mul",
            "Div",
            "Sub",
            "ReduceSum",
        ] {
            assert_eq!(count(&fused, gone), 0, "{gone} left after fusion");
        }
        let got = fused.run_cpu(&input).unwrap()["out"].clone();
        let worst = want
            .data
            .iter()
            .zip(&got.data)
            .map(|(a, b)| (a - b).abs() / (1.0 + a.abs()))
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "fused CPU differs from unfused: {worst}");
        // On the tensor-core path every GEMM rounds its operands to f16, and the
        // block chains seven of them.
        let tol = if coop_gemm_active() { 2e-2 } else { 1e-4 };
        assert_gpu_matches_cpu_with(&fused, input, tol);
    }
}
