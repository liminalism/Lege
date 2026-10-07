//! Programmatic PDF fixture builder — hand-rolled bytes, no external deps.
//!
//! Produces small but *structurally real* PDFs: classic xref tables,
//! cross-reference streams, object streams, incremental updates, hybrid
//! files, deliberate corruption (wrong `/Length`, missing `startxref`,
//! leading garbage). Test-only; panics on misuse instead of returning
//! errors.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Where an object added to the current revision lives.
#[derive(Debug, Clone, Copy)]
enum Entry {
    /// Header-relative byte offset, generation.
    Offset(u64, u16),
    /// Member of an object stream added in this revision.
    InStream { container: u32, index: u32 },
}

/// Incremental PDF byte builder.
#[derive(Debug)]
pub struct PdfBuilder {
    buf: Vec<u8>,
    /// Entries accumulated since the last `finish_*` call.
    revision: BTreeMap<u32, Entry>,
    /// Highest object number ever used (for /Size).
    max_number: u32,
    /// Offset of the previously written xref section (for /Prev).
    prev_xref: Option<u64>,
}

impl Default for PdfBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PdfBuilder {
    pub fn new() -> Self {
        Self {
            buf: b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec(),
            revision: BTreeMap::new(),
            max_number: 0,
            prev_xref: None,
        }
    }

    /// Raw bytes so far (usually called after a `finish_*`).
    pub fn into_bytes(self) -> Vec<u8> {
        assert!(
            self.revision.is_empty(),
            "unfinished revision: call a finish_* method"
        );
        self.buf
    }

    /// Current length — handy for asserting offsets in tests.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn note(&mut self, number: u32, entry: Entry) {
        self.max_number = self.max_number.max(number);
        self.revision.insert(number, entry);
    }

    /// Append `N 0 obj <body> endobj`.
    pub fn add_object(&mut self, number: u32, body: &str) -> u64 {
        let offset = self.buf.len() as u64;
        self.note(number, Entry::Offset(offset, 0));
        write!(self.buf_str(), "{number} 0 obj\n{body}\nendobj\n").unwrap();
        offset
    }

    /// Append a stream object with a correct `/Length`. `dict_extra` is
    /// spliced into the dictionary (e.g. `"/Type/XObject"`).
    pub fn add_stream(&mut self, number: u32, dict_extra: &str, data: &[u8]) -> u64 {
        self.add_stream_with_length(number, dict_extra, data, data.len() as i64)
    }

    /// Append a stream object with an explicit (possibly lying) `/Length`.
    pub fn add_stream_with_length(
        &mut self,
        number: u32,
        dict_extra: &str,
        data: &[u8],
        declared_len: i64,
    ) -> u64 {
        let offset = self.buf.len() as u64;
        self.note(number, Entry::Offset(offset, 0));
        write!(
            self.buf_str(),
            "{number} 0 obj\n<<{dict_extra}/Length {declared_len}>>\nstream\n"
        )
        .unwrap();
        self.buf.extend_from_slice(data);
        self.buf.extend_from_slice(b"\nendstream\nendobj\n");
        offset
    }

    /// Append an *uncompressed* `/Type /ObjStm` object stream containing
    /// `members` (object number, body source). Member entries are recorded
    /// for the next xref stream; the containing revision must therefore be
    /// finished with [`Self::finish_xref_stream`].
    pub fn add_object_stream(&mut self, number: u32, members: &[(u32, &str)]) -> u64 {
        let mut pairs = String::new();
        let mut bodies = String::new();
        for (member_number, body) in members {
            if !bodies.is_empty() {
                bodies.push(' ');
            }
            write!(pairs, "{member_number} {} ", bodies.len()).unwrap();
            bodies.push_str(body);
        }
        let first = pairs.len();
        let data = format!("{pairs}{bodies}");
        for (index, (member_number, _)) in members.iter().enumerate() {
            self.note(
                *member_number,
                Entry::InStream {
                    container: number,
                    index: index as u32,
                },
            );
        }
        self.add_stream(
            number,
            &format!("/Type/ObjStm/N {}/First {first}", members.len()),
            data.as_bytes(),
        )
    }

    /// Finish the revision with a classic xref table + trailer +
    /// `startxref`. `trailer_extra` must contain `/Root N 0 R` for the
    /// first revision. Panics if the revision contains object-stream
    /// members (those need an xref stream).
    pub fn finish_classic_xref(&mut self, trailer_extra: &str) {
        let xref_offset = self.buf.len() as u64;
        let mut entries: Vec<(u32, u64, u16)> = Vec::new();
        if self.prev_xref.is_none() {
            entries.push((0, 0, u16::MAX)); // free-list head
        }
        for (&number, entry) in &self.revision {
            match *entry {
                Entry::Offset(offset, generation) => entries.push((number, offset, generation)),
                Entry::InStream { .. } => {
                    panic!("object-stream members need finish_xref_stream")
                }
            }
        }
        self.revision.clear();

        self.buf.extend_from_slice(b"xref\n");
        for run in consecutive_runs(&entries) {
            writeln!(self.buf_str(), "{} {}", run[0].0, run.len()).unwrap();
            for &(number, offset, generation) in run {
                if number == 0 || (offset == 0 && generation == u16::MAX) {
                    write!(self.buf_str(), "0000000000 65535 f\r\n").unwrap();
                } else {
                    write!(self.buf_str(), "{offset:010} {generation:05} n\r\n").unwrap();
                }
            }
        }
        let size = self.max_number + 1;
        write!(self.buf_str(), "trailer\n<</Size {size}{trailer_extra}").unwrap();
        if let Some(prev) = self.prev_xref {
            write!(self.buf_str(), "/Prev {prev}").unwrap();
        }
        write!(self.buf_str(), ">>\nstartxref\n{xref_offset}\n%%EOF\n").unwrap();
        self.prev_xref = Some(xref_offset);
    }

    /// Finish the revision with an *uncompressed* cross-reference stream
    /// (object `number`), covering every object added this revision plus
    /// the stream itself. `trailer_extra` is spliced into the stream dict
    /// (e.g. `"/Root 1 0 R"`).
    pub fn finish_xref_stream(&mut self, number: u32, trailer_extra: &str) {
        let xref_offset = self.buf.len() as u64;
        self.note(number, Entry::Offset(xref_offset, 0));
        let mut rows: Vec<(u32, [u8; 7])> = Vec::new(); // W [1 4 2]
        if self.prev_xref.is_none() {
            rows.push((0, [0, 0, 0, 0, 0, 0xFF, 0xFF]));
        }
        for (&object_number, entry) in &self.revision {
            let row = match *entry {
                Entry::Offset(offset, generation) => {
                    let o = u32::try_from(offset)
                        .expect("fixture too large")
                        .to_be_bytes();
                    let g = generation.to_be_bytes();
                    [1, o[0], o[1], o[2], o[3], g[0], g[1]]
                }
                Entry::InStream { container, index } => {
                    let c = container.to_be_bytes();
                    let i = u16::try_from(index)
                        .expect("objstm too large")
                        .to_be_bytes();
                    [2, c[0], c[1], c[2], c[3], i[0], i[1]]
                }
            };
            rows.push((object_number, row));
        }
        self.revision.clear();

        let mut index = String::new();
        let mut data: Vec<u8> = Vec::new();
        let entries: Vec<(u32, [u8; 7])> = rows;
        let numbers: Vec<(u32, u64, u16)> = entries.iter().map(|&(n, _)| (n, 0, 0)).collect();
        for run in consecutive_runs(&numbers) {
            write!(index, " {} {}", run[0].0, run.len()).unwrap();
        }
        for &(_, row) in &entries {
            data.extend_from_slice(&row);
        }

        let mut dict = format!(
            "/Type/XRef/Size {}/W[1 4 2]/Index[{}]{trailer_extra}",
            self.max_number + 1,
            index.trim(),
        );
        if let Some(prev) = self.prev_xref {
            write!(dict, "/Prev {prev}").unwrap();
        }
        write!(
            self.buf_str(),
            "{number} 0 obj\n<<{dict}/Length {}>>\nstream\n",
            data.len()
        )
        .unwrap();
        self.buf.extend_from_slice(&data);
        write!(
            self.buf_str(),
            "\nendstream\nendobj\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .unwrap();
        self.prev_xref = Some(xref_offset);
    }

    /// Finish the revision as a *hybrid-reference* save: an auxiliary
    /// cross-reference stream plus a classic xref table sharing one
    /// `startxref`/`%%EOF` tail.
    ///
    /// The stream (object `number`) indexes every object added this
    /// revision through the shared [`Self::finish_xref_stream`]
    /// implementation, but its `startxref` tail is stripped: the stream
    /// stays reachable only through the classic trailer's `/XRefStm`, so
    /// the save reads as one revision with an auxiliary section rather
    /// than two revisions. The stream carries no `/Prev` — the classic
    /// table holds the revision's chain link — matching the single-save
    /// hybrid shape of [`hybrid_fixture`].
    ///
    /// The classic table lists the revision's uncompressed entries (plus
    /// the object-0 free-list head for a first revision); object-stream
    /// members live in the stream. `trailer_extra` is spliced into the
    /// classic trailer (e.g. `"/Root 1 0 R"`).
    pub fn finish_hybrid_revision(&mut self, number: u32, trailer_extra: &str) {
        // Snapshot the plain entries for the classic table: the stream
        // writer below drains the revision. Already sorted — `revision`
        // is a `BTreeMap` — as the subsection runs below require.
        let mut plain: Vec<(u32, u64, u16)> = Vec::new();
        for (&object_number, entry) in &self.revision {
            if let Entry::Offset(offset, generation) = *entry {
                plain.push((object_number, offset, generation));
            }
        }
        // The auxiliary stream holds no chain link; stash ours across the
        // shared writer so it emits none, then restore it for the table.
        let prev = self.prev_xref.take();
        self.finish_xref_stream(number, "");
        let stream_offset = self
            .prev_xref
            .expect("auxiliary stream records its own offset");
        self.prev_xref = prev;
        let tail = self
            .buf
            .windows(9)
            .rposition(|w| w == b"startxref")
            .expect("auxiliary stream writes a startxref tail");
        self.buf.truncate(tail);

        // Classic table over the plain entries, mirroring
        // `finish_classic_xref` plus the `/XRefStm` link to the stream.
        if prev.is_none() {
            plain.insert(0, (0, 0, u16::MAX)); // free-list head
        }
        let table_offset = self.buf.len() as u64;
        self.buf.extend_from_slice(b"xref\n");
        for run in consecutive_runs(&plain) {
            writeln!(self.buf_str(), "{} {}", run[0].0, run.len()).unwrap();
            for &(number, offset, generation) in run {
                if number == 0 || (offset == 0 && generation == u16::MAX) {
                    write!(self.buf_str(), "0000000000 65535 f\r\n").unwrap();
                } else {
                    write!(self.buf_str(), "{offset:010} {generation:05} n\r\n").unwrap();
                }
            }
        }
        let size = self.max_number + 1;
        write!(self.buf_str(), "trailer\n<</Size {size}{trailer_extra}").unwrap();
        if let Some(prev) = prev {
            write!(self.buf_str(), "/Prev {prev}").unwrap();
        }
        writeln!(self.buf_str(), "/XRefStm {stream_offset}>>").unwrap();
        write!(self.buf_str(), "startxref\n{table_offset}\n%%EOF\n").unwrap();
        self.prev_xref = Some(table_offset);
    }

    /// `Vec<u8>` implements `fmt::Write` only via a wrapper; this gives the
    /// builder ergonomic `write!` calls (fixture bytes are ASCII anyway).
    fn buf_str(&mut self) -> BufWriter<'_> {
        BufWriter(&mut self.buf)
    }

    /// The bytes built so far, for locating patch sites mid-build.
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Overwrite `data` at `offset`, in place. The patch must fit inside
    /// the bytes built so far; nothing shifts, so every recorded offset
    /// stays valid. Test-only; panics on misuse like the rest of this
    /// builder.
    pub fn patch_bytes(&mut self, offset: usize, data: &[u8]) {
        assert!(
            offset.saturating_add(data.len()) <= self.buf.len(),
            "patch at {offset} ({} bytes) overruns {} built bytes",
            data.len(),
            self.buf.len()
        );
        self.buf[offset..offset + data.len()].copy_from_slice(data);
    }
}

struct BufWriter<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// Group `(number, …)` tuples (sorted) into consecutive-number runs.
fn consecutive_runs<T>(entries: &[(u32, T, u16)]) -> Vec<&[(u32, T, u16)]>
where
    T: Copy,
{
    let mut runs = Vec::new();
    let mut start = 0usize;
    for i in 1..=entries.len() {
        if i == entries.len() || entries[i].0 != entries[i - 1].0 + 1 {
            runs.push(&entries[start..i]);
            start = i;
        }
    }
    runs
}

/// Object-number layout used by the canned fixtures.
pub mod layout {
    pub const CATALOG: u32 = 1;
    pub const PAGES: u32 = 2;
    pub const OBJSTM: u32 = 100;
    pub const FONT: u32 = 101;
    pub const EXTRA: u32 = 102;
    pub const XREF_STREAM: u32 = 200;

    pub fn page(i: u32) -> u32 {
        10 + 2 * i
    }
    pub fn content(i: u32) -> u32 {
        11 + 2 * i
    }
    pub fn content_bytes(i: u32) -> Vec<u8> {
        format!("BT /F1 12 Tf 72 720 Td (Page {i}) Tj ET").into_bytes()
    }
    pub fn updated_content_bytes(i: u32) -> Vec<u8> {
        format!("BT /F1 12 Tf 72 720 Td (Updated page {i}) Tj ET").into_bytes()
    }
}

/// Write catalog + page tree + per-page content streams (one revision's
/// worth of body objects, no xref yet).
fn add_document_body(builder: &mut PdfBuilder, page_count: u32) {
    use layout::*;
    builder.add_object(CATALOG, &format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
    let kids: Vec<String> = (0..page_count)
        .map(|i| format!("{} 0 R", page(i)))
        .collect();
    builder.add_object(
        PAGES,
        &format!(
            "<</Type/Pages/Kids[{}]/Count {page_count}/MediaBox[0 0 612 792]>>",
            kids.join(" ")
        ),
    );
    for i in 0..page_count {
        // Page 1 carries its own rotation and crop box; everything else
        // inherits. Resources reference the object-stream font.
        let extras = if i == 1 {
            "/Rotate 90/CropBox[10 10 400 500]"
        } else {
            ""
        };
        builder.add_object(
            page(i),
            &format!(
                "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R{extras}\
                 /Resources<</Font<</F1 {FONT} 0 R>>>>>>",
                content(i)
            ),
        );
        builder.add_stream(content(i), "", &content_bytes(i));
    }
}

/// A clean `page_count`-page PDF with a classic xref table. The font the
/// pages reference is a plain indirect object (no object streams).
pub fn multipage_classic(page_count: u32) -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_document_body(&mut builder, page_count);
    builder.add_object(
        layout::FONT,
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// The Phase 1 exit-gate fixture: `page_count` pages (classic first
/// revision) + an incremental update whose xref stream adds an object
/// stream (containing the shared font) and rewrites page 0's content.
pub fn phase1_exit_fixture(page_count: u32) -> Vec<u8> {
    use layout::*;
    assert!(page_count >= 1);
    let mut builder = PdfBuilder::new();
    add_document_body(&mut builder, page_count);
    // First revision: the font object does not exist yet (forward ref from
    // the pages is fine — it resolves after the update).
    builder.finish_classic_xref("/Root 1 0 R");

    // Incremental update: object stream with the font + an extra dict,
    // plus a replacement content stream for page 0.
    builder.add_object_stream(
        OBJSTM,
        &[
            (FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>"),
            (EXTRA, "<</Kind(objstm-extra)>>"),
        ],
    );
    builder.add_stream(content(0), "", &updated_content_bytes(0));
    builder.finish_xref_stream(XREF_STREAM, "/Root 1 0 R");
    builder.into_bytes()
}

/// Single-revision file indexed *only* by a cross-reference stream, with
/// the font inside an object stream.
pub fn xref_stream_fixture(page_count: u32) -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    add_document_body(&mut builder, page_count);
    builder.add_object_stream(
        OBJSTM,
        &[(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>")],
    );
    builder.finish_xref_stream(XREF_STREAM, "/Root 1 0 R");
    builder.into_bytes()
}

/// Prefix a fixture with garbage. Structural offsets are header-relative,
/// so the fixture stays valid — exactly the real-world case this exercises.
pub fn with_leading_garbage(fixture: Vec<u8>, garbage: &[u8]) -> Vec<u8> {
    let mut out = garbage.to_vec();
    out.extend_from_slice(&fixture);
    out
}

/// Corrupt a fixture by deleting its final `startxref` block. The xref
/// table itself survives, so a reader can still recover it by scanning.
pub fn without_startxref(mut fixture: Vec<u8>) -> Vec<u8> {
    let pos = fixture
        .windows(9)
        .rposition(|w| w == b"startxref")
        .expect("fixture has no startxref");
    fixture.truncate(pos);
    fixture.extend_from_slice(b"%%EOF\n");
    fixture
}

/// Corrupt a fixture by deleting the entire xref section (table, trailer,
/// startxref) — only object bodies remain, forcing a full rebuild.
pub fn without_xref(mut fixture: Vec<u8>) -> Vec<u8> {
    let pos = fixture
        .windows(5)
        .rposition(|w| w == b"\nxref")
        .expect("fixture has no xref table");
    fixture.truncate(pos + 1);
    fixture.extend_from_slice(b"%%EOF\n");
    fixture
}

/// A one-page document whose content stream declares a wrong `/Length`.
pub fn wrong_length_fixture() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    builder.add_object(CATALOG, &format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
    builder.add_object(
        PAGES,
        &format!(
            "<</Type/Pages/Kids[{} 0 R]/Count 1/MediaBox[0 0 612 792]>>",
            page(0)
        ),
    );
    builder.add_object(
        page(0),
        &format!(
            "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R>>",
            content(0)
        ),
    );
    let data = content_bytes(0);
    builder.add_stream_with_length(content(0), "", &data, 3); // lies
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A one-page document carrying an `/Encrypt` dictionary (RC4 V2/R3) — a
/// supported scheme, so it *opens* (the derived key is exercised even though
/// this fixture has no encrypted payload to check).
pub fn encrypted_fixture() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    builder.add_object(CATALOG, &format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
    builder.add_object(
        PAGES,
        &format!("<</Type/Pages/Kids[{} 0 R]/Count 1>>", page(0)),
    );
    builder.add_object(page(0), &format!("<</Type/Page/Parent {PAGES} 0 R>>"));
    // A conforming /U for the empty user password: `DocumentSnapshot::open`
    // genuinely validates /U (Algorithm 6), so the fixture must store the
    // value a real writer would.
    let enc = pdf_security::EncryptDict {
        v: 2,
        r: 3,
        o: b"o".to_vec(),
        u: vec![],
        ue: vec![],
        oe: vec![],
        p: -44,
        perms: vec![],
        key_bytes: 16,
        encrypt_metadata: true,
        cipher: pdf_security::Cipher::Rc4,
        file_id: vec![0x01],
    };
    let u = pdf_security::StandardHandler::compute_user_entry(&enc, "");
    let u_hex: String = u.iter().map(|b| format!("{b:02x}")).collect();
    builder.add_object(
        50,
        &format!("<</Filter/Standard/V 2/R 3/Length 128/P -44/O(o)/U<{u_hex}>>>"),
    );
    builder.finish_classic_xref("/Root 1 0 R/Encrypt 50 0 R/ID[<01><02>]");
    builder.into_bytes()
}

/// A one-page document carrying an `/Encrypt` dictionary (RC4 V2/R3) whose
/// `/U` validates for `password` — a non-empty user password on a scheme the
/// standard handler supports, so the file opens only when that password is
/// supplied.
///
/// Like [`encrypted_fixture`], the page carries no content stream, so there is
/// no encrypted payload to check; the open gate (`PasswordRequired` without
/// the password, `Full` with it) is what this exercises.
///
/// # Panics
///
/// Panics when `password` is empty; use [`encrypted_fixture`] for the
/// permissions-only (empty-password) shape.
pub fn encrypted_with_user_password(password: &str) -> Vec<u8> {
    use layout::*;
    assert!(
        !password.is_empty(),
        "use encrypted_fixture for the empty user password"
    );
    let mut builder = PdfBuilder::new();
    builder.add_object(CATALOG, &format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
    builder.add_object(
        PAGES,
        &format!("<</Type/Pages/Kids[{} 0 R]/Count 1>>", page(0)),
    );
    builder.add_object(page(0), &format!("<</Type/Page/Parent {PAGES} 0 R>>"));
    // A conforming /U for the given user password: `DocumentSnapshot::open`
    // genuinely validates /U (Algorithm 6), so the fixture must store the
    // value a real writer would.
    let enc = pdf_security::EncryptDict {
        v: 2,
        r: 3,
        o: b"o".to_vec(),
        u: vec![],
        ue: vec![],
        oe: vec![],
        p: -44,
        perms: vec![],
        key_bytes: 16,
        encrypt_metadata: true,
        cipher: pdf_security::Cipher::Rc4,
        file_id: vec![0x01],
    };
    let u = pdf_security::StandardHandler::compute_user_entry(&enc, password);
    let u_hex: String = u.iter().map(|b| format!("{b:02x}")).collect();
    builder.add_object(
        50,
        &format!("<</Filter/Standard/V 2/R 3/Length 128/P -44/O(o)/U<{u_hex}>>>"),
    );
    builder.finish_classic_xref("/Root 1 0 R/Encrypt 50 0 R/ID[<01><02>]");
    builder.into_bytes()
}

/// A one-page document carrying an AES-256 (`/V 5 /R 6`) `/Encrypt` dictionary
/// — a scheme the standard handler declines, so open must fail with a typed
/// security error rather than pretend the file parsed.
pub fn aes256_encrypted_fixture() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    builder.add_object(CATALOG, &format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
    builder.add_object(
        PAGES,
        &format!("<</Type/Pages/Kids[{} 0 R]/Count 1>>", page(0)),
    );
    builder.add_object(page(0), &format!("<</Type/Page/Parent {PAGES} 0 R>>"));
    builder.add_object(
        50,
        "<</Filter/Standard/V 5/R 6/Length 256/P -44\
         /CF<</StdCF<</CFM/AESV3>>>>/StmF/StdCF/StrF/StdCF/O(o)/U(u)>>",
    );
    builder.finish_classic_xref("/Root 1 0 R/Encrypt 50 0 R/ID[<01><02>]");
    builder.into_bytes()
}

/// Hybrid-reference fixture: classic table for the base objects plus an
/// /XRefStm whose stream indexes the object-stream members.
pub fn hybrid_fixture(page_count: u32) -> Vec<u8> {
    use layout::*;
    // Build the pieces manually: body, xref stream (not startxref'd), then
    // the classic table pointing at the stream via /XRefStm.
    let mut builder = PdfBuilder::new();
    add_document_body(&mut builder, page_count);
    builder.add_object_stream(
        OBJSTM,
        &[(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>")],
    );

    // Snapshot the entries the classic table must NOT know about (the
    // members) vs the ones it lists. We reach into the byte stream: write
    // the xref stream with finish_xref_stream, then strip its trailing
    // startxref block and append the classic section.
    let objstm_and_member_revision_done = {
        builder.finish_xref_stream(XREF_STREAM, "");
        true
    };
    assert!(objstm_and_member_revision_done);
    let mut bytes = builder.into_bytes();
    // Remove the xref stream's startxref/%%EOF tail; remember its offset.
    let sx = bytes
        .windows(9)
        .rposition(|w| w == b"startxref")
        .expect("startxref");
    let stream_offset: u64 = {
        let tail = &bytes[sx..];
        let text = std::str::from_utf8(tail).expect("ascii tail");
        text.lines()
            .nth(1)
            .expect("offset line")
            .trim()
            .parse()
            .expect("offset")
    };
    bytes.truncate(sx);

    // Classic table: catalog, pages, page/content objects (offsets must be
    // re-derived — scan for their headers, test-support style).
    let mut entries: Vec<(u32, u64)> = Vec::new();
    for number in [CATALOG, PAGES]
        .into_iter()
        .chain((0..page_count).flat_map(|i| [page(i), content(i)]))
    {
        let needle = format!("{number} 0 obj");
        let off = bytes
            .windows(needle.len())
            .position(|w| w == needle.as_bytes())
            .unwrap_or_else(|| panic!("object {number} not found")) as u64;
        entries.push((number, off));
    }
    entries.sort_unstable();

    let table_offset = bytes.len();
    let mut out = BufWriter(&mut bytes);
    write!(out, "xref\n0 1\n0000000000 65535 f\r\n").unwrap();
    let mut i = 0usize;
    while i < entries.len() {
        let mut j = i + 1;
        while j < entries.len() && entries[j].0 == entries[j - 1].0 + 1 {
            j += 1;
        }
        writeln!(out, "{} {}", entries[i].0, j - i).unwrap();
        for &(_, off) in &entries[i..j] {
            write!(out, "{off:010} 00000 n\r\n").unwrap();
        }
        i = j;
    }
    write!(
        out,
        "trailer\n<</Size {}/Root 1 0 R/XRefStm {stream_offset}>>\nstartxref\n{table_offset}\n%%EOF\n",
        XREF_STREAM + 1
    )
    .unwrap();
    bytes
}

/// A `page_count`-page raster scan: every page is one full-page image and
/// nothing else — the ordinary-scan and PDF-Verse-export shape.
///
/// With `ocr_layer`, each page additionally carries one invisible (`3 Tr`)
/// text run, the scan-with-OCR shape. Without it the file has no fonts at
/// all. Covers up to 9 pages, which is all a fixture needs.
pub fn raster_scan(page_count: u32, ocr_layer: bool) -> Vec<u8> {
    assert!(page_count >= 1, "a scan has at least one page");
    assert!(page_count <= 9, "scan object layout covers up to 9 pages");
    let mut builder = PdfBuilder::new();
    builder.add_object(1, "<</Type/Catalog/Pages 2 0 R>>");
    let kids: Vec<String> = (0..page_count).map(|i| format!("{} 0 R", 10 + i)).collect();
    builder.add_object(
        2,
        &format!(
            "<</Type/Pages/Kids[{}]/Count {page_count}/MediaBox[0 0 612 792]>>",
            kids.join(" ")
        ),
    );
    for i in 0..page_count {
        let page = 10 + i;
        let content = 20 + i;
        let image = 30 + i;
        let resources = if ocr_layer {
            format!("/XObject<</Im {image} 0 R>>/Font<</F1 40 0 R>>")
        } else {
            format!("/XObject<</Im {image} 0 R>>")
        };
        builder.add_object(
            page,
            &format!("<</Type/Page/Parent 2 0 R/Contents {content} 0 R/Resources<<{resources}>>>>"),
        );
        let content_bytes: &[u8] = if ocr_layer {
            b"q 612 0 0 792 0 0 cm /Im Do Q BT 3 Tr /F1 12 Tf 72 720 Td (scanned line) Tj ET"
        } else {
            b"q 612 0 0 792 0 0 cm /Im Do Q"
        };
        builder.add_stream(content, "", content_bytes);
        // 8x8 mid-grey: coverage is driven by the placement matrix above,
        // not by the sample count, so the image stays tiny.
        builder.add_stream(
            image,
            "/Type/XObject/Subtype/Image/Width 8/Height 8/BitsPerComponent 8/ColorSpace/DeviceGray",
            &[0x88; 64],
        );
    }
    if ocr_layer {
        builder.add_object(40, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    }
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A `page_count`-page vector document whose `raster_index`-th page is a
/// full-page raster instead — the one-raster-page-outlier shape.
///
/// Vector pages carry one text run each through the shared font; the outlier
/// page carries one full-page image and no text.
pub fn vector_with_raster_outlier(page_count: u32, raster_index: u32) -> Vec<u8> {
    use layout::*;
    assert!(page_count >= 1, "a document has at least one page");
    assert!(raster_index < page_count, "the outlier must be a real page");
    let mut builder = PdfBuilder::new();
    builder.add_object(CATALOG, &format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
    let kids: Vec<String> = (0..page_count)
        .map(|i| format!("{} 0 R", page(i)))
        .collect();
    builder.add_object(
        PAGES,
        &format!(
            "<</Type/Pages/Kids[{}]/Count {page_count}/MediaBox[0 0 612 792]>>",
            kids.join(" ")
        ),
    );
    for i in 0..page_count {
        if i == raster_index {
            let image = 300 + i;
            builder.add_object(
                page(i),
                &format!(
                    "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R\
                     /Resources<</XObject<</Im {image} 0 R>>>>>>",
                    content(i)
                ),
            );
            builder.add_stream(content(i), "", b"q 612 0 0 792 0 0 cm /Im Do Q");
            builder.add_stream(
                image,
                "/Type/XObject/Subtype/Image/Width 8/Height 8/BitsPerComponent 8/ColorSpace/DeviceGray",
                &[0x88; 64],
            );
        } else {
            builder.add_object(
                page(i),
                &format!(
                    "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R\
                     /Resources<</Font<</F1 {FONT} 0 R>>>>>>",
                    content(i)
                ),
            );
            builder.add_stream(content(i), "", &content_bytes(i));
        }
    }
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A one-page vector document with a filled `/AcroForm`: two text fields
/// carrying `/V` values.
///
/// The fields are bare field dictionaries rather than widget annotations, so
/// this fixture exercises the AcroForm flag without contributing
/// annotations.
pub fn acroform_filled() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    builder.add_object(
        CATALOG,
        &format!("<</Type/Catalog/Pages {PAGES} 0 R/AcroForm 50 0 R>>"),
    );
    builder.add_object(
        PAGES,
        &format!(
            "<</Type/Pages/Kids[{} 0 R]/Count 1/MediaBox[0 0 612 792]>>",
            page(0)
        ),
    );
    builder.add_object(
        page(0),
        &format!(
            "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R\
             /Resources<</Font<</F1 {FONT} 0 R>>>>>>",
            content(0)
        ),
    );
    builder.add_stream(content(0), "", &content_bytes(0));
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.add_object(50, "<</Fields [51 0 R 52 0 R]/DA (/Helv 12 Tf)>>");
    builder.add_object(51, "<</FT/Tx/T (full-name)/V (Ada Lovelace)>>");
    builder.add_object(52, "<</FT/Tx/T (city)/V (London)>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A one-page vector document carrying three annotations — `/Text`,
/// `/Highlight` and `/Ink` — the annotation-heavy shape in miniature.
///
/// The annotations carry forensic provenance metadata with varied coverage:
/// the `/Text` note has the full set (`/Contents`, `/NM`, `/M`,
/// `/CreationDate`, `/T`, `/Subj`, `/IT`), the `/Highlight` a partial set,
/// and the `/Ink` only `/NM` — so consumers exercise both present and
/// absent `Option` fields.
pub fn annotated() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    builder.add_object(CATALOG, &format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
    builder.add_object(
        PAGES,
        &format!(
            "<</Type/Pages/Kids[{} 0 R]/Count 1/MediaBox[0 0 612 792]>>",
            page(0)
        ),
    );
    builder.add_object(
        page(0),
        &format!(
            "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R/Annots [60 0 R 61 0 R 62 0 R]\
             /Resources<</Font<</F1 {FONT} 0 R>>>>>>",
            content(0)
        ),
    );
    builder.add_stream(content(0), "", &content_bytes(0));
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.add_object(
        60,
        "<</Type/Annot/Subtype/Text/Rect [72 700 90 718]/Contents (margin note)\
         /NM (note-1)/M (D:20240101120000Z)/CreationDate (D:20240101090000Z)\
         /T (Alice)/Subj (Review)/IT /Comment>>",
    );
    builder.add_object(
        61,
        "<</Type/Annot/Subtype/Highlight/Rect [72 680 200 692]\
         /QuadPoints [72 692 200 692 72 680 200 680]\
         /Contents (highlighted passage)/NM (hl-1)/M (D:20240202130000Z)/T (Bob)>>",
    );
    builder.add_object(
        62,
        "<</Type/Annot/Subtype/Ink/Rect [72 640 200 660]\
         /InkList [[72 650 200 650]]/NM (ink-1)>>",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A one-page vector document with a `/Linearized` parameter dictionary as
/// its first object, the marker a linearized file opens with.
///
/// Only the marker shape is reproduced — first object, `/Linearized 1` —
/// not first-page-first ordering or hint tables. Detection reads exactly
/// that shape: a valid `/Linearized` dict as the first object.
pub fn linearized() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    builder.add_object(90, "<</Linearized 1/L 1/O 92/E 1/N 1/T 1>>");
    add_document_body(&mut builder, 1);
    builder.add_object(
        layout::FONT,
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A two-page document with an interactive-edit marker: page 0 draws one
/// plain run, then wraps its second run (at 72,700 in 12pt Helvetica) in
/// `/TouchUp_TextEdit BMC … EMC` — the shape Acrobat's in-place text edit
/// leaves behind. Page 1 wraps its run in a neutral `/Span` span, so
/// consumers exercise both a TouchUp tag and an ordinary one.
pub fn touchup_edit() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    builder.add_object(CATALOG, &format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
    builder.add_object(
        PAGES,
        &format!(
            "<</Type/Pages/Kids[{} 0 R {} 0 R]/Count 2/MediaBox[0 0 612 792]>>",
            page(0),
            page(1)
        ),
    );
    for i in 0..2 {
        builder.add_object(
            page(i),
            &format!(
                "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R\
                 /Resources<</Font<</F1 {FONT} 0 R>>>>>>",
                content(i)
            ),
        );
    }
    builder.add_stream(
        content(0),
        "",
        b"BT /F1 12 Tf 72 720 Td (Original line) Tj ET\n\
          /TouchUp_TextEdit BMC\n\
          BT /F1 12 Tf 72 700 Td (Retyped line) Tj ET\n\
          EMC",
    );
    builder.add_stream(
        content(1),
        "",
        b"/Span BMC\nBT /F1 12 Tf 72 720 Td (Plain page) Tj ET\nEMC",
    );
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// First revision shared by the incremental fixtures below: a complete
/// one-page document with a classic xref table whose font is a plain
/// indirect object, so the revision opens standalone. That is the point —
/// a forward-referencing first revision like [`phase1_exit_fixture`]'s
/// could never be a replay target.
fn add_complete_single_page(builder: &mut PdfBuilder) {
    use layout::*;
    add_document_body(builder, 1);
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.finish_classic_xref("/Root 1 0 R");
}

/// Two revisions, both classic: the update (with `/Prev`) redefines page
/// 0's content stream while the original font object sits untouched and
/// reused — the minimal text-edit shape, and the historical-replay
/// target: revision 0 renders `(Page 0)`, revision 1 `(Updated page 0)`.
///
/// Revision entries: 6 then 1 (the rewritten content stream alone).
pub fn incremental_redefined_content() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    add_complete_single_page(&mut builder);
    builder.add_stream(content(0), "", &updated_content_bytes(0));
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two classic revisions with a controlled GPA edit: revision 0 shows
/// `(Cumulative GPA: 2.84)`; the incremental update redefines page 0's
/// content stream alone — the shared font object sits untouched and reused —
/// to `(Cumulative GPA: 3.84)`. The revision-diff target: one changed word,
/// same font, exact before/after strings.
///
/// Revision entries: 6 then 1 (the rewritten content stream alone).
pub fn gpa_edit() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    builder.add_object(CATALOG, &format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
    builder.add_object(
        PAGES,
        &format!(
            "<</Type/Pages/Kids[{} 0 R]/Count 1/MediaBox[0 0 612 792]>>",
            page(0)
        ),
    );
    builder.add_object(
        page(0),
        &format!(
            "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R\
             /Resources<</Font<</F1 {FONT} 0 R>>>>>>",
            content(0)
        ),
    );
    builder.add_stream(
        content(0),
        "",
        b"BT /F1 12 Tf 72 720 Td (Cumulative GPA: 2.84) Tj ET",
    );
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.add_stream(
        content(0),
        "",
        b"BT /F1 12 Tf 72 720 Td (Cumulative GPA: 3.84) Tj ET",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two classic revisions where the update only *adds* objects (two plain
/// dictionaries): nothing is redefined, so every second-revision member
/// classifies as added.
pub fn incremental_added_objects() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    add_complete_single_page(&mut builder);
    builder.add_object(EXTRA, "<</Kind(added-extra)>>");
    builder.add_object(103, "<</Kind(added-second)>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A classic first revision plus an xref-stream second revision (with
/// `/Prev`) that redefines page 0's content stream and adds one
/// dictionary object: the compressed-index incremental shape without any
/// object stream.
///
/// Second-revision entries: the rewritten content stream, the added
/// object, and the xref stream itself.
pub fn incremental_xref_stream() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    add_complete_single_page(&mut builder);
    builder.add_stream(content(0), "", &updated_content_bytes(0));
    builder.add_object(EXTRA, "<</Kind(added-extra)>>");
    builder.finish_xref_stream(XREF_STREAM, "/Root 1 0 R");
    builder.into_bytes()
}

/// A classic first revision plus a hybrid-reference second revision
/// (with `/Prev`): the update redefines page 0's content stream through
/// the classic table and adds an object stream carrying one member, the
/// stream indexed by the auxiliary `/XRefStm` section. One save, two
/// sections — the incremental analogue of [`hybrid_fixture`].
pub fn incremental_hybrid() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    add_complete_single_page(&mut builder);
    builder.add_stream(content(0), "", &updated_content_bytes(0));
    builder.add_object_stream(OBJSTM, &[(EXTRA, "<</Kind(objstm-extra)>>")]);
    builder.finish_hybrid_revision(XREF_STREAM, "/Root 1 0 R");
    builder.into_bytes()
}

/// One-page GPA document (`(Cumulative GPA: 2.84)` at 72,720 in 12pt
/// Helvetica) carrying the caller's `/Annots` entries: the shared body for
/// the annotation-overlay fixtures below. The overlay rectangle
/// `[70 705 265 735]` covers the whole run with margin, and appearance
/// forms use `/BBox [0 0 195 30]` so the §12.5.5 fit is a plain translate
/// with no scaling.
fn add_overlay_body(builder: &mut PdfBuilder, annots: &str) {
    use layout::*;
    builder.add_object(CATALOG, &format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
    builder.add_object(
        PAGES,
        &format!(
            "<</Type/Pages/Kids[{} 0 R]/Count 1/MediaBox[0 0 612 792]>>",
            page(0)
        ),
    );
    builder.add_object(
        page(0),
        &format!(
            "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R/Annots [{annots}]\
             /Resources<</Font<</F1 {FONT} 0 R>>>>>>",
            content(0)
        ),
    );
    builder.add_stream(
        content(0),
        "",
        b"BT /F1 12 Tf 72 720 Td (Cumulative GPA: 2.84) Tj ET",
    );
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
}

/// A `/FreeText` annotation whose appearance whites out the GPA run and
/// writes `(3.84)` over it: the explicit-overlay shape with replacement
/// text in the appearance stream.
pub fn freetext_overlay() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_overlay_body(&mut builder, "60 0 R");
    builder.add_object(
        60,
        "<</Type/Annot/Subtype/FreeText/Rect [70 705 265 735]/AP <</N 61 0 R>>>>",
    );
    builder.add_stream(
        61,
        "/Type/XObject/Subtype/Form/BBox [0 0 195 30]/Resources <</Font <</F1 101 0 R>>>>",
        b"1 1 1 rg 0 0 195 30 re f BT /F1 12 Tf 12 6 Td (3.84) Tj ET",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A `/Stamp` annotation whose appearance is a full-rectangle grey image
/// over the GPA run: the image-stamp overlay shape, with no text in the
/// appearance stream.
pub fn stamp_image_overlay() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_overlay_body(&mut builder, "60 0 R");
    builder.add_object(
        60,
        "<</Type/Annot/Subtype/Stamp/Rect [70 705 265 735]/AP <</N 61 0 R>>>>",
    );
    builder.add_stream(
        61,
        "/Type/XObject/Subtype/Form/BBox [0 0 195 30]/Resources <</XObject <</Im 62 0 R>>>>",
        b"q 195 0 0 30 0 0 cm /Im Do Q",
    );
    builder.add_stream(
        62,
        "/Type/XObject/Subtype/Image/Width 8/Height 8/BitsPerComponent 8/ColorSpace/DeviceGray",
        &[0x88; 64],
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A `/Redact` annotation with an applied (opaque black) appearance over
/// the GPA run.
pub fn redact_overlay() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_overlay_body(&mut builder, "60 0 R");
    builder.add_object(
        60,
        "<</Type/Annot/Subtype/Redact/Rect [70 705 265 735]/AP <</N 61 0 R>>>>",
    );
    builder.add_stream(
        61,
        "/Type/XObject/Subtype/Form/BBox [0 0 195 30]",
        b"0 0 0 rg 0 0 195 30 re f",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A `/FreeText` annotation whose appearance sits far from the GPA run:
/// it paints, but over no text, so it is geometry-gated out.
///
/// The fill is grey rather than white: text inside annotation appearances
/// does not rasterize in the evidence renderer, so a white rect over the
/// blank margin would contribute zero changed pixels and the control would
/// prove nothing.
pub fn freetext_elsewhere() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_overlay_body(&mut builder, "60 0 R");
    builder.add_object(
        60,
        "<</Type/Annot/Subtype/FreeText/Rect [400 100 500 130]/AP <</N 61 0 R>>>>",
    );
    builder.add_stream(
        61,
        "/Type/XObject/Subtype/Form/BBox [0 0 100 30]/Resources <</Font <</F1 101 0 R>>>>",
        b"0.8 0.8 0.8 rg 0 0 100 30 re f BT /F1 12 Tf 12 6 Td (side note) Tj ET",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A `/Highlight` annotation over the GPA run with no authored appearance:
/// the renderer synthesizes its translucent yellow fill, so pixels change
/// over glyphs — and the overlay analysis must still stay silent.
pub fn highlight_over_text() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_overlay_body(&mut builder, "60 0 R");
    builder.add_object(
        60,
        "<</Type/Annot/Subtype/Highlight/Rect [70 705 265 735]\
         /QuadPoints [70 735 265 735 70 705 265 705]>>",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A form `/Widget` annotation with an opaque appearance over the GPA run:
/// it erases glyph pixels like any covering box, and the overlay analysis
/// must still stay silent.
pub fn widget_over_text() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_overlay_body(&mut builder, "60 0 R");
    builder.add_object(
        60,
        "<</Type/Annot/Subtype/Widget/Rect [70 705 265 735]/AP <</N 61 0 R>>>>",
    );
    builder.add_stream(
        61,
        "/Type/XObject/Subtype/Form/BBox [0 0 195 30]",
        b"1 1 1 rg 0 0 195 30 re f",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// One-page GPA document with caller-supplied page content and resources:
/// the shared body for the flattened-occlusion fixtures below, where the
/// cover lives in the page content stream itself rather than in an
/// annotation appearance. Covers use the rectangle `[70 705 265 735]`,
/// which spans the whole GPA run with margin.
fn add_flattened_body(builder: &mut PdfBuilder, contents: &[u8], resources: &str) {
    use layout::*;
    builder.add_object(CATALOG, &format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
    builder.add_object(
        PAGES,
        &format!(
            "<</Type/Pages/Kids[{} 0 R]/Count 1/MediaBox[0 0 612 792]>>",
            page(0)
        ),
    );
    builder.add_object(
        page(0),
        &format!(
            "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R/Resources<<{resources}>>>>",
            content(0)
        ),
    );
    builder.add_stream(content(0), "", contents);
}

/// The plain GPA text page: `(Cumulative GPA: 2.84)` at 72,720 in 12pt
/// Helvetica and nothing else. The baseline the occlusion controls diff
/// against to prove a silent cover really paints.
pub fn gpa_text_page() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    add_flattened_body(
        &mut builder,
        b"BT /F1 12 Tf 72 720 Td (Cumulative GPA: 2.84) Tj ET",
        &format!("/Font<</F1 {FONT} 0 R>>"),
    );
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Page content that whites out the GPA run with an opaque rectangle and
/// writes `(3.84)` over it: the flattened-occlusion shape with replacement
/// text in the content stream.
pub fn white_rect_replacement() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    add_flattened_body(
        &mut builder,
        b"BT /F1 12 Tf 72 720 Td (Cumulative GPA: 2.84) Tj ET\n\
          1 1 1 rg 70 705 195 30 re f\n\
          BT /F1 12 Tf 172 720 Td (3.84) Tj ET",
        &format!("/Font<</F1 {FONT} 0 R>>"),
    );
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Page content that pastes a grey image patch over the GPA run: the
/// raster-patch shape, with no replacement text in the content stream.
pub fn image_patch_over_text() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    add_flattened_body(
        &mut builder,
        b"BT /F1 12 Tf 72 720 Td (Cumulative GPA: 2.84) Tj ET\n\
          q 195 0 0 30 70 705 cm /Im Do Q",
        &format!("/Font<</F1 {FONT} 0 R>>/XObject<</Im 62 0 R>>"),
    );
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.add_stream(
        62,
        "/Type/XObject/Subtype/Image/Width 8/Height 8/BitsPerComponent 8/ColorSpace/DeviceGray",
        &[0x88; 64],
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Page content that whites out the GPA run from inside a form XObject and
/// writes `(3.84)` over it: the same occlusion as
/// [`white_rect_replacement`], but with form-xobject provenance instead of
/// page-content provenance.
pub fn form_rect_replacement() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    add_flattened_body(
        &mut builder,
        b"BT /F1 12 Tf 72 720 Td (Cumulative GPA: 2.84) Tj ET\n\
          /Fm1 Do\n\
          BT /F1 12 Tf 172 720 Td (3.84) Tj ET",
        &format!("/Font<</F1 {FONT} 0 R>>/XObject<</Fm1 61 0 R>>"),
    );
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.add_stream(
        61,
        "/Type/XObject/Subtype/Form/BBox [0 0 612 792]",
        b"1 1 1 rg 70 705 195 30 re f",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Page content that paints an opaque grey rectangle far from the GPA run:
/// it paints, but over no text, so it is geometry-gated out. Grey rather
/// than white so the control render visibly differs from [`gpa_text_page`].
pub fn grey_rect_elsewhere() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    add_flattened_body(
        &mut builder,
        b"BT /F1 12 Tf 72 720 Td (Cumulative GPA: 2.84) Tj ET\n\
          0.5 0.5 0.5 rg 400 100 100 30 re f",
        &format!("/Font<</F1 {FONT} 0 R>>"),
    );
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Page content that covers the GPA run with a half-alpha white rectangle:
/// it paints over the glyphs, but translucent paint is not an occlusion
/// candidate, so the analysis must still stay silent. Lowercase `/ca` is
/// the fill (nonstroking) alpha — the key the renderer reads onto fills.
pub fn translucent_rect_over_text() -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    add_flattened_body(
        &mut builder,
        b"BT /F1 12 Tf 72 720 Td (Cumulative GPA: 2.84) Tj ET\n\
          /GS1 gs 1 1 1 rg 70 705 195 30 re f",
        &format!("/Font<</F1 {FONT} 0 R>>/ExtGState<</GS1 60 0 R>>"),
    );
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.add_object(60, "<</ca 0.5>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Object numbers for the signed fixtures below.
pub mod signed_layout {
    /// Signature form field (`/FT /Sig` widget).
    pub const FIELD: u32 = 4;
    /// Signature value dictionary (`/Type /Sig`).
    pub const VALUE: u32 = 5;
    /// `/AcroForm` dictionary.
    pub const ACROFORM: u32 = 6;
    /// Bare text field (`/FT /Tx`), empty until the post-signature fill.
    pub const TEXT_FIELD: u32 = 7;
    /// Second bare text field, for FieldMDP include/exclude lock coverage.
    pub const TEXT_FIELD_SECOND: u32 = 8;
    /// Widget text field, for the appearance-regenerating fill.
    pub const WIDGET_FIELD: u32 = 7;
    /// The widget field's appearance stream before the fill.
    pub const AP_FIRST: u32 = 70;
    /// The widget field's appearance stream after the fill.
    pub const AP_SECOND: u32 = 71;
    /// Plain `/Text` annotation, for the annotation-permission fixtures.
    pub const NOTE: u32 = 60;
}

/// Ten zero-padded digits: the fixed-width `/ByteRange` slots below, so a
/// patched range never shifts a byte and every recorded offset stays valid.
const RANGE_SLOT: &str = "0000000000";

/// The `/ByteRange` placeholder as written by [`add_signed_body`].
fn range_placeholder() -> String {
    format!("[{RANGE_SLOT} {RANGE_SLOT} {RANGE_SLOT} {RANGE_SLOT}]")
}

/// One-page document with a `/FT /Sig` field whose `/V` value dictionary
/// carries a `/ByteRange` placeholder and a `/Contents` hex hole of
/// `contents_hex_len` zero nibbles — the shape a signer prepares before
/// writing the range. No xref yet; the caller finishes the revision, then
/// patches the placeholder with [`patch_signed_range`].
fn add_signed_body(builder: &mut PdfBuilder, contents_hex_len: usize) {
    add_signed_body_full(builder, contents_hex_len, "", &[], &[], &[]);
}

/// [`add_signed_body`] with a signature-policy lock, bare text fields, and
/// extra page annotations: the shared base for the DocMDP/FieldMDP fixtures.
///
/// `sig_extra` is spliced into the `/V` value dictionary (a `/Reference`
/// lock); `text_fields` are `(number, name)` bare `/FT /Tx` fields, empty
/// until the post-signature fill; `extra_annots` are `(number, body)`
/// page annotations appended to the page's `/Annots`; `annot_fields` lists
/// the extra annotations that are also form fields, so they join the
/// `/AcroForm` `/Fields` alongside the signature field and the bare fields.
fn add_signed_body_full(
    builder: &mut PdfBuilder,
    contents_hex_len: usize,
    sig_extra: &str,
    text_fields: &[(u32, &str)],
    extra_annots: &[(u32, &str)],
    annot_fields: &[u32],
) {
    use layout::*;
    builder.add_object(
        CATALOG,
        &format!(
            "<</Type/Catalog/Pages {PAGES} 0 R/AcroForm {} 0 R>>",
            signed_layout::ACROFORM
        ),
    );
    builder.add_object(
        PAGES,
        &format!(
            "<</Type/Pages/Kids[{} 0 R]/Count 1/MediaBox[0 0 612 792]>>",
            page(0)
        ),
    );
    let mut annots = format!("{} 0 R", signed_layout::FIELD);
    for (number, _) in extra_annots {
        annots.push_str(&format!(" {number} 0 R"));
    }
    builder.add_object(
        page(0),
        &format!(
            "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R/Annots [{annots}]\
             /Resources<</Font<</F1 {FONT} 0 R>>>>>>",
            content(0)
        ),
    );
    builder.add_stream(content(0), "", &content_bytes(0));
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
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
             /M(D:20241201120000Z)/ByteRange{}/Contents<{contents}>{sig_extra}>>",
            range_placeholder()
        ),
    );
    let mut fields = format!("{} 0 R", signed_layout::FIELD);
    for (number, _) in text_fields {
        fields.push_str(&format!(" {number} 0 R"));
    }
    for number in annot_fields {
        fields.push_str(&format!(" {number} 0 R"));
    }
    builder.add_object(signed_layout::ACROFORM, &format!("<</Fields [{fields}]>>"));
    for (number, name) in text_fields {
        builder.add_object(*number, &format!("<</FT/Tx/T({name})/V()>>"));
    }
    for (number, body) in extra_annots {
        builder.add_object(*number, body);
    }
}

/// Absolute span of the `/Contents` hex hole (the `<…>` brackets
/// included): what a real signer's two covered ranges straddle.
fn contents_hole(bytes: &[u8]) -> (u64, u64) {
    let open = bytes
        .windows(10)
        .position(|w| w == b"/Contents<")
        .map(|pos| pos + 9)
        .expect("signed body carries a /Contents hole");
    let close = bytes[open..]
        .iter()
        .position(|b| *b == b'>')
        .map(|rel| open + rel + 1)
        .expect("the /Contents hole is closed");
    (open as u64, close as u64)
}

/// Patch the `/ByteRange` placeholder in `bytes` (or in a builder's
/// in-progress bytes) with the four numbers `range`, zero-padded to the
/// fixed slot width. The patch shifts no bytes.
fn patch_signed_range(bytes: &mut [u8], range: [u64; 4]) {
    let needle = range_placeholder();
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle.as_bytes())
        .expect("the /ByteRange placeholder is present");
    let mut pos = at + 1; // past the opening bracket
    for value in range {
        let text = format!("{value:010}");
        assert_eq!(text.len(), 10, "fixture ranges fit ten digits");
        bytes[pos..pos + 10].copy_from_slice(text.as_bytes());
        pos += 11; // slot plus the following space or bracket
    }
}

/// Patch the placeholder against `end`: the assumption every signed
/// fixture below shares is that the first covered run is `[0, hole_open)`
/// and the second starts at `hole_close`.
fn straddling_range(bytes: &[u8], end: u64) -> [u64; 4] {
    let (hole_open, hole_close) = contents_hole(bytes);
    [0, hole_open, hole_close, end - hole_close]
}

/// A signed one-page document whose `/ByteRange` straddles the `/Contents`
/// hole and spans the whole file: the valid shape.
pub fn signed_valid() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_signed_body(&mut builder, 512);
    builder.finish_classic_xref("/Root 1 0 R");
    let mut bytes = builder.into_bytes();
    let end = bytes.len() as u64;
    let range = straddling_range(&bytes, end);
    patch_signed_range(&mut bytes, range);
    bytes
}

/// Like [`signed_valid`], but the second run ends 100 bytes past the end
/// of the file: the truncated shape.
pub fn signed_truncated() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_signed_body(&mut builder, 512);
    builder.finish_classic_xref("/Root 1 0 R");
    let mut bytes = builder.into_bytes();
    let end = bytes.len() as u64 + 100;
    let range = straddling_range(&bytes, end);
    patch_signed_range(&mut bytes, range);
    bytes
}

/// Like [`signed_valid`], but the second run starts 50 bytes inside the
/// first run while still ending at the end of the file: the overlapping
/// shape.
pub fn signed_overlapping() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_signed_body(&mut builder, 512);
    builder.finish_classic_xref("/Root 1 0 R");
    let mut bytes = builder.into_bytes();
    let (hole_open, _) = contents_hole(&bytes);
    let end = bytes.len() as u64;
    let range = [0, hole_open, hole_open - 50, end - (hole_open - 50)];
    patch_signed_range(&mut bytes, range);
    bytes
}

/// Like [`signed_valid`], but the second run stops 20 bytes before the end
/// of the file: trailing bytes its own revision cannot explain — the
/// gap-containing shape.
pub fn signed_with_gap() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_signed_body(&mut builder, 512);
    builder.finish_classic_xref("/Root 1 0 R");
    let mut bytes = builder.into_bytes();
    let end = bytes.len() as u64 - 20;
    let range = straddling_range(&bytes, end);
    patch_signed_range(&mut bytes, range);
    bytes
}

/// A signed one-page document whose literal `/ByteRange` array content is
/// `range` (for example `"0 100 200"`): the malformed-shape fixture for
/// odd counts, negative values, and non-integer entries. The range is
/// written before the xref, so the file stays structurally sound whatever
/// the range claims.
pub fn signed_with_range(range: &str) -> Vec<u8> {
    use layout::*;
    let mut builder = PdfBuilder::new();
    builder.add_object(
        CATALOG,
        &format!(
            "<</Type/Catalog/Pages {PAGES} 0 R/AcroForm {} 0 R>>",
            signed_layout::ACROFORM
        ),
    );
    builder.add_object(
        PAGES,
        &format!(
            "<</Type/Pages/Kids[{} 0 R]/Count 1/MediaBox[0 0 612 792]>>",
            page(0)
        ),
    );
    builder.add_object(
        page(0),
        &format!(
            "<</Type/Page/Parent {PAGES} 0 R/Contents {} 0 R/Annots [{} 0 R]\
             /Resources<</Font<</F1 {FONT} 0 R>>>>>>",
            content(0),
            signed_layout::FIELD
        ),
    );
    builder.add_stream(content(0), "", &content_bytes(0));
    builder.add_object(FONT, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    builder.add_object(
        signed_layout::FIELD,
        &format!(
            "<</Type/Annot/Subtype/Widget/FT/Sig/T(Sig1)/Rect[0 0 10 10]/V {} 0 R>>",
            signed_layout::VALUE
        ),
    );
    let contents = "0".repeat(512);
    builder.add_object(
        signed_layout::VALUE,
        &format!(
            "<</Type/Sig/Filter/Adobe.PPKLite/SubFilter/adbe.pkcs7.detached\
             /M(D:20241201120000Z)/ByteRange[{range}]/Contents<{contents}>>>"
        ),
    );
    builder.add_object(
        signed_layout::ACROFORM,
        &format!("<</Fields [{} 0 R]>>", signed_layout::FIELD),
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a validly signed first save, then an incremental update
/// appending one object. The signature's range is gap-containing against
/// the whole file, but it ends exactly at its own revision's `%%EOF` —
/// the legitimate post-signing update, which must not read as invalid.
pub fn signed_then_updated() -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    add_signed_body(&mut builder, 512);
    builder.finish_classic_xref("/Root 1 0 R");
    // Patch mid-build, against the first save's end: the patch shifts no
    // bytes, so the finished revision's offsets stay valid and the range
    // genuinely covers the file as it was when signed.
    let end = builder.bytes().len() as u64;
    let range = straddling_range(builder.bytes(), end);
    let mut patched = builder.bytes().to_vec();
    patch_signed_range(&mut patched, range);
    builder.patch_bytes(0, &patched);
    builder.add_object(50, "<</Kind(appended-after-signing)>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// A DocMDP `/Reference` lock for permission `p` (1–3), spliced into a
/// signed fixture's `/V` dictionary.
fn docmdp_reference(p: i64) -> String {
    format!(
        "/Reference[<</Type/SigRef/TransformMethod/DocMDP/TransformParams<</Type/TransformParams/P {p}/V/1.2>>>>]"
    )
}

/// A FieldMDP `/Reference` lock over `fields` per `action` (`All`,
/// `Include`, or `Exclude`), spliced into a signed fixture's `/V`
/// dictionary.
fn fieldmdp_reference(action: &str, fields: &[&str]) -> String {
    let names = fields
        .iter()
        .map(|name| format!("({name})"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "/Reference[<</Type/SigRef/TransformMethod/FieldMDP/TransformParams<</Type/TransformParams/Action/{action}/Fields[{names}]>>>>]"
    )
}

/// Start a locked signed first save (no xref yet): the signed base with the
/// given `/Reference` lock, bare text fields, and extra page annotations.
/// The caller adds any further first-save objects (such as the widget
/// field's initial appearance), then finishes via
/// [`finish_signed_first_save`].
fn signed_base_builder(
    sig_extra: &str,
    text_fields: &[(u32, &str)],
    extra_annots: &[(u32, &str)],
    annot_fields: &[u32],
) -> PdfBuilder {
    let mut builder = PdfBuilder::new();
    add_signed_body_full(
        &mut builder,
        512,
        sig_extra,
        text_fields,
        extra_annots,
        annot_fields,
    );
    builder
}

/// Finish the signed first save and patch its `/ByteRange` against the
/// save's own end, so the range genuinely covers the file as it was when
/// signed. The builder is then ready for the incremental revision.
fn finish_signed_first_save(builder: &mut PdfBuilder) {
    builder.finish_classic_xref("/Root 1 0 R");
    let end = builder.bytes().len() as u64;
    let range = straddling_range(builder.bytes(), end);
    let mut patched = builder.bytes().to_vec();
    patch_signed_range(&mut patched, range);
    builder.patch_bytes(0, &patched);
}

/// Two revisions: a DocMDP P=2 signed first save carrying one empty text
/// field, then an incremental update filling it (`/V (Ada Lovelace)`).
/// The permitted post-signature form fill: the single redefined field
/// object is form filling, so the lock stays silent.
pub fn signed_docmdp_p2_then_form_fill() -> Vec<u8> {
    use signed_layout::*;
    let mut builder =
        signed_base_builder(&docmdp_reference(2), &[(TEXT_FIELD, "full-name")], &[], &[]);
    finish_signed_first_save(&mut builder);
    builder.add_object(TEXT_FIELD, "<</FT/Tx/T(full-name)/V(Ada Lovelace)>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a DocMDP P=2 signed first save, then an incremental
/// update redefining page 0's content stream (`(Page 0)` becomes
/// `(Updated page 0)`). The violating post-signature page-content edit:
/// page content changes are not form filling.
pub fn signed_docmdp_p2_then_content_edit() -> Vec<u8> {
    use layout::*;
    use signed_layout::TEXT_FIELD;
    let mut builder =
        signed_base_builder(&docmdp_reference(2), &[(TEXT_FIELD, "full-name")], &[], &[]);
    finish_signed_first_save(&mut builder);
    builder.add_stream(content(0), "", &updated_content_bytes(0));
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a DocMDP P=1 signed first save carrying one empty text
/// field, then an incremental update filling it. P=1 permits no changes at
/// all, so even the form fill violates the lock.
pub fn signed_docmdp_p1_then_form_fill() -> Vec<u8> {
    use signed_layout::*;
    let mut builder =
        signed_base_builder(&docmdp_reference(1), &[(TEXT_FIELD, "full-name")], &[], &[]);
    finish_signed_first_save(&mut builder);
    builder.add_object(TEXT_FIELD, "<</FT/Tx/T(full-name)/V(Ada Lovelace)>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a DocMDP P=3 signed first save, then an incremental
/// update redefining page 0's content stream. P=3 additionally permits
/// annotation changes, but page content stays forbidden.
pub fn signed_docmdp_p3_then_content_edit() -> Vec<u8> {
    use layout::*;
    use signed_layout::TEXT_FIELD;
    let mut builder =
        signed_base_builder(&docmdp_reference(3), &[(TEXT_FIELD, "full-name")], &[], &[]);
    finish_signed_first_save(&mut builder);
    builder.add_stream(content(0), "", &updated_content_bytes(0));
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a DocMDP P=3 signed first save carrying a `/Text`
/// annotation, then an incremental update redefining the annotation in
/// place (a new `/Contents`). P=3 permits annotation changes, so the lock
/// stays silent.
pub fn signed_docmdp_p3_then_annot_edit() -> Vec<u8> {
    use signed_layout::*;
    let mut builder = signed_base_builder(
        &docmdp_reference(3),
        &[],
        &[(
            NOTE,
            "<</Type/Annot/Subtype/Text/Rect[72 700 90 718]/Contents(original note)/NM(note-1)>>",
        )],
        &[],
    );
    finish_signed_first_save(&mut builder);
    builder.add_object(
        NOTE,
        "<</Type/Annot/Subtype/Text/Rect[72 700 90 718]/Contents(edited note)/NM(note-1)>>",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a DocMDP P=2 signed first save carrying a `/Text`
/// annotation, then an incremental update redefining the annotation in
/// place. Annotation changes require P=3, so the edit violates the lock.
pub fn signed_docmdp_p2_then_annot_edit() -> Vec<u8> {
    use signed_layout::*;
    let mut builder = signed_base_builder(
        &docmdp_reference(2),
        &[],
        &[(
            NOTE,
            "<</Type/Annot/Subtype/Text/Rect[72 700 90 718]/Contents(original note)/NM(note-1)>>",
        )],
        &[],
    );
    finish_signed_first_save(&mut builder);
    builder.add_object(
        NOTE,
        "<</Type/Annot/Subtype/Text/Rect[72 700 90 718]/Contents(edited note)/NM(note-1)>>",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a DocMDP P=2 signed first save carrying a widget text
/// field with an appearance, then an incremental update filling the field
/// and regenerating its appearance stream. The permitted widget fill:
/// the redefined widget and its new appearance are form filling.
pub fn signed_docmdp_p2_then_widget_fill() -> Vec<u8> {
    use signed_layout::*;
    let mut builder = signed_base_builder(
        &docmdp_reference(2),
        &[],
        &[(
            WIDGET_FIELD,
            "<</Type/Annot/Subtype/Widget/FT/Tx/T(full-name)/V()/Rect[72 700 200 716]/AP<</N 70 0 R>>>>",
        )],
        &[WIDGET_FIELD],
    );
    builder.add_stream(
        AP_FIRST,
        "/Type/XObject/Subtype/Form/BBox[0 0 128 16]",
        b"0.9 0.9 0.9 rg 0 0 128 16 re f",
    );
    finish_signed_first_save(&mut builder);
    builder.add_object(
        WIDGET_FIELD,
        "<</Type/Annot/Subtype/Widget/FT/Tx/T(full-name)/V(Ada Lovelace)/Rect[72 700 200 716]/AP<</N 71 0 R>>>>",
    );
    builder.add_stream(
        AP_SECOND,
        "/Type/XObject/Subtype/Form/BBox[0 0 128 16]/Resources<</Font<</F1 101 0 R>>>>",
        b"0.9 0.9 0.9 rg 0 0 128 16 re f BT /F1 12 Tf 4 3 Td (Ada Lovelace) Tj ET",
    );
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a FieldMDP signed first save locking `full-name`
/// (`/Action /Include`), then an incremental update filling that field.
/// The locked fill violates the lock.
pub fn signed_fieldmdp_include_then_locked_fill() -> Vec<u8> {
    use signed_layout::*;
    let mut builder = signed_base_builder(
        &fieldmdp_reference("Include", &["full-name"]),
        &[(TEXT_FIELD, "full-name"), (TEXT_FIELD_SECOND, "city")],
        &[],
        &[],
    );
    finish_signed_first_save(&mut builder);
    builder.add_object(TEXT_FIELD, "<</FT/Tx/T(full-name)/V(Ada Lovelace)>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a FieldMDP signed first save locking `city`
/// (`/Action /Include`), then an incremental update filling the unlocked
/// `full-name` field. The unlocked fill is permitted, so the lock stays
/// silent.
pub fn signed_fieldmdp_include_then_unlocked_fill() -> Vec<u8> {
    use signed_layout::*;
    let mut builder = signed_base_builder(
        &fieldmdp_reference("Include", &["city"]),
        &[(TEXT_FIELD, "full-name"), (TEXT_FIELD_SECOND, "city")],
        &[],
        &[],
    );
    finish_signed_first_save(&mut builder);
    builder.add_object(TEXT_FIELD, "<</FT/Tx/T(full-name)/V(Ada Lovelace)>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a FieldMDP signed first save locking everything except
/// `city` (`/Action /Exclude`), then an incremental update filling
/// `full-name`. The field is outside the exclusion, so it is locked and
/// the fill violates the lock.
pub fn signed_fieldmdp_exclude_then_locked_fill() -> Vec<u8> {
    use signed_layout::*;
    let mut builder = signed_base_builder(
        &fieldmdp_reference("Exclude", &["city"]),
        &[(TEXT_FIELD, "full-name"), (TEXT_FIELD_SECOND, "city")],
        &[],
        &[],
    );
    finish_signed_first_save(&mut builder);
    builder.add_object(TEXT_FIELD, "<</FT/Tx/T(full-name)/V(Ada Lovelace)>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a FieldMDP signed first save locking every field
/// (`/Action /All`), then an incremental update filling one of them. Any
/// fill violates the lock.
pub fn signed_fieldmdp_all_then_fill() -> Vec<u8> {
    use signed_layout::*;
    let mut builder = signed_base_builder(
        &fieldmdp_reference("All", &[]),
        &[(TEXT_FIELD, "full-name"), (TEXT_FIELD_SECOND, "city")],
        &[],
        &[],
    );
    finish_signed_first_save(&mut builder);
    builder.add_object(TEXT_FIELD, "<</FT/Tx/T(full-name)/V(Ada Lovelace)>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a signed first save whose DocMDP entry carries `/P 9`,
/// outside the specified 1–3, then an incremental content edit. The
/// permission is unestablished: the later revision is reported, but no
/// violation is claimed.
pub fn signed_docmdp_p9_then_content_edit() -> Vec<u8> {
    use layout::*;
    use signed_layout::TEXT_FIELD;
    let mut builder =
        signed_base_builder(&docmdp_reference(9), &[(TEXT_FIELD, "full-name")], &[], &[]);
    finish_signed_first_save(&mut builder);
    builder.add_stream(content(0), "", &updated_content_bytes(0));
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}

/// Two revisions: a FieldMDP signed first save whose transform parameters
/// carry no `/Action`, then an incremental form fill. The lock set is
/// unestablished: the later revision is reported, but no violation is
/// claimed.
pub fn signed_fieldmdp_no_action_then_fill() -> Vec<u8> {
    use signed_layout::*;
    let reference = "/Reference[<</Type/SigRef/TransformMethod/FieldMDP/TransformParams<</Type/TransformParams/Fields[(full-name)]>>>>]";
    let mut builder = signed_base_builder(
        reference,
        &[(TEXT_FIELD, "full-name"), (TEXT_FIELD_SECOND, "city")],
        &[],
        &[],
    );
    finish_signed_first_save(&mut builder);
    builder.add_object(TEXT_FIELD, "<</FT/Tx/T(full-name)/V(Ada Lovelace)>>");
    builder.finish_classic_xref("/Root 1 0 R");
    builder.into_bytes()
}
