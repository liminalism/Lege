//! Plan-time types: op kinds, conv/pool params, elementwise ops.
//! These are leaf types; nothing in this module imports other crate submodules.

#[derive(Debug, Clone)]
pub(crate) enum TensorConst {
    Int64(Vec<i64>),
    Float32(Vec<f32>),
}

#[derive(Debug)]
pub(crate) struct PlannedOp {
    pub(crate) name: String,
    pub(crate) inputs: Vec<String>,
    pub(crate) outputs: Vec<String>,
    pub(crate) input_shapes: Vec<Vec<i64>>,
    pub(crate) output_shapes: Vec<Vec<i64>>,
    pub(crate) kind: PlannedOpKind,
}

#[derive(Debug)]
pub(crate) enum PlannedOpKind {
    Conv2d(Conv2dPlan),
    /// Winograd F(2,3) input transform: x[1,Cin,H,W] -> V[16,Cin,P].
    WinogradInputTransform,
    /// Winograd F(2,3) batched transform-space GEMM: U[16,Cout,Cin] * V[16,Cin,P] -> M[16,Cout,P].
    WinogradBatchedGemm,
    /// Winograd F(2,3) output transform: M[16,Cout,P] -> y[1,Cout,H,W].
    WinogradOutputTransform {
        use_bias: bool,
        h: usize,
        w: usize,
    },
    Elementwise(ElementwiseKind),
    Unary(UnaryKind),
    Concat {
        axis: usize,
    },
    Split {
        axis: usize,
        sizes: Vec<i64>,
    },
    Slice {
        axes: Vec<usize>,
        starts: Vec<i64>,
        ends: Vec<i64>,
        steps: Vec<i64>,
    },
    Reshape {
        target: Vec<i64>,
    },
    Transpose {
        perm: Vec<usize>,
    },
    MaxPool2d(Pool2dPlan),
    AvgPool2d(Pool2dPlan),
    ResizeNearest {
        scales: Vec<f32>,
    },
    ResizeLinear {
        sizes: Vec<i64>,
        align_corners: bool,
    },
    Pad {
        pads: Vec<i64>,
        mode: PadMode,
        value: f32,
    },
    PRelu,
    Unsqueeze {
        axes: Vec<usize>,
    },
    Squeeze {
        axes: Vec<usize>,
    },
    GridSample {
        align_corners: bool,
    },
    MatMul,
    Gemm {
        alpha: f32,
        beta: f32,
        trans_a: bool,
        trans_b: bool,
    },
    Softmax {
        axis: usize,
    },
    GlobalAveragePool,
    /// Inclusive prefix sum along one axis (exclusive=0, reverse=0).
    CumSum {
        axis: usize,
    },
    /// Sum-reduce a single axis. `keepdims` retains it as a size-1 dim.
    /// When `mean` is set, the sum is divided by the axis length (ReduceMean).
    ReduceSum {
        axis: usize,
        keepdims: bool,
        mean: bool,
    },
    /// SpaceToDepth (DCR mode): [N,C,H,W] -> [N,C*b*b,H/b,W/b].
    SpaceToDepth {
        blocksize: usize,
    },
    /// DepthToSpace (DCR mode): [N,C,H,W] -> [N,C/(b*b),H*b,W*b].
    DepthToSpace {
        blocksize: usize,
    },
    /// Max-reduce a single axis.
    ReduceMax {
        axis: usize,
        keepdims: bool,
    },
    /// Tile with constant repeats (the planned output shape carries them).
    Tile {
        out_shape: Vec<usize>,
    },
    /// Expand to a constant target shape (bidirectional broadcast).
    Expand {
        out_shape: Vec<usize>,
    },
    /// Where(cond, x, y) with broadcasting; cond is carried as 0.0 / 1.0.
    Where,
    /// Gather along `axis` of a runtime tensor with constant, normalised indices.
    /// `out_shape` is the planned output (a scalar index drops the axis).
    Gather {
        axis: usize,
        indices: Vec<usize>,
        out_shape: Vec<usize>,
    },
    /// GatherElements along `axis`; indices are a runtime tensor (integers in f32).
    GatherElements {
        axis: usize,
    },
    /// TopK along `axis` with a constant k; outputs (values, indices) sorted.
    TopK {
        axis: usize,
        k: usize,
        largest: bool,
    },
    /// MatMul with a fused epilogue, produced by `fuse_rewrite`:
    /// `out = act(alpha · (a @ b) + bias) + residual`, numpy-batched like MatMul.
    /// Inputs: `a`, `b` (`[.., N, K]` when `trans_b`), then the per-column bias
    /// (length N) if `has_bias`, then the residual (same layout as the output)
    /// if `has_residual`. `out_shape` is the planned output (it may be a reshape
    /// of the `[batch.., M, N]` product).
    FusedMatMul {
        alpha: f32,
        trans_b: bool,
        has_bias: bool,
        has_residual: bool,
        act: Activation,
        out_shape: Vec<usize>,
    },
    /// LayerNorm over one axis: `(x - mean) / sqrt(var + epsilon) · gamma + beta`.
    /// Inputs: `x`, `gamma`, `beta` (each `axis`-length).
    LayerNorm {
        axis: usize,
        epsilon: f32,
    },
}

/// Pointwise activation applied in a fused epilogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Activation {
    None,
    /// Exact (erf) GELU: `0.5 · x · (1 + erf(x / sqrt 2))`.
    Gelu,
    Relu,
}

impl Activation {
    pub(crate) fn code(self) -> u32 {
        match self {
            Activation::None => 0,
            Activation::Gelu => 1,
            Activation::Relu => 2,
        }
    }
}

impl PlannedOpKind {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            PlannedOpKind::Conv2d(_) => "Conv2d",
            PlannedOpKind::WinogradInputTransform => "WinogradInputTransform",
            PlannedOpKind::WinogradBatchedGemm => "WinogradBatchedGemm",
            PlannedOpKind::WinogradOutputTransform { .. } => "WinogradOutputTransform",
            PlannedOpKind::Elementwise(ElementwiseKind::Add) => "Add",
            PlannedOpKind::Elementwise(ElementwiseKind::Mul) => "Mul",
            PlannedOpKind::Elementwise(ElementwiseKind::Sub) => "Sub",
            PlannedOpKind::Elementwise(ElementwiseKind::Div) => "Div",
            PlannedOpKind::Elementwise(ElementwiseKind::Max) => "Max",
            PlannedOpKind::Unary(UnaryKind::Identity) => "Identity",
            PlannedOpKind::Unary(UnaryKind::Sigmoid) => "Sigmoid",
            PlannedOpKind::Unary(UnaryKind::Relu) => "Relu",
            PlannedOpKind::Unary(UnaryKind::HardSwish) => "HardSwish",
            PlannedOpKind::Unary(UnaryKind::HardSigmoid { .. }) => "HardSigmoid",
            PlannedOpKind::Unary(UnaryKind::Sqrt) => "Sqrt",
            PlannedOpKind::Unary(UnaryKind::Erf) => "Erf",
            PlannedOpKind::Unary(UnaryKind::Exp) => "Exp",
            PlannedOpKind::Unary(UnaryKind::Sin) => "Sin",
            PlannedOpKind::Unary(UnaryKind::Cos) => "Cos",
            PlannedOpKind::Unary(UnaryKind::Pow { .. }) => "Pow",
            PlannedOpKind::Concat { .. } => "Concat",
            PlannedOpKind::Split { .. } => "Split",
            PlannedOpKind::Slice { .. } => "Slice",
            PlannedOpKind::Reshape { .. } => "Reshape",
            PlannedOpKind::Transpose { .. } => "Transpose",
            PlannedOpKind::MaxPool2d(_) => "MaxPool2d",
            PlannedOpKind::AvgPool2d(_) => "AvgPool2d",
            PlannedOpKind::ResizeNearest { .. } => "ResizeNearest",
            PlannedOpKind::ResizeLinear { .. } => "ResizeLinear",
            PlannedOpKind::Pad { .. } => "Pad",
            PlannedOpKind::PRelu => "PRelu",
            PlannedOpKind::Unsqueeze { .. } => "Unsqueeze",
            PlannedOpKind::Squeeze { .. } => "Squeeze",
            PlannedOpKind::GridSample { .. } => "GridSample",
            PlannedOpKind::MatMul => "MatMul",
            PlannedOpKind::Gemm { .. } => "Gemm",
            PlannedOpKind::Softmax { .. } => "Softmax",
            PlannedOpKind::GlobalAveragePool => "GlobalAveragePool",
            PlannedOpKind::CumSum { .. } => "CumSum",
            PlannedOpKind::ReduceSum { .. } => "ReduceSum",
            PlannedOpKind::SpaceToDepth { .. } => "SpaceToDepth",
            PlannedOpKind::DepthToSpace { .. } => "DepthToSpace",
            PlannedOpKind::ReduceMax { .. } => "ReduceMax",
            PlannedOpKind::Tile { .. } => "Tile",
            PlannedOpKind::Expand { .. } => "Expand",
            PlannedOpKind::Where => "Where",
            PlannedOpKind::Gather { .. } => "Gather",
            PlannedOpKind::GatherElements { .. } => "GatherElements",
            PlannedOpKind::TopK { .. } => "TopK",
            PlannedOpKind::FusedMatMul { .. } => "FusedMatMul",
            PlannedOpKind::LayerNorm { .. } => "LayerNorm",
        }
    }
}

#[derive(Debug)]
pub(crate) enum PadMode {
    Constant,
    Reflect,
}

#[derive(Debug, Clone)]
pub(crate) struct Conv2dPlan {
    pub(crate) group: i64,
    pub(crate) pads: [i64; 4],
    pub(crate) strides: [i64; 2],
    pub(crate) dilations: [i64; 2],
    pub(crate) kernel_shape: [i64; 2],
}

#[derive(Debug)]
pub(crate) struct Pool2dPlan {
    pub(crate) pads: [i64; 4],
    pub(crate) strides: [i64; 2],
    pub(crate) dilations: [i64; 2],
    pub(crate) kernel_shape: [i64; 2],
}

#[derive(Debug)]
pub(crate) enum ElementwiseKind {
    Add,
    Mul,
    Sub,
    Div,
    Max,
}

#[derive(Debug)]
pub(crate) enum UnaryKind {
    Identity,
    Sigmoid,
    Relu,
    HardSwish,
    HardSigmoid {
        alpha: f32,
        beta: f32,
    },
    Sqrt,
    /// Gauss error function. PP-OCRv6 builds GELU out of it as
    /// `x * 0.5 * (1 + Erf(x / sqrt(2)))`, so it appears once per activation.
    Erf,
    /// Power with a constant scalar exponent (sign-correct for integer exponents).
    Pow {
        exponent: f32,
    },
    Exp,
    Sin,
    Cos,
}
