#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: panics are the assertion mechanism

//! N1 / A8: the resolver reads an object through a 64 KiB window that grows
//! when the parse runs out of bytes. A stream whose correct `/Length` runs
//! past the window used to fall into the parser's wrong-length repair (scan
//! for the first `endstream`/`endobj`) whenever such a keyword appeared in
//! the window, so the body was truncated at the keyword and the window was
//! never grown. The resolver now grows once to the declared end before it
//! accepts a repair; a genuinely wrong length still repairs as before.

use std::sync::Arc;

use pdf_document::{DocumentLimits, DocumentSnapshot, ParseContext};
use pdf_object::{ObjectId, PdfObject};
use pdf_source::{OwnedBytesSource, PdfSource};
use pdf_structure::RecoveryEvent;
use pdf_test_support::builder::PdfBuilder;

const STREAM: u32 = 4;

fn open_bytes(bytes: Vec<u8>) -> DocumentSnapshot {
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes));
    DocumentSnapshot::open(source, DocumentLimits::default()).expect("open failed")
}

/// 150,000 ASCII bytes whose first 200 contain `endstream` and `endobj`
/// (an embedded PDF, a raw object dump in a text stream).
fn body() -> String {
    let mut s = String::from(
        "% embedded object dump\n1 0 obj\n<</Length 3>>\nstream\nabc\nendstream\nendobj\n",
    );
    assert!(s.len() < 200);
    let mut i = 0u32;
    while s.len() < 150_000 {
        s.push_str(&format!("{:08} filler line\n", i));
        i += 1;
    }
    s.truncate(150_000);
    s
}

/// A document whose object `STREAM` carries `body` under `length` (a
/// literal or an indirect reference), followed by `tail` bytes of padding so
/// the file is larger than the declared end.
fn document(body: &str, length: &str, indirect_length: Option<usize>, tail: usize) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add_object(1, "<</Type/Catalog/Pages 2 0 R>>");
    b.add_object(
        2,
        "<</Type/Pages/Kids[3 0 R]/Count 1/MediaBox[0 0 612 792]>>",
    );
    b.add_object(3, "<</Type/Page/Parent 2 0 R>>");
    b.add_object(
        STREAM,
        &format!("<</Length {length}>>\nstream\n{body}\nendstream"),
    );
    if let Some(n) = indirect_length {
        b.add_object(5, &n.to_string());
    }
    if tail > 0 {
        b.add_stream(6, "", &vec![b'.'; tail]);
    }
    b.finish_classic_xref("/Root 1 0 R");
    b.into_bytes()
}

/// Resolve `STREAM` on a fresh worker context: its raw body and the
/// stream-length repairs the resolution recorded.
fn resolve_stream(bytes: Vec<u8>) -> (Vec<u8>, Vec<RecoveryEvent>) {
    let snapshot = open_bytes(bytes);
    let mut ctx = ParseContext::new();
    let object = snapshot
        .objects()
        .resolve(&snapshot, ObjectId::new(STREAM, 0), &mut ctx)
        .unwrap();
    let PdfObject::Stream(stream) = &*object else {
        panic!("not a stream: {object:?}")
    };
    let data = snapshot.decode_stream_data(stream, &mut ctx).unwrap();
    let repairs = ctx
        .recovery
        .iter()
        .filter(|e| matches!(e, RecoveryEvent::StreamLengthRepaired { .. }))
        .cloned()
        .collect();
    (data, repairs)
}

#[test]
fn correct_length_stream_past_the_window_is_not_truncated_at_an_inner_endstream() {
    let body = body();
    let (data, repairs) = resolve_stream(document(&body, "150000", None, 0));
    assert!(repairs.is_empty(), "spurious repair: {repairs:?}");
    assert_eq!(data.len(), 150_000);
    assert_eq!(data, body.as_bytes());
}

#[test]
fn indirect_length_stream_past_the_window_is_not_truncated_at_an_inner_endstream() {
    let body = body();
    let (data, repairs) = resolve_stream(document(&body, "5 0 R", Some(150_000), 0));
    assert!(repairs.is_empty(), "spurious repair: {repairs:?}");
    assert_eq!(data, body.as_bytes());
}

/// Control: a genuinely wrong (too long) `/Length` that also runs past the
/// window still repairs to the real body, after the one grown read.
#[test]
fn wrong_length_past_the_window_still_repairs() {
    let body = "BT /F1 12 Tf (fifty bytes of real content here....) Tj ET";
    let (data, repairs) = resolve_stream(document(body, "200000", None, 300_000));
    assert_eq!(data, body.as_bytes());
    assert!(
        matches!(
            repairs.as_slice(),
            [RecoveryEvent::StreamLengthRepaired {
                declared: Some(200_000),
                actual,
                ..
            }] if *actual == body.len() as u64
        ),
        "{repairs:?}"
    );
}

/// Control: a stream shorter than the window with `endstream` text inside
/// and a correct length was always read whole.
#[test]
fn correct_length_stream_inside_the_window_is_read_whole() {
    let body = body()[..20_000].to_string();
    let (data, repairs) = resolve_stream(document(&body, "20000", None, 0));
    assert!(repairs.is_empty(), "{repairs:?}");
    assert_eq!(data, body.as_bytes());
}
