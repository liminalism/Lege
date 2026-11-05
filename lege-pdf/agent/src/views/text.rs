use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct TextProvenance {
    /// `native` or `ocr`.
    pub source: &'static str,
    /// `native-text`, `scanned-image`, `hybrid`, `render-required`, or
    /// `blank` (the page draws nothing).
    pub page_content_kind: &'static str,
    pub native_text_trustworthy: bool,
    pub native_char_count: usize,
    /// Images drawn on the page.
    pub image_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr_engine: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr_language: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextPageData {
    pub unit: &'static str,
    pub layout: String,
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<WordView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chars: Option<Vec<CharView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<BlockView>>,
    pub char_count: usize,
    pub provenance: TextProvenance,
}

#[derive(Debug, Clone, Serialize)]
pub struct WordView {
    pub text: String,
    #[serde(serialize_with = "super::rounded_box")]
    pub bbox: [f64; 4],
    /// Where the word starts in the page text, in characters.
    pub first_char: usize,
    /// How many characters of the page text it covers: for a piece ending in
    /// a line-end hyphen, one fewer than its own text, as the page text
    /// leaves that hyphen out.
    pub char_count: usize,
    /// The word goes on at the start of the next line.
    pub continued: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CharView {
    pub unicode: u32,
    pub text: String,
    pub char_code: u32,
    pub cid: u32,
    pub glyph_id: u32,
    pub text_object: u32,
    #[serde(serialize_with = "super::rounded_box")]
    pub tight_box: [f64; 4],
    #[serde(serialize_with = "super::rounded_box")]
    pub loose_box: [f64; 4],
    pub char_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockView {
    pub text: String,
    #[serde(serialize_with = "super::rounded_box")]
    pub bbox: [f64; 4],
    pub lines: Vec<LineView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LineView {
    pub text: String,
    #[serde(serialize_with = "super::rounded_box")]
    pub bbox: [f64; 4],
    pub words: Vec<WordView>,
}
