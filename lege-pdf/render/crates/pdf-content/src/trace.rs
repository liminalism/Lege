//! Audit-mode operator provenance: the [`PageTrace`] sidecar.
//!
//! A [`PageTrace`] is recorded parallel to [`SemanticPage`](crate::SemanticPage)
//! `ops` — one [`OpTrace`] per emitted semantic op, in the same order — naming,
//! for each op, the content operator that produced it, the source stream
//! object, the nested-object stack (form XObjects, patterns, Type 3 CharProcs,
//! soft masks, annotation appearances), the operator's byte span within its
//! decoded stream, and the [`PaintOrigin`].
//!
//! It exists for integrity auditing (occlusion analysis needs per-operator
//! evidence: alpha, blend modes and soft masks, which the coarse attribution
//! map deliberately ignores). It is produced only when the audit entry point
//! ([`PageCompiler::compile_semantic_traced`](crate::PageCompiler::compile_semantic_traced))
//! asks for it; default compilation neither allocates nor observes it.

use pdf_object::ObjectId;
use pdf_page_ir::PaintOrigin;

/// What kind of object a [`TraceFrame`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TraceFrameKind {
    /// A form XObject invoked by `Do` (any nesting depth).
    FormXObject,
    /// An annotation's `/AP` appearance stream, or a synthesized markup
    /// appearance for an annotation that carries no `/AP /N`.
    AnnotationAppearance,
    /// A tiling pattern (PatternType 1) resolved while producing the op.
    TilingPattern,
    /// A shading pattern (PatternType 2) resolved while producing the op.
    ShadingPattern,
    /// A shading dictionary painted by the `sh` operator.
    Shading,
    /// A Type 3 font's glyph procedure (CharProc).
    Type3Glyph,
    /// A soft-mask (`/SMask`) group content stream.
    SoftMask,
}

impl TraceFrameKind {
    /// The paint origin this frame executes content under, or `None` for
    /// frames that name a resolved *resource* (patterns, shadings) rather
    /// than an executing content stream.
    pub fn paint_origin(self) -> Option<PaintOrigin> {
        match self {
            Self::FormXObject => Some(PaintOrigin::FormXObject),
            Self::AnnotationAppearance => Some(PaintOrigin::AnnotationAppearance),
            Self::TilingPattern | Self::ShadingPattern | Self::Shading => None,
            Self::Type3Glyph => Some(PaintOrigin::Type3Glyph),
            Self::SoftMask => Some(PaintOrigin::SoftMaskContent),
        }
    }

    /// Stable name for report output.
    pub fn name(self) -> &'static str {
        match self {
            Self::FormXObject => "form-xobject",
            Self::AnnotationAppearance => "annotation-appearance",
            Self::TilingPattern => "tiling-pattern",
            Self::ShadingPattern => "shading-pattern",
            Self::Shading => "shading",
            Self::Type3Glyph => "type3-glyph",
            Self::SoftMask => "soft-mask",
        }
    }
}

/// One level of a traced op's nested-object stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceFrame {
    /// What kind of object this level is.
    pub kind: TraceFrameKind,
    /// The backing indirect object, when the stream was reached through one
    /// (`None` for direct streams and synthesized appearances).
    pub object: Option<ObjectId>,
}

/// Provenance of one emitted [`SemanticOp`](crate::SemanticOp).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpTrace {
    /// Index of the op in [`SemanticPage::ops`](crate::SemanticPage::ops).
    /// Always equal to this entry's position in
    /// [`PageTrace::entries`]; stored so a detached entry stays attributable.
    pub op_index: usize,
    /// Execution sequence number of the content operator that produced the
    /// op: 1-based, counting every operator keyword and inline image in
    /// execution order across the page and all nested streams it invokes.
    /// Dropped (malformed) operators still consume sequence numbers.
    /// Synthesized ops reuse the sequence of the most recently executed
    /// operator (0 when none has executed yet).
    pub content_op_index: u64,
    /// The triggering operator keyword as written (e.g. `b"f"`, `b"Do"`),
    /// `b"BI"` for an inline image, or empty when
    /// [`synthesized`](Self::synthesized).
    pub operator: Vec<u8>,
    /// Whether the op was emitted outside any content operator (form-invocation
    /// wrappers are *not* synthesized — they attribute to their `Do` — but
    /// annotation-appearance wrappers, balancing restores, synthesized markup
    /// appearances, and `Begin`/`EndPaintOrigin` markers are).
    pub synthesized: bool,
    /// The source stream object whose decoded bytes the operator was read
    /// from: a page `/Contents` stream, a form, CharProc, or mask-group
    /// stream. `None` for direct streams and synthesized ops.
    pub stream: Option<ObjectId>,
    /// The nested-object stack that produced the op, outermost first; empty
    /// for ops the page content stream produced directly. A trailing
    /// pattern/shading frame names a resource resolved while producing the
    /// op rather than an executing stream (see
    /// [`TraceFrameKind::paint_origin`]).
    pub stack: Vec<TraceFrame>,
    /// Byte span of the operator keyword within the decoded source stream
    /// ([`stream`](Self::stream)) — stream-relative even when the page
    /// `/Contents` concatenates several streams. An inline image spans its
    /// whole `BI`…`EI` extent; a synthesized op spans `(0, 0)`.
    pub span: (usize, usize),
    /// The innermost executing content stream: [`PaintOrigin::PageContent`]
    /// for page-level ops, otherwise the innermost
    /// form/annotation/Type-3/soft-mask frame.
    pub origin: PaintOrigin,
}

/// The audit sidecar: per-op provenance parallel to a page's semantic ops.
///
/// `entries[i]` describes `ops[i]` of the [`SemanticPage`](crate::SemanticPage)
/// it was compiled alongside. Produced only by the audit entry point
/// ([`PageCompiler::compile_semantic_traced`](crate::PageCompiler::compile_semantic_traced)).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageTrace {
    /// One entry per semantic op, in op order.
    pub entries: Vec<OpTrace>,
}

impl PageTrace {
    /// Number of traced ops (equal to the semantic page's op count).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the traced page emitted no ops.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The provenance of the op at `op_index`, or `None` when out of range.
    pub fn for_op(&self, op_index: usize) -> Option<&OpTrace> {
        self.entries.get(op_index)
    }
}

/// One decoded stream's extent within a page's concatenated `/Contents`
/// bytes: the audit map from a page-level offset back to its source stream
/// object and stream-relative span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContentSegment {
    /// The stream object, or `None` for a direct (non-indirect) stream.
    pub object: Option<ObjectId>,
    /// Start of the decoded stream within the concatenated bytes (after the
    /// `'\n'` separator the gatherer inserts between streams).
    pub start: usize,
    /// Length of the decoded stream in bytes.
    pub len: usize,
}

/// Provenance of the content operator currently being dispatched: the
/// interpreter stamps each op it emits while dispatching with this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TraceCurrent {
    /// The operator keyword as written (`b"BI"` for an inline image).
    pub operator: Vec<u8>,
    /// Start of the operator keyword within the current decoded stream
    /// (the `BI` start for an inline image).
    pub span_start: usize,
    /// End of the operator keyword — just past `EI` for an inline image.
    pub span_end: usize,
    /// Content-operator execution sequence number (see
    /// [`OpTrace::content_op_index`]).
    pub seq: u64,
}
