//! Raw-byte and semantic hashes for revision members, parsed straight
//! from the file bytes at each entry's declared location.
//!
//! Both hashes are revision-correct by construction: an uncompressed object
//! is parsed at the header-relative offset its entry declares, and an
//! object-stream member is sliced out of its container's decoded bytes as
//! the container stands in that same revision (resolved through the live
//! locations accumulated up to it). Nothing here resolves through the
//! latest-view snapshot, so an object redefined in a later save keeps the
//! hashes of the definition the revision actually declares.
//!
//! Nothing here decrypts. Stream bodies that do not decode — encrypted
//! bodies included — yield no hash rather than a hash of ciphertext
//! mislabelled as content. Per-revision decryption would need each
//! revision's own file identifier and `/Encrypt` state; that is cryptographic
//! work for a later milestone, not a silent fallback here. The one exception
//! is the raw span of an uncompressed object in an encrypted file, which is
//! honestly hashable ciphertext: identical ciphertext still means an
//! identical object.

use std::collections::{BTreeMap, HashMap};

use pdf_object::parser::ObjectParser;
use pdf_object::{Dictionary, NameId, NameTable, PdfObject, PdfStream, StreamData};
use pdf_structure::decode::{DecodeBudget, decode_stream};
use pdf_structure::objstm::{ObjectStreamLayout, parse_object_stream_layout};
use pdf_syntax::SyntaxLimits;
use sha2::{Digest, Sha256};

/// Where a revision member claims its object lives, in this crate's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocationKey {
    /// An uncompressed object at this header-relative file offset.
    Offset(u64),
    /// A compressed member of an object stream.
    InStream {
        /// Object number of the containing `/Type /ObjStm` stream.
        container: u32,
        /// Zero-based index within the container.
        index: u32,
    },
    /// A free entry.
    Free,
}

/// The two hashes of one revision member. Each is `None` exactly when its
/// input could not be established; see
/// [`crate::document::ObjectChangeFacts`] for what that means per hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ByteHashes {
    /// SHA-256 over the bytes as stored, lowercase hex.
    pub raw: Option<String>,
    /// SHA-256 over the shallow canonical form, lowercase hex.
    pub semantic: Option<String>,
}

impl ByteHashes {
    /// Both hashes unknown: the entry is free, or nothing parsed.
    fn none() -> Self {
        Self {
            raw: None,
            semantic: None,
        }
    }
}

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // Hand-rolled rather than pulling in a hex crate for sixteen nibbles.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Per-object decode budget for hashing. A stream whose decoded body would
/// exceed this yields no semantic hash (and, for a container, no member
/// hashes): hashing must not become a decompression bomb.
const MAX_DECODED_BYTES_PER_OBJECT: usize = 64 * 1024 * 1024;

/// Parses and hashes revision members from the raw file bytes.
///
/// One instance serves a whole audit: the name table, parser limits, and
/// decoded object-stream containers are shared across every member.
pub(crate) struct HashSources<'a> {
    bytes: &'a [u8],
    /// Bytes of leading garbage before `%PDF`. Structural offsets are
    /// header-relative; adding this yields absolute source positions.
    /// `None` when no header was found, in which case no absolute span
    /// can be established and every hash is `None`.
    header_offset: Option<u64>,
    names: NameTable,
    limits: SyntaxLimits,
    /// `/F`, `/FFilter`, `/FDecodeParms`, pinned at construction: stream
    /// framing keys excluded from the semantic form alongside the
    /// well-known `/Length`, `/Filter`, and `/DecodeParms`.
    framing: [NameId; 3],
    /// Decoded containers keyed by (container number, header-relative
    /// offset of the container definition hashed): the same container
    /// number redefined in a later save decodes separately. `None` marks
    /// a container that failed to decode, so it is retried never.
    containers: HashMap<(u32, u64), Option<DecodedContainer>>,
}

/// One decoded object-stream container plus its member layout.
#[derive(Debug, Clone)]
struct DecodedContainer {
    data: Vec<u8>,
    layout: ObjectStreamLayout,
}

impl<'a> HashSources<'a> {
    /// Borrow the file bytes for hashing. `header_offset` is the `%PDF`
    /// header position, without which structural offsets cannot be placed.
    pub(crate) fn new(bytes: &'a [u8], header_offset: Option<u64>) -> Self {
        let names = NameTable::new();
        let framing = [
            names.intern(b"F"),
            names.intern(b"FFilter"),
            names.intern(b"FDecodeParms"),
        ];
        Self {
            bytes,
            header_offset,
            names,
            limits: SyntaxLimits::default(),
            framing,
            containers: HashMap::new(),
        }
    }

    /// Hash the uncompressed object declared at `offset` (header-relative),
    /// expecting object `number` there.
    ///
    /// The span is delimited by the object parser itself — from the
    /// entry's offset through the end of the parsed indirect object — so
    /// `endobj` bytes inside stream data can never truncate it. Indirect
    /// `/Length` entries are not resolved (there is no object repository
    /// at this layer); the parser falls back to its `endstream` scan,
    /// which delimits well-formed streams exactly.
    pub(crate) fn hash_offset(&mut self, offset: u64, number: u32) -> ByteHashes {
        let Some((span, value)) = self.parse_offset(offset, number) else {
            return ByteHashes::none();
        };
        let raw = Some(hex_sha256(span));
        let semantic = self.canonical_bytes(&value).map(|bytes| hex_sha256(&bytes));
        ByteHashes { raw, semantic }
    }

    /// Hash the object-stream member `number` at `index` of `container`.
    ///
    /// The container is decoded as it stands in the member's own revision:
    /// `live` maps object numbers to the locations live as of that
    /// revision, so a container redefined later does not leak its newer
    /// bytes into this member's hashes.
    pub(crate) fn hash_member(
        &mut self,
        container: u32,
        index: u32,
        number: u32,
        live: &BTreeMap<u32, LocationKey>,
    ) -> ByteHashes {
        let Some(stored) = self.member_stored(container, index, number, live) else {
            return ByteHashes::none();
        };
        let raw = Some(hex_sha256(&stored.bytes));
        // Members are direct objects: no `obj` wrapper, no streams.
        let mut parser = ObjectParser::new(&stored.bytes, stored.offset, &self.limits, &self.names);
        let semantic = parser
            .parse_object()
            .ok()
            .and_then(|value| self.canonical_bytes(&value))
            .map(|bytes| hex_sha256(&bytes));
        ByteHashes { raw, semantic }
    }

    /// Parse the indirect object at header-relative `offset`, checking it
    /// claims `number`. Returns the raw span and the parsed value.
    ///
    /// Only the object number is checked, not the generation: sloppy
    /// writers get generations wrong, and the resolver is equally
    /// tolerant. A generation change still classifies as a redefinition;
    /// it just does not block hashing.
    fn parse_offset(&self, offset: u64, number: u32) -> Option<(&'a [u8], PdfObject)> {
        let base = self.header_offset?.checked_add(offset)?;
        let start = usize::try_from(base).ok()?;
        let window = self.bytes.get(start..)?;
        let mut parser = ObjectParser::new(window, base, &self.limits, &self.names);
        let mut no_lengths = |_: pdf_object::ObjectId| None;
        let parsed = parser.parse_indirect(&mut no_lengths).ok()?;
        if parsed.id.number != number {
            return None;
        }
        let span = window.get(..parser.pos())?;
        Some((span, parsed.object))
    }

    /// The member's value bytes as stored in its decoded container, plus
    /// the member's offset within that decoded data (as the parser base).
    fn member_stored(
        &mut self,
        container: u32,
        index: u32,
        number: u32,
        live: &BTreeMap<u32, LocationKey>,
    ) -> Option<StoredMember> {
        // The container is an ordinary uncompressed stream; anything else
        // (freed, or impossibly nested in another stream) has no bytes.
        let LocationKey::Offset(container_offset) = live.get(&container).copied()? else {
            return None;
        };
        let decoded = self.container_bytes(container, container_offset)?;
        let member = decoded.layout.member(index)?;
        if member.number != number {
            return None;
        }
        let start = usize::try_from(member.offset).ok()?;
        // Members pack contiguously: one member's stored bytes run to the
        // next member's start, or to the end of the container data.
        let mut end = decoded.data.len() as u64;
        for other in &decoded.layout.members {
            if other.offset > member.offset && other.offset < end {
                end = other.offset;
            }
        }
        let end = usize::try_from(end).ok()?.min(decoded.data.len());
        let bytes = decoded.data.get(start..end)?;
        Some(StoredMember {
            bytes: bytes.to_vec(),
            offset: member.offset,
        })
    }

    /// Decode the container stream at header-relative `offset`, memoized.
    fn container_bytes(&mut self, container: u32, offset: u64) -> Option<DecodedContainer> {
        if let Some(cached) = self.containers.get(&(container, offset)) {
            return cached.clone();
        }
        let decoded = self.decode_container(container, offset);
        self.containers.insert((container, offset), decoded.clone());
        decoded
    }

    /// Parse one container stream and decode its body.
    fn decode_container(&self, container: u32, offset: u64) -> Option<DecodedContainer> {
        let (_, value) = self.parse_offset(offset, container)?;
        let PdfObject::Stream(stream) = value else {
            return None;
        };
        let data = self.decoded_body(&stream)?;
        let number = |key: &[u8]| {
            self.names
                .lookup(key)
                .and_then(|id| stream.dict.get(id))
                .and_then(PdfObject::as_int)
                .unwrap_or(0)
        };
        let layout =
            parse_object_stream_layout(&data, number(b"N"), number(b"First"), &self.limits).ok()?;
        Some(DecodedContainer { data, layout })
    }

    /// The shallow canonical form of `value`: dictionaries with keys
    /// sorted by name bytes, references by identity, names and strings as
    /// length-prefixed bytes, streams with framing keys excluded and
    /// bodies decoded. `None` when a stream body cannot be produced.
    ///
    /// Shallow on purpose: following references would need cycle guards
    /// and a repository, and would blur which object's hash this is. Two
    /// revisions of one object compare as stored plus one decode step.
    fn canonical_bytes(&self, value: &PdfObject) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        self.write_canonical(value, &mut out)?;
        Some(out)
    }

    /// Write the canonical form of `value` into `out`.
    fn write_canonical(&self, value: &PdfObject, out: &mut Vec<u8>) -> Option<()> {
        match value {
            PdfObject::Null => out.extend_from_slice(b"z"),
            PdfObject::Boolean(b) => out.extend_from_slice(if *b { b"b1" } else { b"b0" }),
            PdfObject::Integer(i) => {
                out.extend_from_slice(b"i");
                out.extend_from_slice(i.to_string().as_bytes());
            }
            PdfObject::Real(r) => {
                out.extend_from_slice(b"r");
                // Negative zero is zero: two serializations of the same
                // value must hash the same.
                let normalized = if *r == 0.0 { 0.0 } else { *r };
                out.extend_from_slice(normalized.to_string().as_bytes());
            }
            PdfObject::Name(id) => {
                let bytes = self.names.resolve(*id);
                write_prefixed(out, b'n', &bytes);
            }
            PdfObject::String(s) => write_prefixed(out, b's', s.as_bytes()),
            PdfObject::Array(items) => {
                out.extend_from_slice(b"a[");
                for item in items.iter() {
                    self.write_canonical(item, out)?;
                }
                out.extend_from_slice(b"]");
            }
            PdfObject::Dictionary(dict) => {
                out.extend_from_slice(b"d[");
                self.write_dict_canonical(dict, out, false)?;
                out.extend_from_slice(b"]");
            }
            PdfObject::Stream(stream) => {
                out.extend_from_slice(b"t[");
                self.write_dict_canonical(&stream.dict, out, true)?;
                let decoded = self.decoded_body(stream)?;
                write_prefixed(out, b's', &decoded);
                out.extend_from_slice(b"]");
            }
            PdfObject::Reference(id) => {
                out.extend_from_slice(b"f");
                out.extend_from_slice(id.number.to_string().as_bytes());
                out.extend_from_slice(b"g");
                out.extend_from_slice(id.generation.to_string().as_bytes());
            }
        }
        Some(())
    }

    /// Write a dictionary's entries sorted by resolved name bytes. For a
    /// stream dictionary (`framing` true), the framing keys — `/Length`,
    /// `/Filter`, `/DecodeParms`, `/F`, `/FFilter`, `/FDecodeParms` —
    /// are excluded: they describe the encoding, and the decoded body
    /// that follows carries the content.
    fn write_dict_canonical(
        &self,
        dict: &Dictionary,
        out: &mut Vec<u8>,
        framing: bool,
    ) -> Option<()> {
        let mut entries: Vec<(Vec<u8>, &PdfObject)> = Vec::with_capacity(dict.len());
        for (key, value) in dict.iter() {
            if framing && self.is_framing_key(key) {
                continue;
            }
            entries.push((self.names.resolve(key).to_vec(), value));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, value) in &entries {
            write_prefixed(out, b'n', name);
            self.write_canonical(value, out)?;
        }
        Some(())
    }

    /// Whether `key` is stream framing rather than content.
    fn is_framing_key(&self, key: NameId) -> bool {
        let known = &self.names.known;
        key == known.length
            || key == known.filter
            || key == known.decode_parms
            || self.framing.contains(&key)
    }

    /// A stream's decoded body: the stored bytes run through its `/Filter`
    /// chain. Indirect filter parameters are not resolved at this layer;
    /// such streams simply fail to decode and yield no hash.
    fn decoded_body(&self, stream: &PdfStream) -> Option<Vec<u8>> {
        let raw: &[u8] = match &stream.data {
            StreamData::InSource { offset, len } => {
                let start = usize::try_from(*offset).ok()?;
                let len = usize::try_from(*len).ok()?;
                self.bytes.get(start..start.checked_add(len)?)?
            }
            StreamData::Owned(bytes) => bytes,
        };
        let mut budget = DecodeBudget::new(MAX_DECODED_BYTES_PER_OBJECT);
        decode_stream(raw, &stream.dict, &self.names, &mut budget).ok()
    }
}

/// One member's stored bytes plus its offset in the decoded container.
struct StoredMember {
    bytes: Vec<u8>,
    /// Offset within the decoded container data, as the parser base.
    /// Carried so error offsets stay meaningful; the bytes are already cut.
    offset: u64,
}

/// Write `tag`, the decimal length, a colon, then `bytes`: unambiguous
/// framing for variable-length fields.
fn write_prefixed(out: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
    out.push(tag);
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(bytes);
}
