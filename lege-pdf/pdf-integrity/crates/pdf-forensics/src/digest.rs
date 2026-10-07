//! CMS digest verification over signature `/ByteRange`s (M4 crypto layer).
//!
//! Layer one ([`crate::byterange`]) asks whether a signature's claimed ranges
//! are well-formed and end where their own save ends. This layer asks the next
//! question: does the content those ranges cover hash to the digest the CMS
//! `SignedData` in `/Contents` claims? It parses the CMS with the RustCrypto
//! stack (`cms` for `SignedData`, `x509-cert` for certificates, `sha2` for the
//! digest — see `@audit.decision.crypto-crate-choice`), reports signer
//! identity, certificate validity windows, and structural issuer chaining, and
//! keeps the five signature statuses separate:
//!
//! * digest valid ([`DigestValidity`]) — computed here,
//! * certificate trusted ([`TrustStatus`]) — explicitly [`TrustStatus::Unevaluated`]
//!   in M4; trust-anchor validation is follow-up work, never an implied claim,
//! * certificate time-valid ([`TimeValidity`]) — the signer certificate's
//!   window evaluated at verification time,
//! * later revision present — [`crate::docmdp::post_signature_changes`],
//! * later change permitted — [`crate::docmdp::check_post_signature_policies`].
//!
//! Timestamp tokens are reported by presence only ([`SignatureDigestFacts::timestamp_present`]);
//! TSA-chain verification is explicitly [`TimestampStatus::Unevaluated`] per the
//! decision. RSA/ECDSA signature math over the signed attributes is likewise
//! out of M4: a [`DigestValidity::Valid`] digest means the covered bytes match
//! what the CMS claims, not that the CMS itself is authentic. A
//! [`DigestValidity::Mismatch`] proves the covered bytes changed after the CMS
//! was built.
//!
//! Verification never fails and never panics: unparseable CMS, unsupported
//! digest algorithms, missing attributes, and out-of-bounds ranges all degrade
//! to [`DigestValidity::Unestablished`] (or
//! [`DigestValidity::UnsupportedAlgorithm`]) with a reason, and trust and
//! timestamp stay explicitly unevaluated. Only a proven mismatch becomes a
//! [`SIGNATURE_DIGEST_MISMATCH`] finding.

use std::sync::Arc;
use std::time::SystemTime;

use cms::cert::CertificateChoices;
use cms::content_info::ContentInfo;
use cms::signed_data::SignerIdentifier;
use const_oid::db::{rfc5911, rfc5912};
use der::{Decode, Encode};
use pdf_document::{DocumentLimits, DocumentSnapshot, ParseContext};
use pdf_object::{ObjectId, PdfObject};
use pdf_source::{OwnedBytesSource, PdfSource};
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::document::{ByteRangeValidity, DocumentFacts, SignatureFacts};
use crate::report::{AnalysisMode, EvidenceStrength, Finding, FindingCode};

/// The CMS content digest over a signature's `/ByteRange` does not match the
/// `messageDigest` the CMS claims: the covered bytes changed after signing.
pub const SIGNATURE_DIGEST_MISMATCH: FindingCode = FindingCode::new("SIGNATURE_DIGEST_MISMATCH");

/// OID of the `id-aa-signatureTimeStampToken` unsigned attribute (RFC 3161
/// token carried in CMS). Compared as a string because `const-oid` 0.9 carries
/// no symbol for it.
const TIMESTAMP_TOKEN_OID: &str = "1.2.840.113549.1.9.16.2.14";

/// Why trust is never evaluated in M4, carried on every digest report.
const TRUST_DETAIL: &str = "M4 does not evaluate certificate trust anchors; chain \
    validation to a trust store is follow-up work per the crypto-crate decision, and no \
    claim about trust is made here.";

/// Why timestamp verification is never evaluated in M4, with presence noted.
const TIMESTAMP_ABSENT_DETAIL: &str = "no signature timestamp token is present; TSA-chain \
    verification is follow-up work per the crypto-crate decision and is not attempted.";

const TIMESTAMP_PRESENT_DETAIL: &str = "a signature timestamp token is present; the TSA \
    chain is not verified in M4 per the crypto-crate decision, so no claim about the \
    signing time is made here.";

/// Whether the CMS content digest matches the `/ByteRange` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestValidity {
    /// The computed digest equals the CMS `messageDigest`.
    Valid,
    /// The computed digest differs: the covered bytes changed.
    Mismatch,
    /// No claim is made: unparseable CMS, missing attributes, malformed
    /// ranges, or another gap spelled out in
    /// [`SignatureDigestFacts::digest_detail`].
    Unestablished,
    /// The signer's digest algorithm is not SHA-256/384/512. The CMS is
    /// parsed and identity is still reported; only the comparison is skipped.
    UnsupportedAlgorithm,
}

/// Whether the signer certificate's validity window covers verification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeValidity {
    /// Verification time falls within `[not_before, not_after]`.
    Valid,
    /// Verification time is past `not_after`.
    Expired,
    /// Verification time precedes `not_before`.
    NotYetValid,
    /// No claim is made: no signer certificate was matched, or the window
    /// could not be read.
    Unestablished,
}

/// Whether the signer certificate chains to a trust anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustStatus {
    /// Explicitly not evaluated in M4: chain validation to a trust store is
    /// follow-up work, and this status must never be read as trusted.
    Unevaluated,
}

/// Whether a present timestamp token verifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestampStatus {
    /// Explicitly not evaluated in M4: TSA-chain verification is follow-up
    /// work, whether or not a token is present.
    Unevaluated,
}

/// One X.509 certificate carried in the CMS, with its validity window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CertificateFacts {
    /// Subject distinguished name (RFC 4514), e.g. `CN=test`.
    pub subject: String,
    /// Issuer distinguished name (RFC 4514).
    pub issuer: String,
    /// Serial number, uppercase hex with colon separators.
    pub serial: String,
    /// `notBefore`, as rendered by the X.509 time type.
    pub not_before: String,
    /// `notAfter`, as rendered by the X.509 time type.
    pub not_after: String,
    /// Whether verification time falls within the window.
    pub time_validity: TimeValidity,
}

/// One certificate's place in the structural issuer chain.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IssuerLinkFacts {
    /// Subject distinguished name (RFC 4514).
    pub subject: String,
    /// Issuer distinguished name (RFC 4514).
    pub issuer: String,
    /// Whether issuer equals subject (self-issued, typically self-signed).
    pub self_issued: bool,
}

/// Structural issuer chaining: distinguished-name linkage only, never
/// signature verification. A `complete` chain means every certificate's issuer
/// matches some carried subject (or is self-issued); it says nothing about
/// whether any signature verifies or any anchor is trusted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IssuerChainFacts {
    /// One link per carried X.509 certificate, in CMS order.
    pub links: Vec<IssuerLinkFacts>,
    /// Whether every link's issuer resolves within the carried set.
    pub complete: bool,
    /// Why the chain is incomplete, when it is.
    pub detail: Option<String>,
}

/// The single CMS signer: identity plus the digest it claims.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SignerDigestFacts {
    /// Who signed: the matched certificate's subject, or the raw
    /// issuer-and-serial / key-identifier when no certificate matched.
    pub identity: String,
    /// Issuer distinguished name from an issuer-and-serial sid, when present.
    pub issuer: Option<String>,
    /// Serial from an issuer-and-serial sid, when present.
    pub serial: Option<String>,
    /// Digest algorithm name (`sha256`, `sha384`, `sha512`), when recognized.
    pub digest_algorithm: Option<String>,
    /// Claimed signing time from the signed attributes, verbatim rendering.
    /// A claim by the signer, not an observation; unused by validity.
    pub signing_time: Option<String>,
}

/// M4 crypto report for one signature: digest comparison, signer identity,
/// certificate windows, and structural chaining — with trust and timestamp
/// verification as explicit unevaluated states.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SignatureDigestFacts {
    /// Whether the computed `/ByteRange` digest matches the CMS claim.
    pub digest_validity: DigestValidity,
    /// Why the digest is unestablished or unsupported, when it is.
    pub digest_detail: Option<String>,
    /// Digest algorithm name (`sha256`, `sha384`, `sha512`), when recognized.
    pub digest_algorithm: Option<String>,
    /// Lowercase hex digest computed over the covered ranges, when computed.
    pub computed_digest: Option<String>,
    /// Lowercase hex `messageDigest` claimed by the CMS, when read.
    pub expected_digest: Option<String>,
    /// The single signer, when exactly one `SignerInfo` is carried.
    pub signer: Option<SignerDigestFacts>,
    /// Every X.509 certificate carried in the CMS, in CMS order.
    pub certificates: Vec<CertificateFacts>,
    /// Structural issuer linkage over [`Self::certificates`].
    pub issuer_chaining: IssuerChainFacts,
    /// Always [`TrustStatus::Unevaluated`] in M4.
    pub trust: TrustStatus,
    /// Why trust is unevaluated.
    pub trust_detail: String,
    /// Time validity of the matched signer certificate, or
    /// [`TimeValidity::Unestablished`] when no certificate matched.
    pub time_validity: TimeValidity,
    /// Whether any signer carries a timestamp-token unsigned attribute.
    pub timestamp_present: bool,
    /// Always [`TimestampStatus::Unevaluated`] in M4.
    pub timestamp_status: TimestampStatus,
    /// Presence-aware note that no timestamp claim is made.
    pub timestamp_detail: String,
}

/// Verify one signature's CMS digest over the original PDF bytes.
///
/// `covered` and `byte_range_validity` come from the signature's structural
/// facts; `cms_der` is the raw `/Contents` bytes (zero padding tolerated).
/// Ranges are re-checked against `pdf_bytes` here rather than trusted from the
/// tolerant parse, per the strict-parse decision.
///
/// Never fails and never panics: every gap degrades to
/// [`DigestValidity::Unestablished`] or
/// [`DigestValidity::UnsupportedAlgorithm`] with a reason.
#[must_use]
pub fn verify_signature_digest(
    pdf_bytes: &[u8],
    covered: &[crate::document::ByteRangeSpan],
    byte_range_validity: ByteRangeValidity,
    cms_der: &[u8],
) -> SignatureDigestFacts {
    let mut facts = SignatureDigestFacts {
        digest_validity: DigestValidity::Unestablished,
        digest_detail: None,
        digest_algorithm: None,
        computed_digest: None,
        expected_digest: None,
        signer: None,
        certificates: Vec::new(),
        issuer_chaining: IssuerChainFacts {
            links: Vec::new(),
            complete: false,
            detail: Some("no certificates were read".to_owned()),
        },
        trust: TrustStatus::Unevaluated,
        trust_detail: TRUST_DETAIL.to_owned(),
        time_validity: TimeValidity::Unestablished,
        timestamp_present: false,
        timestamp_status: TimestampStatus::Unevaluated,
        timestamp_detail: TIMESTAMP_ABSENT_DETAIL.to_owned(),
    };
    if !matches!(
        byte_range_validity,
        ByteRangeValidity::Valid | ByteRangeValidity::GapContaining
    ) {
        facts.digest_detail =
            Some("digest not attempted: the /ByteRange is not well-formed".to_owned());
        return facts;
    }
    if covered.is_empty() {
        facts.digest_detail =
            Some("digest not attempted: the signature claims no covered runs".to_owned());
        return facts;
    }
    let signed_data = match parse_signed_data(cms_der) {
        Some(data) => data,
        None => {
            facts.digest_detail =
                Some("CMS SignedData could not be parsed from /Contents".to_owned());
            return facts;
        }
    };
    let now = SystemTime::now();
    facts.certificates = certificates_of(&signed_data, now);
    facts.issuer_chaining = chain_of(&signed_data);
    facts.timestamp_present = has_timestamp(&signed_data);
    facts.timestamp_detail = if facts.timestamp_present {
        TIMESTAMP_PRESENT_DETAIL.to_owned()
    } else {
        TIMESTAMP_ABSENT_DETAIL.to_owned()
    };
    let signers: Vec<&cms::signed_data::SignerInfo> = signed_data.signer_infos.0.iter().collect();
    let single = match signers.as_slice() {
        [one] => *one,
        [] => {
            facts.digest_detail = Some("CMS carries no SignerInfo".to_owned());
            return facts;
        }
        many => {
            facts.digest_detail = Some(format!(
                "CMS carries {} SignerInfos; M4 evaluates single-signer only",
                many.len()
            ));
            return facts;
        }
    };
    let algorithm = digest_name(&single.digest_alg.oid);
    facts.digest_algorithm = algorithm.map(str::to_owned);
    facts.signer = Some(signer_of(
        single,
        &signed_data,
        now,
        &mut facts.time_validity,
    ));
    let Some(algorithm) = algorithm else {
        facts.digest_validity = DigestValidity::UnsupportedAlgorithm;
        facts.digest_detail = Some(format!(
            "unsupported digest algorithm {}",
            single.digest_alg.oid
        ));
        return facts;
    };
    let computed = match hash_covered(pdf_bytes, covered, algorithm) {
        Some(digest) => digest,
        None => {
            facts.digest_detail = Some("a covered run reaches past the end of the file".to_owned());
            return facts;
        }
    };
    let expected = match message_digest_of(single) {
        Some(digest) => digest,
        None => {
            facts.digest_detail =
                Some("no messageDigest attribute in the signed attributes".to_owned());
            return facts;
        }
    };
    facts.computed_digest = Some(hex_of(&computed));
    facts.expected_digest = Some(hex_of(&expected));
    if computed == expected {
        facts.digest_validity = DigestValidity::Valid;
    } else {
        facts.digest_validity = DigestValidity::Mismatch;
        facts.digest_detail = Some("computed digest differs from the CMS claim".to_owned());
    }
    facts
}

/// Attach M4 crypto reports to every signed signature.
///
/// Opens the document exactly as the audit opened it, extracts each
/// signature's `/Contents` via its field/value object identity, and verifies
/// the digest over the original bytes. Signatures whose bytes cannot be
/// reached keep `crypto` as `None`; unparseable CMS becomes an explicit
/// [`DigestValidity::Unestablished`] report, never a silent gap.
///
/// Never fails and never panics.
pub(crate) fn attach_crypto(
    pdf_bytes: &[u8],
    password: Option<&str>,
    signatures: &mut [SignatureFacts],
) {
    if signatures.is_empty() {
        return;
    }
    let Some(snapshot) = open_snapshot(pdf_bytes, password) else {
        return;
    };
    let mut ctx = ParseContext::new();
    for signature in signatures.iter_mut() {
        let Some(cms) = contents_of(
            &snapshot,
            signature.field_object.as_deref(),
            signature.value_object.as_deref(),
            &mut ctx,
        ) else {
            continue;
        };
        signature.crypto = Some(verify_signature_digest(
            pdf_bytes,
            signature.covered_ranges.as_slice(),
            signature.byte_range_validity,
            cms.as_slice(),
        ));
    }
}

/// Emit a [`SIGNATURE_DIGEST_MISMATCH`] finding for every signature whose
/// digest provably mismatches.
///
/// Unestablished and unsupported digests are facts, not findings: only a
/// computed comparison that disagrees becomes a cryptographic fact. Never
/// fails and never panics.
#[must_use]
pub fn check_signature_digests(facts: &DocumentFacts) -> Vec<Finding> {
    let Some(inventory) = facts.signatures.value() else {
        return Vec::new();
    };
    let rests_on_recovered = facts.parse.rests_on_recovered_structure();
    let mut findings = Vec::new();
    for signature in &inventory.signatures {
        let Some(crypto) = signature.crypto.as_ref() else {
            continue;
        };
        if crypto.digest_validity != DigestValidity::Mismatch {
            continue;
        }
        let name = signature.field_name.as_deref().unwrap_or("(unnamed)");
        let algorithm = crypto
            .digest_algorithm
            .as_deref()
            .unwrap_or("unknown digest");
        findings.push(Finding {
            code: SIGNATURE_DIGEST_MISMATCH,
            strength: EvidenceStrength::CryptographicFact,
            explanation: format!(
                "signature '{name}' CMS digest mismatch ({algorithm}): the /ByteRange bytes \
                 hash to {} but the CMS claims {}",
                crypto.computed_digest.as_deref().unwrap_or("uncomputed"),
                crypto.expected_digest.as_deref().unwrap_or("unread"),
            ),
            limitation: None,
            requires_mode: AnalysisMode::Full,
            rests_on_recovered_structure: rests_on_recovered,
        });
    }
    findings
}

/// Parse `/Contents` as CMS `SignedData`, tolerating the zero padding a PDF
/// signer leaves after the DER in an oversized `/Contents` hole.
fn parse_signed_data(cms_der: &[u8]) -> Option<cms::signed_data::SignedData> {
    if let Some(data) = parse_exact(cms_der) {
        return Some(data);
    }
    let trimmed = trim_padding(cms_der);
    if trimmed.len() == cms_der.len() {
        return None;
    }
    parse_exact(trimmed)
}

/// Parse exactly these bytes as `ContentInfo` wrapping `SignedData`.
fn parse_exact(bytes: &[u8]) -> Option<cms::signed_data::SignedData> {
    if bytes.is_empty() {
        return None;
    }
    let info = ContentInfo::from_der(bytes).ok()?;
    if info.content_type != rfc5911::ID_SIGNED_DATA {
        return None;
    }
    info.content.decode_as().ok()
}

/// Strip trailing zero bytes left by an oversized `/Contents` hole.
fn trim_padding(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 {
        let byte = bytes.get(end - 1).copied().unwrap_or(1);
        if byte != 0 {
            break;
        }
        end -= 1;
    }
    bytes.get(..end).unwrap_or(&[])
}

/// Digest algorithm name for a CMS digest OID, when M4 supports it.
fn digest_name(oid: &der::asn1::ObjectIdentifier) -> Option<&'static str> {
    if *oid == rfc5912::ID_SHA_256 {
        Some("sha256")
    } else if *oid == rfc5912::ID_SHA_384 {
        Some("sha384")
    } else if *oid == rfc5912::ID_SHA_512 {
        Some("sha512")
    } else {
        None
    }
}

/// Hash the covered runs in `/ByteRange` order. `None` when any run escapes
/// the file: the independent bounds re-check the strict-parse decision
/// requires.
fn hash_covered(
    pdf_bytes: &[u8],
    covered: &[crate::document::ByteRangeSpan],
    algorithm: &str,
) -> Option<Vec<u8>> {
    let mut spans: Vec<&[u8]> = Vec::new();
    for span in covered {
        let start = usize::try_from(span.offset).ok()?;
        let len = usize::try_from(span.length).ok()?;
        let end = start.checked_add(len)?;
        spans.push(pdf_bytes.get(start..end)?);
    }
    if algorithm == "sha256" {
        let mut hasher = Sha256::new();
        for span in spans {
            hasher.update(span);
        }
        Some(hasher.finalize().to_vec())
    } else if algorithm == "sha384" {
        let mut hasher = Sha384::new();
        for span in spans {
            hasher.update(span);
        }
        Some(hasher.finalize().to_vec())
    } else if algorithm == "sha512" {
        let mut hasher = Sha512::new();
        for span in spans {
            hasher.update(span);
        }
        Some(hasher.finalize().to_vec())
    } else {
        None
    }
}

/// The `messageDigest` octets from the signed attributes, when present and
/// well-formed.
fn message_digest_of(signer: &cms::signed_data::SignerInfo) -> Option<Vec<u8>> {
    let attrs = signer.signed_attrs.as_ref()?;
    let attr = attrs
        .iter()
        .find(|attr| attr.oid == rfc5911::ID_MESSAGE_DIGEST)?;
    let value = attr.values.as_ref().first()?;
    let digest: der::asn1::OctetString = value.decode_as().ok()?;
    Some(digest.as_bytes().to_vec())
}

/// Whether any signer carries a timestamp-token unsigned attribute.
fn has_timestamp(signed_data: &cms::signed_data::SignedData) -> bool {
    signed_data.signer_infos.0.iter().any(|signer| {
        signer.unsigned_attrs.as_ref().is_some_and(|attrs| {
            attrs
                .iter()
                .any(|attr| attr.oid.to_string() == TIMESTAMP_TOKEN_OID)
        })
    })
}

/// Every carried X.509 certificate with its window evaluated at `now`.
fn certificates_of(
    signed_data: &cms::signed_data::SignedData,
    now: SystemTime,
) -> Vec<CertificateFacts> {
    let Some(set) = signed_data.certificates.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for choice in set.0.iter() {
        let CertificateChoices::Certificate(cert) = choice else {
            continue;
        };
        let tbs = &cert.tbs_certificate;
        out.push(CertificateFacts {
            subject: tbs.subject.to_string(),
            issuer: tbs.issuer.to_string(),
            serial: tbs.serial_number.to_string(),
            not_before: tbs.validity.not_before.to_string(),
            not_after: tbs.validity.not_after.to_string(),
            time_validity: window_validity(&tbs.validity, now),
        });
    }
    out
}

/// Structural issuer chaining over the carried certificates.
fn chain_of(signed_data: &cms::signed_data::SignedData) -> IssuerChainFacts {
    let Some(set) = signed_data.certificates.as_ref() else {
        return IssuerChainFacts {
            links: Vec::new(),
            complete: false,
            detail: Some("CMS carries no certificates".to_owned()),
        };
    };
    let mut subjects: Vec<String> = Vec::new();
    let mut links: Vec<IssuerLinkFacts> = Vec::new();
    for choice in set.0.iter() {
        let CertificateChoices::Certificate(cert) = choice else {
            continue;
        };
        let subject = cert.tbs_certificate.subject.to_string();
        let issuer = cert.tbs_certificate.issuer.to_string();
        subjects.push(subject.clone());
        links.push(IssuerLinkFacts {
            subject,
            issuer: issuer.clone(),
            self_issued: cert.tbs_certificate.issuer == cert.tbs_certificate.subject,
        });
    }
    if links.is_empty() {
        return IssuerChainFacts {
            links,
            complete: false,
            detail: Some("CMS carries no X.509 certificates".to_owned()),
        };
    }
    let mut missing: Option<String> = None;
    for link in &links {
        if link.self_issued {
            continue;
        }
        if !subjects.iter().any(|subject| subject == &link.issuer) {
            missing = Some(link.issuer.clone());
            break;
        }
    }
    match missing {
        None => IssuerChainFacts {
            links,
            complete: true,
            detail: None,
        },
        Some(issuer) => IssuerChainFacts {
            links,
            complete: false,
            detail: Some(format!("issuer {issuer} has no matching carried subject")),
        },
    }
}

/// The single signer's identity, plus its certificate window when matched.
///
/// Writes the matched window into `time_validity`, leaving it unestablished
/// when no certificate matches the sid.
fn signer_of(
    signer: &cms::signed_data::SignerInfo,
    signed_data: &cms::signed_data::SignedData,
    now: SystemTime,
    time_validity: &mut TimeValidity,
) -> SignerDigestFacts {
    let signing_time = signer
        .signed_attrs
        .as_ref()
        .and_then(|attrs| {
            attrs
                .iter()
                .find(|attr| attr.oid == rfc5911::ID_SIGNING_TIME)
        })
        .and_then(|attr| attr.values.as_ref().first())
        .and_then(|value| value.to_der().ok())
        .and_then(|der| x509_cert::time::Time::from_der(der.as_slice()).ok())
        .map(|time| time.to_string());
    match &signer.sid {
        SignerIdentifier::IssuerAndSerialNumber(sid) => {
            let issuer = sid.issuer.to_string();
            let serial = sid.serial_number.to_string();
            let matched = find_certificate(signed_data, sid);
            if let Some(cert) = matched {
                *time_validity = window_validity(&cert.tbs_certificate.validity, now);
                SignerDigestFacts {
                    identity: cert.tbs_certificate.subject.to_string(),
                    issuer: Some(issuer),
                    serial: Some(serial),
                    digest_algorithm: digest_name(&signer.digest_alg.oid).map(str::to_owned),
                    signing_time,
                }
            } else {
                SignerDigestFacts {
                    identity: format!(
                        "issuer {issuer} serial {serial} (no matching carried certificate)"
                    ),
                    issuer: Some(issuer),
                    serial: Some(serial),
                    digest_algorithm: digest_name(&signer.digest_alg.oid).map(str::to_owned),
                    signing_time,
                }
            }
        }
        SignerIdentifier::SubjectKeyIdentifier(ski) => SignerDigestFacts {
            identity: format!(
                "subject key identifier {} (sid matching out of M4)",
                hex_of(ski.0.as_bytes())
            ),
            issuer: None,
            serial: None,
            digest_algorithm: digest_name(&signer.digest_alg.oid).map(str::to_owned),
            signing_time,
        },
    }
}

/// The carried certificate matching an issuer-and-serial sid, when present.
fn find_certificate<'a>(
    signed_data: &'a cms::signed_data::SignedData,
    sid: &cms::cert::IssuerAndSerialNumber,
) -> Option<&'a x509_cert::Certificate> {
    let set = signed_data.certificates.as_ref()?;
    for choice in set.0.iter() {
        let CertificateChoices::Certificate(cert) = choice else {
            continue;
        };
        if cert.tbs_certificate.issuer == sid.issuer
            && cert.tbs_certificate.serial_number == sid.serial_number
        {
            return Some(cert);
        }
    }
    None
}

/// Whether `now` falls within a certificate validity window.
fn window_validity(validity: &x509_cert::time::Validity, now: SystemTime) -> TimeValidity {
    let before: SystemTime = validity.not_before.into();
    let after: SystemTime = validity.not_after.into();
    if now < before {
        TimeValidity::NotYetValid
    } else if now > after {
        TimeValidity::Expired
    } else {
        TimeValidity::Valid
    }
}

/// Lowercase hex of bytes, without pulling in a hex crate.
fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Reopen the document exactly as the audit opened it.
fn open_snapshot(pdf_bytes: &[u8], password: Option<&str>) -> Option<DocumentSnapshot> {
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(pdf_bytes.to_vec()));
    DocumentSnapshot::open_with_password(source, DocumentLimits::default(), password).ok()
}

/// Raw `/Contents` bytes for one signature, via its object identity.
///
/// Prefers the `/V` value object; falls back to the field's `/V` entry when
/// the value is direct. `None` when nothing resolves to a byte string.
fn contents_of(
    snapshot: &DocumentSnapshot,
    field_object: Option<&str>,
    value_object: Option<&str>,
    ctx: &mut ParseContext,
) -> Option<Vec<u8>> {
    if let Some(id) = value_object.and_then(parse_object_id)
        && let Ok(value) = snapshot.objects().resolve(snapshot, id, ctx)
        && let Some(dict) = value.as_dict()
        && let Some(bytes) = contents_from_dict(snapshot, dict, ctx)
    {
        return Some(bytes);
    }
    let id = field_object.and_then(parse_object_id)?;
    let field = snapshot.objects().resolve(snapshot, id, ctx).ok()?;
    let field = field.as_dict()?;
    let key = snapshot.names().lookup(b"V")?;
    let entry = field.get(key)?;
    let value = match entry {
        PdfObject::Reference(id) => snapshot.objects().resolve(snapshot, *id, ctx).ok()?,
        other => Arc::new(other.clone()),
    };
    let dict = value.as_dict()?;
    contents_from_dict(snapshot, dict, ctx)
}

/// `/Contents` from a signature value dictionary, resolved when indirect.
fn contents_from_dict(
    snapshot: &DocumentSnapshot,
    dict: &pdf_object::Dictionary,
    ctx: &mut ParseContext,
) -> Option<Vec<u8>> {
    let key = snapshot.names().lookup(b"Contents")?;
    let entry = dict.get(key)?;
    let resolved = match entry {
        PdfObject::Reference(id) => snapshot.objects().resolve(snapshot, *id, ctx).ok()?,
        other => Arc::new(other.clone()),
    };
    match resolved.as_ref() {
        PdfObject::String(text) => Some(text.as_bytes().to_vec()),
        _ => None,
    }
}

/// Parse an `N G R` identity into an object id.
fn parse_object_id(identity: &str) -> Option<ObjectId> {
    let mut parts = identity.split_whitespace();
    let number: u32 = parts.next()?.parse().ok()?;
    let generation: u16 = parts.next()?.parse().ok()?;
    Some(ObjectId::new(number, generation))
}
