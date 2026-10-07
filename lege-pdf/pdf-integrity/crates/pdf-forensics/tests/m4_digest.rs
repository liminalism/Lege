//! M4 CMS digest layer: a real self-signed RSA signer over the ByteRange,
//! verified digest on the known-good file and a mismatch on one flipped byte
//! — while structural coverage keeps reporting correctly.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap is a failed test"
)]

use cms::builder::{SignedDataBuilder, SignerInfoBuilder};
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::signed_data::{EncapsulatedContentInfo, SignerIdentifier};
use der::Encode;
use pdf_forensics::{
    AuditReport, ByteRangeValidity, DigestValidity, Disposition, SIGNATURE_DIGEST_MISMATCH,
    SignatureFacts, TimeValidity, TimestampStatus, TrustStatus, audit_bytes,
    post_signature_changes,
};
use pdf_test_support::builder::{PdfBuilder, layout, signed_layout};
use rand::rngs::OsRng;
use rsa::{RsaPrivateKey, RsaPublicKey, pkcs1v15::SigningKey};
use sha2_10::{Digest, Sha256};
use std::str::FromStr;
use std::time::Duration;
use x509_cert::builder::{Builder, CertificateBuilder, Profile};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::SubjectPublicKeyInfoOwned;
use x509_cert::time::Validity;

/// Hex hole size for the `/Contents`: 8192 bytes of DER room, zero-padded.
const CONTENTS_HEX_LEN: usize = 16384;

/// Audit `bytes` and require the report to satisfy the schema's own rules.
fn audit(bytes: &[u8]) -> AuditReport {
    let report = audit_bytes(bytes, None);
    report.validate().unwrap();
    report
}

/// The single signature of a signed fixture.
fn signature(report: &AuditReport) -> &SignatureFacts {
    let signatures = report.document.signatures.value().unwrap();
    assert_eq!(signatures.signatures.len(), 1);
    &signatures.signatures[0]
}

/// Lowercase hex of bytes.
fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// One-page document with a `/FT /Sig` field whose `/V` carries a ByteRange
/// placeholder and a `/Contents` hex hole: the shape a signer prepares.
fn add_signed_body(builder: &mut PdfBuilder, contents_hex_len: usize) {
    builder.add_object(
        layout::CATALOG,
        &format!(
            "<</Type/Catalog/Pages {} 0 R/AcroForm {} 0 R>>",
            layout::PAGES,
            signed_layout::ACROFORM
        ),
    );
    builder.add_object(
        layout::PAGES,
        &format!(
            "<</Type/Pages/Kids[{} 0 R]/Count 1/MediaBox[0 0 612 792]>>",
            layout::page(0)
        ),
    );
    builder.add_object(
        layout::page(0),
        &format!(
            "<</Type/Page/Parent {} 0 R/Contents {} 0 R/Annots [{} 0 R]\
             /Resources<</Font<</F1 {} 0 R>>>>>>",
            layout::PAGES,
            layout::content(0),
            signed_layout::FIELD,
            layout::FONT
        ),
    );
    builder.add_stream(layout::content(0), "", &layout::content_bytes(0));
    builder.add_object(
        layout::FONT,
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>",
    );
    builder.add_object(
        signed_layout::FIELD,
        &format!(
            "<</Type/Annot/Subtype/Widget/FT/Sig/T(Sig1)/Rect[0 0 10 10]/V {} 0 R>>",
            signed_layout::VALUE
        ),
    );
    let contents = "0".repeat(contents_hex_len);
    builder.add_object(
        signed_layout::VALUE,
        &format!(
            "<</Type/Sig/Filter/Adobe.PPKLite/SubFilter/adbe.pkcs7.detached\
             /M(D:20241201120000Z)/ByteRange[0000000000 0000000000 0000000000 0000000000]\
             /Contents<{contents}>>>"
        ),
    );
    builder.add_object(
        signed_layout::ACROFORM,
        &format!("<</Fields [{} 0 R]>>", signed_layout::FIELD),
    );
}

/// Absolute span of the `/Contents` hex hole, brackets included.
fn contents_hole(bytes: &[u8]) -> (usize, usize) {
    let open = bytes
        .windows(10)
        .position(|w| w == b"/Contents<")
        .map(|pos| pos + 9)
        .unwrap();
    let close = bytes[open..]
        .iter()
        .position(|b| *b == b'>')
        .map(|rel| open + rel + 1)
        .unwrap();
    (open, close)
}

/// Patch the ByteRange placeholder with `range`, zero-padded to ten digits.
fn patch_range(bytes: &mut [u8], range: [u64; 4]) {
    let needle = "[0000000000 0000000000 0000000000 0000000000]";
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle.as_bytes())
        .unwrap();
    let mut pos = at + 1;
    for value in range {
        let text = format!("{value:010}");
        bytes[pos..pos + 10].copy_from_slice(text.as_bytes());
        pos += 11;
    }
}

/// A signed one-page document with a real CMS: self-signed RSA-2048 cert,
/// SHA-256 over the ByteRange, correct `/ByteRange`, CMS in `/Contents`.
fn signed_real() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_signed_body(&mut builder, CONTENTS_HEX_LEN);
    builder.finish_classic_xref("/Root 1 0 R");
    let mut bytes = builder.into_bytes();
    let (hole_open, hole_close) = contents_hole(&bytes);
    let end = bytes.len() as u64;
    patch_range(
        &mut bytes,
        [
            0,
            hole_open as u64,
            hole_close as u64,
            end - hole_close as u64,
        ],
    );
    let mut hasher = Sha256::new();
    hasher.update(&bytes[..hole_open]);
    hasher.update(&bytes[hole_close..]);
    let byte_range_digest = hasher.finalize().to_vec();

    let mut rng = OsRng;
    let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
    let signing_key = SigningKey::<Sha256>::new(private_key.clone());
    let public_key = RsaPublicKey::from(&private_key);
    let pub_key_info = SubjectPublicKeyInfoOwned::from_key(public_key).unwrap();
    let validity = Validity::from_now(Duration::from_secs(365 * 24 * 3600)).unwrap();
    let subject = Name::from_str("CN=pdf-forensics-test").unwrap();
    let cert_builder = CertificateBuilder::new(
        Profile::Root,
        SerialNumber::from(42u32),
        validity,
        subject,
        pub_key_info,
        &signing_key,
    )
    .unwrap();
    let cert = cert_builder.build::<rsa::pkcs1v15::Signature>().unwrap();

    let content = EncapsulatedContentInfo {
        econtent_type: const_oid::db::rfc5911::ID_DATA,
        econtent: None,
    };
    let digest_alg = x509_cert::spki::AlgorithmIdentifierOwned {
        oid: const_oid::db::rfc5912::ID_SHA_256,
        parameters: None,
    };
    let sid = SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
        issuer: cert.tbs_certificate.issuer.clone(),
        serial_number: cert.tbs_certificate.serial_number.clone(),
    });
    let signer_info = SignerInfoBuilder::new(
        &signing_key,
        sid,
        digest_alg.clone(),
        &content,
        Some(&byte_range_digest),
    )
    .unwrap();
    let mut sd_builder = SignedDataBuilder::new(&content);
    sd_builder.add_digest_algorithm(digest_alg).unwrap();
    sd_builder
        .add_certificate(CertificateChoices::Certificate(cert))
        .unwrap();
    sd_builder
        .add_signer_info::<SigningKey<Sha256>, rsa::pkcs1v15::Signature>(signer_info)
        .unwrap();
    let cms_der = sd_builder.build().unwrap().to_der().unwrap();

    let cms_hex = hex_of(&cms_der);
    assert!(
        cms_hex.len() <= CONTENTS_HEX_LEN,
        "CMS ({} hex) exceeds hole ({CONTENTS_HEX_LEN})",
        cms_hex.len()
    );
    let padded = format!("{cms_hex}{}", "0".repeat(CONTENTS_HEX_LEN - cms_hex.len()));
    bytes[hole_open + 1..hole_close - 1].copy_from_slice(padded.as_bytes());
    bytes
}

#[test]
fn known_good_verifies_with_explicit_unevaluated_trust() {
    let bytes = signed_real();
    let report = audit(&bytes);

    let sig = signature(&report);
    assert_eq!(sig.field_name.as_deref(), Some("Sig1"));
    assert_eq!(sig.byte_range_validity, ByteRangeValidity::Valid);
    assert!(sig.covers_whole_file);
    assert_eq!(sig.revision, Some(0));
    assert_eq!(sig.ends_at_revision_end, Some(true));

    let crypto = sig.crypto.as_ref().unwrap();
    assert_eq!(crypto.digest_validity, DigestValidity::Valid);
    assert_eq!(crypto.digest_algorithm.as_deref(), Some("sha256"));
    assert!(crypto.computed_digest.is_some());
    assert_eq!(crypto.computed_digest, crypto.expected_digest);
    let signer = crypto.signer.as_ref().unwrap();
    assert!(
        signer.identity.contains("pdf-forensics-test"),
        "{}",
        signer.identity
    );
    assert_eq!(crypto.certificates.len(), 1);
    assert!(crypto.issuer_chaining.complete);
    assert_eq!(crypto.trust, TrustStatus::Unevaluated);
    assert_eq!(crypto.time_validity, TimeValidity::Valid);
    assert!(!crypto.timestamp_present);
    assert_eq!(crypto.timestamp_status, TimestampStatus::Unevaluated);

    // The five statuses stay separate: no later revision, no policy finding,
    // no digest finding — and the verdict follows from the pending inventory.
    assert!(post_signature_changes(&report.document, sig).is_empty());
    assert!(report.findings.is_empty());
    assert_eq!(report.disposition, Disposition::Inconclusive);

    let json: serde_json::Value = serde_json::to_value(&report).unwrap();
    let crypto_json = &json["document"]["signatures"]["value"]["signatures"][0]["crypto"];
    assert_eq!(crypto_json["digest_validity"], "valid");
    assert_eq!(crypto_json["trust"], "unevaluated");
    assert_eq!(crypto_json["time_validity"], "valid");
    assert_eq!(crypto_json["timestamp_status"], "unevaluated");
}

#[test]
fn flipped_byte_fails_digest_while_structural_stays_valid() {
    let mut bytes = signed_real();
    let needle = b"(Page 0)";
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap();
    bytes[at + 1] ^= 0x01;
    let report = audit(&bytes);

    let sig = signature(&report);
    assert_eq!(sig.byte_range_validity, ByteRangeValidity::Valid);
    assert!(sig.covers_whole_file);
    assert_eq!(sig.revision, Some(0));
    assert_eq!(sig.ends_at_revision_end, Some(true));

    let crypto = sig.crypto.as_ref().unwrap();
    assert_eq!(crypto.digest_validity, DigestValidity::Mismatch);
    assert_ne!(crypto.computed_digest, crypto.expected_digest);
    assert_eq!(crypto.trust, TrustStatus::Unevaluated);
    assert_eq!(crypto.time_validity, TimeValidity::Valid);

    let mismatches: Vec<_> = report
        .findings
        .iter()
        .filter(|finding| finding.code == SIGNATURE_DIGEST_MISMATCH)
        .collect();
    assert_eq!(mismatches.len(), 1);
    assert!(
        mismatches[0].explanation.contains("Sig1"),
        "{}",
        mismatches[0].explanation
    );
    assert_eq!(report.disposition, Disposition::IntegrityFailure);
}
