use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct InspectData {
    pub open: OpenView,
    pub page_count: u32,
    pub version: Option<String>,
    pub encryption: Option<EncryptionView>,
    pub xref: XrefView,
    pub features: FeaturesView,
    pub annotations: AnnotationView,
    pub outline_item_count: usize,
    /// The outline (bookmarks) in document order, up to the outline limit.
    pub outline: Vec<OutlineItemView>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub outline_truncated: bool,
    pub metadata: MetadataView,
    /// Displayed page sizes in points, most common first.
    pub page_sizes: Vec<PageSizeView>,
    /// Runs of pages whose printed number differs from their position.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub page_labels: Vec<PageLabelView>,
    /// What a few pages spread through the document contain, so a reader
    /// knows whether there is a text layer before asking for text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_sample: Option<TextSampleView>,
    /// Per-page boxes and compile status, for the pages asked for with
    /// `pages`; empty otherwise.
    pub pages: Vec<PageView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutlineItemView {
    pub title: String,
    /// One-based page the item points at, when it points at one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    pub depth: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageSizeView {
    pub width: f64,
    pub height: f64,
    pub count: u32,
    /// The pages of this size, as a page-range string.
    pub pages: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageLabelView {
    /// The pages of the run, as a page-range string.
    pub pages: String,
    /// The printed numbers of its first and last page.
    pub first: String,
    pub last: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextSampleView {
    /// The one-based pages looked at.
    pub pages: Vec<u32>,
    /// How many of them are of each kind: `native-text`, `hybrid`,
    /// `scanned-image` (needs OCR), `render-required`, `blank`.
    pub kinds: std::collections::BTreeMap<&'static str, usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OpenView {
    Ok,
    Recovered { repairs: Vec<String> },
    Failed { error: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct EncryptionView {
    pub version: i64,
    pub revision: i64,
    pub method: String,
    pub user_password_empty: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct XrefView {
    pub recovery_used: bool,
    pub rebuilt: bool,
    pub revision_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeaturesView {
    pub has_acroform: bool,
    pub has_xfa: bool,
    pub has_javascript: bool,
    pub has_outlines: bool,
    pub has_optional_content: bool,
    pub uses_object_streams: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnnotationView {
    pub total: u32,
    pub by_subtype: Vec<(String, u32)>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MetadataView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creation_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modification_date: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageView {
    pub page: u32,
    pub page_index: u32,
    pub media_box: [f64; 4],
    pub crop_box: [f64; 4],
    /// The non-inheritable prepress boxes. Each defaults to `crop_box` when
    /// the page does not declare it, so they are always present.
    pub bleed_box: [f64; 4],
    pub trim_box: [f64; 4],
    pub art_box: [f64; 4],
    pub rotate: u16,
    pub media_box_present: bool,
    pub compile: CompileView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CompileView {
    Ok { op_count: usize },
    Degraded { op_count: usize, detail: String },
    Failed { error: String },
    Unknown,
}
