//! Document open helpers shared by CLI commands and the serve cache.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use pdf_document::{DocumentLimits, DocumentSnapshot};
use pdf_source::{OwnedBytesSource, PdfSource};

/// Identity used to cache snapshots across serve requests.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DocumentIdentity {
    pub path: PathBuf,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl DocumentIdentity {
    pub fn from_path(path: &Path) -> Result<Self> {
        let meta =
            std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
        if meta.is_dir() {
            anyhow::bail!("{} is a directory, not a PDF file", path.display());
        }
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        Ok(Self {
            path: plain_path(canonical),
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }

    pub fn display_path(&self) -> String {
        self.path.display().to_string()
    }
}

/// Drop the Windows verbatim prefix `canonicalize` adds (`\\?\C:\…`,
/// `\\?\UNC\server\share\…`) when the ordinary spelling names the same
/// file. The verbatim form is correct but is not what the caller passed, does
/// not paste into most tools, and doubles every backslash in JSON.
///
/// It is kept where it is load-bearing: past the legacy length limit, or
/// when a component would be reinterpreted without it (a trailing dot or
/// space, a `.`/`..` component, a forward slash).
fn plain_path(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    let plain = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        let mut bytes = rest.bytes();
        let drive = bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
            && bytes.next() == Some(b':')
            && bytes.next() == Some(b'\\');
        if !drive {
            return path;
        }
        rest.to_owned()
    } else {
        return path;
    };
    let reinterpreted = plain.len() >= 248
        || plain.contains('/')
        || plain
            .split('\\')
            .any(|part| part == "." || part == ".." || part.ends_with('.') || part.ends_with(' '));
    if reinterpreted {
        path
    } else {
        PathBuf::from(plain)
    }
}

/// Open a snapshot from an already-loaded source, without touching disk.
///
/// `open_document`, `open_with_identity`, and `load_source` all bottom out
/// here; a caller that already holds a loaded source (e.g. after running it
/// through `pdf_read::examine*`) should call this directly instead of
/// re-reading the file a second time.
pub fn open_from_source(
    identity: &DocumentIdentity,
    source: Arc<dyn PdfSource>,
    password: Option<&str>,
) -> Result<DocumentSnapshot> {
    DocumentSnapshot::open_with_password(source, DocumentLimits::default(), password)
        .with_context(|| format!("opening {}", identity.path.display()))
}

/// Open a PDF from `path` with an optional password.
pub fn open_document(
    path: &Path,
    password: Option<&str>,
) -> Result<(DocumentIdentity, DocumentSnapshot)> {
    let (identity, source) = load_source(path)?;
    let snapshot = open_from_source(&identity, source, password)?;
    Ok((identity, snapshot))
}

/// Files at least this large are mapped rather than read by a one-shot
/// command. Reading a page of a 70 MB scan should not cost a 70 MB copy.
const MAP_THRESHOLD: u64 = 1 << 20;

/// The bytes of a local file for a command that runs once and exits.
///
/// Large files are memory-mapped, so only the parts a command touches are
/// paged in; a file that cannot be mapped is read whole. The long-lived
/// services do not come through here: they hold documents open across
/// requests, and a mapping held that long would keep the file from being
/// replaced (Windows) or fault if it were truncated.
fn one_shot_source(identity: &DocumentIdentity) -> Result<Arc<dyn PdfSource>> {
    if identity.len >= MAP_THRESHOLD
        && let Ok(mapped) = pdf_source::MmapSource::open(&identity.path)
    {
        return Ok(Arc::new(mapped));
    }
    let bytes = std::fs::read(&identity.path)
        .with_context(|| format!("reading {}", identity.path.display()))?;
    Ok(Arc::new(OwnedBytesSource::new(bytes)))
}

/// Open from already-known identity (serve cache miss path).
pub fn open_with_identity(
    identity: &DocumentIdentity,
    password: Option<&str>,
) -> Result<DocumentSnapshot> {
    let bytes = std::fs::read(&identity.path)
        .with_context(|| format!("reading {}", identity.path.display()))?;
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes));
    open_from_source(identity, source, password)
}

/// Load a document's bytes for a one-shot command, without parsing them.
pub fn load_source(path: &Path) -> Result<(DocumentIdentity, Arc<dyn PdfSource>)> {
    let identity = DocumentIdentity::from_path(path)?;
    let source = one_shot_source(&identity)?;
    Ok((identity, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefixes_are_dropped_only_when_the_plain_path_is_equivalent() {
        let plain = |text: &str| plain_path(PathBuf::from(text));
        assert_eq!(
            plain(r"\\?\D:\books\a.pdf"),
            PathBuf::from(r"D:\books\a.pdf")
        );
        assert_eq!(
            plain(r"\\?\UNC\server\share\a.pdf"),
            PathBuf::from(r"\\server\share\a.pdf")
        );
        // Already plain, or not a Windows path at all.
        assert_eq!(plain(r"D:\books\a.pdf"), PathBuf::from(r"D:\books\a.pdf"));
        assert_eq!(plain("/home/dk/a.pdf"), PathBuf::from("/home/dk/a.pdf"));
        // The prefix is what makes these resolve.
        let trailing_dot = r"\\?\D:\books\notes.\a.pdf";
        assert_eq!(plain(trailing_dot), PathBuf::from(trailing_dot));
        let device = r"\\?\Volume{4c1b02c1-d990-11dc-99ae-806e6f6e6963}\a.pdf";
        assert_eq!(plain(device), PathBuf::from(device));
        let long = format!(r"\\?\D:\{}\a.pdf", "x".repeat(300));
        assert_eq!(plain(&long), PathBuf::from(&long));
    }
}
