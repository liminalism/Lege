//! Opening the document as it stood at one revision's end.
//!
//! A revision's bytes are a prefix of the file —
//! [`LogicalRevisionFacts::end`](crate::document::LogicalRevisionFacts::end)
//! is one past its `%%EOF` — so historical replay needs no special parser:
//! the ordinary [`DocumentSnapshot::open`](pdf_document::DocumentSnapshot::open)
//! path runs over a [`PrefixSource`](pdf_source::PrefixSource) truncated at
//! that end. What this module adds is the honest edge: a revision whose end
//! was never established (low boundary confidence) refuses to replay rather
//! than opening a guessed prefix.

use std::fmt;
use std::sync::Arc;

use pdf_document::{DocumentLimits, DocumentSnapshot};
use pdf_source::{OwnedBytesSource, PdfSource, PrefixSource};

use crate::document::LogicalRevisionFacts;

/// Why a historical revision could not be replayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    /// The revision's end is not established — no valid `startxref` names
    /// its section — so there is no prefix known to hold exactly that
    /// save. Replaying would mean opening a guessed byte range.
    NoEstablishedEnd {
        /// Chronological index of the revision that cannot be replayed.
        index: u32,
    },
    /// The prefix at the revision's end did not open.
    OpenFailed {
        /// The open error, rendered.
        error: String,
    },
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplayError::NoEstablishedEnd { index } => write!(
                f,
                "revision {index} has no established end and cannot be replayed"
            ),
            ReplayError::OpenFailed { error } => {
                write!(f, "revision prefix did not open: {error}")
            }
        }
    }
}

impl std::error::Error for ReplayError {}

/// Open the document as it stood at the end of `revision`.
///
/// `bytes` is the whole file; only the prefix ending at the revision's
/// established end is opened, through the same
/// [`DocumentSnapshot::open`](pdf_document::DocumentSnapshot::open) path —
/// and the same recovery behaviour — as a full-file open. `password` is
/// tried against an encrypted document exactly as
/// [`audit_bytes`](crate::intake::audit_bytes) tries it.
///
/// A revision with no established end ([`None`] under
/// [`LogicalRevisionFacts::end`](crate::document::LogicalRevisionFacts::end))
/// is refused with [`ReplayError::NoEstablishedEnd`]: without its
/// `startxref` anchor the save's byte range is unknown, and a following
/// `%%EOF` alone is never a boundary.
pub fn open_revision(
    bytes: &[u8],
    revision: &LogicalRevisionFacts,
    password: Option<&str>,
) -> Result<DocumentSnapshot, ReplayError> {
    let Some(end) = revision.end else {
        return Err(ReplayError::NoEstablishedEnd {
            index: revision.index,
        });
    };
    let full: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes.to_vec()));
    let prefix: Arc<dyn PdfSource> = Arc::new(PrefixSource::new(full, end));
    DocumentSnapshot::open_with_password(prefix, DocumentLimits::default(), password).map_err(
        |err| ReplayError::OpenFailed {
            error: err.to_string(),
        },
    )
}
