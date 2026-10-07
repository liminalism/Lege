//! Turning a file into a report.
//!
//! This is the seam between the renderer's neutral fact layer and this crate's
//! interpretation. `pdf-read` is asked what the document says; everything here
//! is translation, plus the one judgement that cannot be deferred: how much of
//! the document this run could actually see.

use std::sync::Arc;

use pdf_document::{DocumentLimits, DocumentSnapshot};
use pdf_read::{CompileStatus, DocumentReport, OpenOutcome};
use pdf_source::{OwnedBytesSource, PdfSource};

use crate::document::{
    AnnotationFacts, AnnotationIdentityFacts, ByteRangeSpan, ByteRangeValidity, CompileOutcome,
    DocMdpPermission, DocMdpPolicyFacts, DocumentFacts, EditMarkerFacts, EncryptionFacts,
    FeatureFacts, FieldMdpAction, FieldMdpPolicyFacts, FileIdentity, FontFacts, HeaderFacts,
    ImageFacts, LineageFacts, LinearizationFacts, MetadataFacts, OpenState, PageEditMarkers,
    PageFacts, PageRepresentation, ParseAssessment, PendingSection, RevisionHistoryFacts,
    SignatureFacts, SignatureInventoryFacts, XmpHistoryEventFacts, XmpLineageFacts,
};
use crate::findings::AuditEvidence;
use crate::report::{AnalysisMode, AuditReport, Availability, UnavailableReason};
use crate::representation;

/// Audit the bytes of a PDF.
///
/// Never fails and never panics: a file that is not a PDF at all still yields a
/// report, one whose parse assessment says so. That is a requirement rather
/// than politeness — a forensic tool that refuses its most damaged inputs is
/// useless on exactly the documents that warrant a look.
///
/// `password` is tried against an encrypted document. Without one, an encrypted
/// document is still audited, structurally, and the report says which happened.
///
/// A full audit runs the findings engine over the translated facts:
/// signature byte ranges, CMS digests, post-signature DocMDP/FieldMDP policy,
/// revision text diffs, annotation overlays, and flattened occlusion become
/// findings, and the disposition follows from them (see
/// [`decide_disposition`](crate::findings::decide_disposition)).
/// A structure-only run collects no findings — every detector finding
/// needs full evidence — and the verdict follows from the pending
/// inventory.
#[must_use]
pub fn audit_bytes(bytes: &[u8], password: Option<&str>) -> AuditReport {
    audit_bytes_with_evidence(bytes, password).0
}

/// Audit the bytes of a PDF, plus the evidence behind the findings.
///
/// The same report [`audit_bytes`] returns, with the
/// [`AuditEvidence`] bundle the findings were drawn from: the detector
/// outcomes and their renders, for PNG export via
/// [`write_evidence_pngs`](crate::findings::write_evidence_pngs).
///
/// Like [`audit_bytes`], never fails and never panics.
#[must_use]
pub fn audit_bytes_with_evidence(
    bytes: &[u8],
    password: Option<&str>,
) -> (AuditReport, AuditEvidence) {
    let identity = FileIdentity {
        sha256: hex_sha256(bytes),
        size_bytes: bytes.len() as u64,
    };

    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes.to_vec()));
    let report =
        pdf_read::examine_with_password(Arc::clone(&source), DocumentLimits::default(), password);

    let facts = translate(bytes, &report, &source, password, identity);
    let mode = analysis_mode(&facts);

    let evidence = crate::findings::collect_evidence(bytes, password, &facts, mode);
    let findings = evidence.findings();
    let disposition = crate::findings::decide_disposition(&findings, &facts.pending);
    (
        AuditReport::new_with_facts(mode, disposition, findings, facts),
        evidence,
    )
}

/// How much of the document this run could see.
///
/// A document that opened was fully readable. One that did not is
/// structure-only — whether because it is encrypted and no usable password was
/// supplied, or because it is too damaged to open. The distinction between
/// those two causes is carried per-section by [`UnavailableReason`]; the mode
/// records only how far the run got.
fn analysis_mode(facts: &DocumentFacts) -> AnalysisMode {
    match facts.parse.state {
        OpenState::Clean | OpenState::Recovered => AnalysisMode::Full,
        OpenState::Failed { .. } => AnalysisMode::StructureOnly,
    }
}

fn translate(
    bytes: &[u8],
    report: &DocumentReport,
    source: &Arc<dyn PdfSource>,
    password: Option<&str>,
    identity: FileIdentity,
) -> DocumentFacts {
    let (state, recovery_events) = match &report.open {
        OpenOutcome::Ok => (OpenState::Clean, Vec::new()),
        OpenOutcome::Recovered { how } => (OpenState::Recovered, how.clone()),
        OpenOutcome::Failed { error } => (
            OpenState::Failed {
                error: error.clone(),
            },
            Vec::new(),
        ),
    };

    let parse = ParseAssessment {
        state,
        recovery_events,
        xref_rebuilt: report.xref.rebuilt,
        xref_section_count: report.xref.revision_count,
    };

    let opened = !matches!(parse.state, OpenState::Failed { .. });

    let encryption = report.encryption.as_ref().map(|e| EncryptionFacts {
        version: e.version,
        revision: e.revision,
        method: e.method.clone(),
        user_password_empty: e.user_password_empty,
    });

    // Why this run could not read the rest. An encrypted document that failed
    // to open failed *because* of the encryption; anything else that failed to
    // open is simply unparseable.
    let blocked_by = if encryption.is_some() {
        UnavailableReason::Encrypted
    } else {
        UnavailableReason::Unparseable
    };
    let blocked_detail = match &parse.state {
        OpenState::Failed { error } => error.clone(),
        _ => String::new(),
    };

    let encryption = if opened || encryption.is_some() {
        // Either the document opened, or the structural retry read /Encrypt —
        // both are determined answers.
        Availability::available(encryption)
    } else {
        // The document did not open and no /Encrypt was read, which means the
        // structural layer itself was unreadable. "Not encrypted" would be a
        // claim this run cannot make.
        Availability::unavailable(UnavailableReason::Unparseable, blocked_detail.clone())
    };

    // The remaining sections need an open document. Note that the fact layer
    // does fill in `uses_object_streams` on the failed path, but a FeatureFacts
    // with one real flag and five defaulted ones would read as five determined
    // negatives, so the section is reported unavailable as a whole.
    let (features, annotations, pages, fonts, images, edit_markers) = if opened {
        (
            Availability::available(FeatureFacts {
                has_acroform: report.features.has_acroform,
                has_xfa: report.features.has_xfa,
                has_javascript: report.features.has_javascript,
                has_outlines: report.features.has_outlines,
                has_optional_content: report.features.has_optional_content,
                uses_object_streams: report.features.uses_object_streams,
                has_embedded_files: report.features.has_embedded_files,
            }),
            Availability::available(AnnotationFacts {
                count_per_subtype: report.annotations.count_per_subtype.clone(),
                annotations: report
                    .annotations
                    .annotations
                    .iter()
                    .map(translate_annotation)
                    .collect(),
            }),
            Availability::available(report.pages.iter().map(translate_page).collect()),
            Availability::available(report.fonts.iter().map(translate_font).collect()),
            Availability::available(report.images.iter().map(translate_image).collect()),
            Availability::available(translate_edit_markers(&report.pages)),
        )
    } else {
        (
            Availability::unavailable(blocked_by.clone(), blocked_detail.clone()),
            Availability::unavailable(blocked_by.clone(), blocked_detail.clone()),
            Availability::unavailable(blocked_by.clone(), blocked_detail.clone()),
            Availability::unavailable(blocked_by.clone(), blocked_detail.clone()),
            Availability::unavailable(blocked_by.clone(), blocked_detail.clone()),
            Availability::unavailable(blocked_by.clone(), blocked_detail.clone()),
        )
    };

    // The header needs no decryption, so it survives a failed open. Its
    // absence is not a gap in the audit — it is why the document failed.
    let header = match &report.header {
        Some(h) => Availability::available(HeaderFacts {
            major: h.major,
            minor: h.minor,
            header_offset: h.header_offset,
        }),
        None => Availability::unavailable(
            UnavailableReason::Unparseable,
            "no %PDF header found in the file",
        ),
    };

    // Linearization needs only the structural layer — the first object's
    // offset and body — so it is determined whenever the structure loaded,
    // which is exactly when the header is present.
    let linearization = if report.header.is_some() {
        Availability::available(LinearizationFacts {
            linearized: report.linearized,
        })
    } else {
        Availability::unavailable(UnavailableReason::Unparseable, blocked_detail.clone())
    };

    // Revision grouping needs the walked sections plus the raw bytes for
    // `startxref`/`%%EOF` boundary pairing. A rebuilt xref is the honest
    // gap: a full object scan has no section structure, so original revision
    // membership cannot be established — and a zero-revision claim would be
    // a fabrication.
    let revisions = if report.xref.sections.is_empty() {
        Availability::unavailable(blocked_by.clone(), blocked_detail.clone())
    } else if report.xref.rebuilt {
        Availability::unavailable(
            UnavailableReason::Ambiguous,
            "cross-reference table rebuilt by full object scan; original revision \
             membership cannot be established",
        )
    } else {
        let mut history =
            crate::revisions::group_revisions(&report.xref.sections, bytes, report.linearized);
        // Object-change classification over the same grouping: structural
        // in every mode, with roles from the resolved document when the
        // run opened one. The snapshot is opened here — rather than
        // reused from the fact layer, which does not hand its own out —
        // and only when the fact layer's identical open succeeded.
        let snapshot = if opened {
            DocumentSnapshot::open_with_password(
                Arc::clone(source),
                DocumentLimits::default(),
                password,
            )
            .ok()
        } else {
            None
        };
        crate::changes::attach_changes(
            &mut history,
            crate::changes::ChangeInputs {
                sections: &report.xref.sections,
                bytes,
                header_offset: report.header.as_ref().map(|header| header.header_offset),
                linearized: report.linearized,
                fonts: &report.fonts,
                images: &report.images,
                snapshot: snapshot.as_ref(),
            },
        );
        Availability::available(history)
    };

    // The trailer /ID pair is never encrypted; /Info is.
    let lineage = LineageFacts {
        id_permanent: report.identity.id_permanent.clone(),
        id_changing: report.identity.id_changing.clone(),
        metadata: if opened {
            Availability::available(MetadataFacts {
                info: report.identity.info.clone(),
                has_xmp_metadata: report.identity.has_xmp_metadata,
                xmp: report.identity.xmp.as_ref().map(translate_xmp),
            })
        } else {
            Availability::unavailable(blocked_by.clone(), blocked_detail.clone())
        },
    };

    let mut signatures = if opened {
        let history = revisions.value();
        Availability::available(SignatureInventoryFacts {
            signatures: report
                .signatures
                .signatures
                .iter()
                .map(|signature| translate_signature(signature, history, bytes))
                .collect(),
            unsigned_fields: report.signatures.unsigned_fields,
        })
    } else {
        Availability::unavailable(blocked_by, blocked_detail)
    };
    if let Availability::Available { value } = &mut signatures {
        crate::digest::attach_crypto(bytes, password, value.signatures.as_mut_slice());
    }

    DocumentFacts {
        identity,
        parse,
        revisions,
        header,
        linearization,
        lineage,
        encryption,
        features,
        annotations,
        pages,
        fonts,
        images,
        edit_markers,
        signatures,
        pending: pending_sections(),
    }
}

fn translate_xmp(xmp: &pdf_read::XmpLineage) -> XmpLineageFacts {
    XmpLineageFacts {
        document_id: xmp.document_id.clone(),
        instance_id: xmp.instance_id.clone(),
        original_document_id: xmp.original_document_id.clone(),
        derived_from_document_id: xmp.derived_from_document_id.clone(),
        derived_from_instance_id: xmp.derived_from_instance_id.clone(),
        history: xmp
            .history
            .iter()
            .map(|e| XmpHistoryEventFacts {
                action: e.action.clone(),
                when: e.when.clone(),
                software_agent: e.software_agent.clone(),
                changed: e.changed.clone(),
                instance_id: e.instance_id.clone(),
            })
            .collect(),
    }
}

fn translate_annotation(annot: &pdf_read::AnnotationRecord) -> AnnotationIdentityFacts {
    AnnotationIdentityFacts {
        page: annot.page,
        object: annot.object.map(|id| id.to_string()),
        subtype: annot.subtype.clone(),
        contents: annot.contents.clone(),
        name: annot.name.clone(),
        modified: annot.modified.clone(),
        creation_date: annot.creation_date.clone(),
        author: annot.author.clone(),
        subject: annot.subject.clone(),
        intent: annot.intent.clone(),
    }
}

fn translate_signature(
    sig: &pdf_read::SignatureInfo,
    history: Option<&RevisionHistoryFacts>,
    bytes: &[u8],
) -> SignatureFacts {
    let covered_ranges: Vec<ByteRangeSpan> = sig
        .covered_ranges
        .iter()
        .map(|span| ByteRangeSpan {
            offset: span.offset,
            length: span.length,
        })
        .collect();
    let revision = history.and_then(|history| {
        crate::byterange::introducing_revision(
            sig.field_object.map(|id| id.number),
            sig.value_object.map(|id| id.number),
            history,
        )
    });
    let revision_end = revision.and_then(|index| {
        history.and_then(|history| {
            history
                .revisions
                .iter()
                .find(|revision| revision.index == index)
                .and_then(|revision| revision.end)
        })
    });
    SignatureFacts {
        field_name: sig.field_name.clone(),
        field_object: sig.field_object.map(|id| id.to_string()),
        value_object: sig.value_object.map(|id| id.to_string()),
        filter: sig.filter.clone(),
        sub_filter: sig.sub_filter.clone(),
        signed_at: sig.signed_at.clone(),
        byte_range: sig.byte_range.clone(),
        covers_whole_file: sig.covers_whole_file,
        byte_range_validity: match sig.byte_range_validity {
            pdf_read::ByteRangeValidity::Valid => ByteRangeValidity::Valid,
            pdf_read::ByteRangeValidity::GapContaining => ByteRangeValidity::GapContaining,
            pdf_read::ByteRangeValidity::Truncated => ByteRangeValidity::Truncated,
            pdf_read::ByteRangeValidity::Overlapping => ByteRangeValidity::Overlapping,
            pdf_read::ByteRangeValidity::Malformed => ByteRangeValidity::Malformed,
        },
        ends_at_revision_end: crate::byterange::ends_at_revision_end(
            &covered_ranges,
            revision_end,
            bytes,
        ),
        covered_ranges,
        revision,
        docmdp: sig.docmdp.as_ref().map(|policy| DocMdpPolicyFacts {
            p: policy.p,
            permission: policy.p.and_then(DocMdpPermission::from_p),
        }),
        fieldmdp: sig.fieldmdp.as_ref().map(|policy| FieldMdpPolicyFacts {
            p: policy.p,
            action: policy.action.as_deref().and_then(FieldMdpAction::from_name),
            fields: policy.fields.clone(),
        }),
        crypto: None,
    }
}

fn translate_page(page: &pdf_read::PageStatus) -> PageFacts {
    let compile = match &page.compile {
        CompileStatus::Ok { op_count } => CompileOutcome::Ok {
            op_count: *op_count,
        },
        CompileStatus::Degraded { op_count, detail } => CompileOutcome::Degraded {
            op_count: *op_count,
            detail: detail.clone(),
        },
        CompileStatus::Failed { error } => CompileOutcome::Failed {
            error: error.clone(),
        },
    };
    match &page.metrics {
        Some(metrics) => PageFacts {
            index: page.index,
            media_box_present: page.media_box_present,
            compile,
            representation: representation::classify(metrics),
            text_runs: metrics.text_runs,
            visible_text_runs: metrics.visible_text_runs,
            fonts: metrics.fonts,
            images: metrics.images,
            path_paints: metrics.path_paints,
            image_coverage_bps: metrics.image_coverage_bps,
            effective_dpi: metrics.effective_dpi,
        },
        None => PageFacts {
            index: page.index,
            media_box_present: page.media_box_present,
            compile,
            representation: PageRepresentation::Unknown,
            text_runs: 0,
            visible_text_runs: 0,
            fonts: 0,
            images: 0,
            path_paints: 0,
            image_coverage_bps: 0,
            effective_dpi: None,
        },
    }
}

fn translate_font(font: &pdf_read::FontRecord) -> FontFacts {
    FontFacts {
        resource_name: font.resource_name.clone(),
        object: font.object.map(|id| id.to_string()),
        subtype: font.subtype.clone(),
        base_font: font.base_font.clone(),
        embedded: font.embedded,
    }
}

fn translate_image(image: &pdf_read::ImageRecord) -> ImageFacts {
    ImageFacts {
        object: image.object.map(|id| id.to_string()),
        width: image.width,
        height: image.height,
        bits_per_component: image.bits_per_component,
        is_mask: image.is_mask,
        filters: image.filters.clone(),
    }
}

/// The fact layer reports every marked-content tag neutrally; this is the
/// interpretation step that decides which of them count as edit markers:
/// the `/TouchUp_*` family. Sparse: only pages carrying at least one
/// appear.
fn translate_edit_markers(pages: &[pdf_read::PageStatus]) -> Vec<PageEditMarkers> {
    pages
        .iter()
        .filter_map(|page| {
            let markers: Vec<EditMarkerFacts> = page
                .marked_content
                .iter()
                .filter(|span| span.tag.starts_with("TouchUp_"))
                .map(|span| EditMarkerFacts {
                    tag: span.tag.clone(),
                    open_offset: span.open_offset,
                    close_offset: span.close_offset,
                    bbox: span.bbox.and_then(millipoints),
                })
                .collect();
            if markers.is_empty() {
                None
            } else {
                Some(PageEditMarkers {
                    page: page.index,
                    markers,
                })
            }
        })
        .collect()
}

/// User-space points to thousandths of a point. `None` for a non-finite
/// box, which the fact layer never emits but this crate will not bless
/// into the report either.
fn millipoints(bbox: [f64; 4]) -> Option<[i64; 4]> {
    if !bbox.iter().all(|v| v.is_finite()) {
        return None;
    }
    Some([
        (bbox[0] * 1000.0).round() as i64,
        (bbox[1] * 1000.0).round() as i64,
        (bbox[2] * 1000.0).round() as i64,
        (bbox[3] * 1000.0).round() as i64,
    ])
}

/// The inventory this build does not reach yet.
///
/// Every entry is a section the milestones name that no layer computes.
/// Keeping the list in the report — rather than in a comment — means a
/// consumer can tell a partial audit from a complete one, and means the list
/// has to be shortened rather than quietly forgotten.
fn pending_sections() -> Vec<PendingSection> {
    vec![PendingSection::new(
        "signature_cryptography",
        "CMS digest verification is reported per signature, but certificate trust \
         and timestamp-token verification are explicitly unevaluated follow-up work \
         per the crypto-crate decision",
    )]
}

fn hex_sha256(bytes: &[u8]) -> String {
    crate::hashes::hex_sha256(bytes)
}
