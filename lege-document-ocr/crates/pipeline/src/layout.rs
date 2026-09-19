//! PP-DocLayout-M page layout for the document pipeline.
//!
//! The [`LayoutEngine`] runs the same prepared PP-DocLayout-M weights the
//! `lege` e-ink pipeline embeds (`lege-process/models/doclayout.onnx`,
//! 23 classes, see `doclayout-m/onnx-work/provenance.json`) over each page
//! raster and groups text evidence into layout boxes:
//!
//! * page furniture (`header`, `footer`, `number`, header/footer images) is
//!   dropped, so running heads and page numbers never reach the reading text;
//! * `doc_title` becomes a [`lege_docir::RegionKind::Title`] and
//!   `paragraph_title` a [`lege_docir::RegionKind::Heading`], so document and
//!   chapter titles format as `#` / `##` in Markdown;
//! * content illustrations (`image`, `seal`, `chart`) become
//!   [`lege_docir::RegionKind::Figure`] regions whose caption is a short
//!   placeholder (`[image]` / `[seal]` / `[chart]`), so every retained figure
//!   is visible in text exports instead of vanishing silently;
//! * every other text-like class (body `text`, `content`, `abstract`,
//!   `footnote`, captions, `table`, `formula`, …) keeps its OCR or native
//!   text, classified by kind.
//!
//! Lines or words that fall outside every detection are kept as plain
//! paragraphs: layout refines recall, it never reduces it. Reading order is
//! column-aware via [`lege_ocr::reading_order::order_bboxes`].

use std::path::Path;
use std::sync::Mutex;

use image::GrayImage;
use lege_docir::{
    Figure, GeometrySource, Provenance, RecognitionConfidence, Region, RegionConfidence,
    RegionContent, RegionKind, TextBlock, TextEvidence, TextLine, rect_polygon,
};

/// Embedded PP-DocLayout-M weights (fp16, 640x640 `pp_image` input).
/// Same artifact `lege-process` embeds; kept as the single source file so the
/// standalone OCR binary works with no external assets.
static EMBEDDED_LAYOUT_MODEL: &[u8] =
    include_bytes!("../../../../lege-process/models/doclayout.onnx");

/// Provenance marker for regions grouped by this module. The deterministic
/// geometry heuristic in `lib.rs` leaves these regions alone.
pub const LAYOUT_GROUP_PREPROCESSING: &str = "doclayout-m-grouped";

/// Provenance marker for uncertain page-number candidates. The document pass
/// drops them only when the same margin spot repeats across pages.
pub const LAYOUT_NUMBER_PREPROCESSING: &str = "doclayout-m-number";

/// Model identity recorded in the processing manifest when layout runs.
pub const LAYOUT_MODEL_PROVIDER: &str = "PaddlePaddle";
/// Model identity recorded in the processing manifest when layout runs.
pub const LAYOUT_MODEL_NAME: &str = "PP-DocLayout-M";
/// Model identity recorded in the processing manifest when layout runs.
pub const LAYOUT_MODEL_VERSION: &str = "fp16-640-picodef";
/// Model identity recorded in the processing manifest when layout runs.
pub const LAYOUT_MODEL_SOURCE: &str = "https://huggingface.co/PaddlePaddle/PP-DocLayout-M";

/// What a layout class means for the reading document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutRole {
    /// Confident page number: never reaches reading text.
    Drop,
    /// Possible page number: kept unless a later pass confirms it.
    Number,
    /// Possible running head: kept as a candidate; the document pass drops
    /// only repeats across pages (low-confidence singletons are usually
    /// title-page titles the model mislabels).
    Header,
    /// Possible page footer: same repeat rule as [`LayoutRole::Header`].
    Footer,
    /// Document title (`#` in Markdown).
    Title,
    /// Chapter/section title (`##` in Markdown).
    Heading,
    /// Figure/table/chart caption.
    Caption,
    /// Ordinary reading text.
    Paragraph,
    /// Tabular text (kept as text; the table specialist may upgrade it).
    Table,
    /// Content illustration: emit a placeholder figure.
    Figure,
}

/// Page numbers at or above this confidence are dropped on sight. Measured
/// 0.73-0.81 on real page numbers; title-page lookalikes score far lower.
pub const NUMBER_DROP_CONFIDENCE: f32 = 0.5;

/// A header/footer text candidate must repeat on this many pages before the
/// document pass treats it as running furniture.
pub const FURNITURE_REPEAT_PAGES: usize = 2;

/// A placeholder figure must repeat on this many pages before the document
/// pass treats it as a logo/watermark rather than content.
pub const FIGURE_REPEAT_PAGES: usize = 3;

/// Classify a PP-DocLayout-M label. Unknown labels stay readable paragraphs;
/// only the known furniture and illustration classes change behavior.
pub fn role_for_label(label: &str) -> LayoutRole {
    match label {
        "number" => LayoutRole::Number,
        "header" => LayoutRole::Header,
        "footer" => LayoutRole::Footer,
        "doc_title" => LayoutRole::Title,
        "paragraph_title" => LayoutRole::Heading,
        "figure_title" | "table_title" | "chart_title" => LayoutRole::Caption,
        "table" => LayoutRole::Table,
        "image" | "seal" | "chart" | "header_image" | "footer_image" => LayoutRole::Figure,
        _ => LayoutRole::Paragraph,
    }
}

/// Placeholder caption for an illustration class.
pub fn placeholder_for_label(label: &str) -> &'static str {
    match label {
        "seal" => "[seal]",
        "chart" => "[chart]",
        _ => "[image]",
    }
}

/// One layout detection in page-pixel coordinates, decoupled from the
/// detector type so grouping stays unit-testable without a GPU.
#[derive(Debug, Clone)]
pub struct LayoutBox {
    /// Canonical PP-DocLayout-M class name (or `"text"` for synthetic boxes).
    pub label: String,
    /// Detector confidence, retained on the emitted regions.
    pub confidence: Option<f32>,
    /// `[x0, y0, x1, y1]` in the same pixel space as the grouped evidence.
    pub bbox: [f32; 4],
}

/// Outcome counters reported as page warnings.
#[derive(Debug, Clone, Default)]
pub struct LayoutStats {
    /// Text lines/words dropped as confident page numbers.
    pub dropped_furniture: usize,
    /// Header/footer candidates kept for the document-level repeat check.
    pub furniture_candidates: usize,
    /// Uncertain page numbers kept for the document-level position check.
    pub number_candidates: usize,
    /// Text lines/words covered by illustrations (replaced by placeholders).
    pub dropped_image_text: usize,
    /// Placeholder figures emitted.
    pub figures: usize,
    /// Text regions grouped from layout boxes.
    pub grouped_regions: usize,
    /// Lines/words outside every box, kept as plain paragraphs.
    pub unassigned: usize,
}

impl LayoutStats {
    /// Human-readable page warning, or `None` when layout changed nothing.
    pub fn warning(&self) -> Option<String> {
        if self.grouped_regions == 0
            && self.figures == 0
            && self.dropped_furniture == 0
            && self.furniture_candidates == 0
            && self.number_candidates == 0
        {
            return None;
        }
        Some(format!(
            "layout pp-doclayout-m grouped {} region(s), ignored {} page-number line(s), flagged {} running-head and {} page-number candidate(s), emitted {} image placeholder(s)",
            self.grouped_regions,
            self.dropped_furniture,
            self.furniture_candidates,
            self.number_candidates,
            self.figures,
        ))
    }
}

/// GPU layout session. `Send` (via the internal mutex) so the shared
/// [`crate::DocumentProcessor`] can serve concurrent batch workers; pages of
/// one document are processed sequentially and concurrent documents serialize
/// on the one inference session.
pub struct LayoutEngine {
    detector: Mutex<lege_gpu::vision::LayoutDetector>,
}

impl std::fmt::Debug for LayoutEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("LayoutEngine").finish()
    }
}

impl LayoutEngine {
    /// Build from the embedded PP-DocLayout-M weights.
    pub fn embedded() -> anyhow::Result<Self> {
        let detector = lege_gpu::vision::LayoutDetector::from_model_bytes(
            EMBEDDED_LAYOUT_MODEL,
            lege_gpu::vision::LayoutConfig::default(),
        )?;
        Ok(Self {
            detector: Mutex::new(detector),
        })
    }

    /// Build from an external prepared PP-DocLayout ONNX file
    /// (`--layout-model`), verified to parse before any page runs.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        let detector = lege_gpu::vision::LayoutDetector::from_model_bytes(
            &bytes,
            lege_gpu::vision::LayoutConfig::default(),
        )?;
        Ok(Self {
            detector: Mutex::new(detector),
        })
    }

    /// BLAKE3 of the active weights for the resumable configuration hash.
    pub fn model_hash(model_path: Option<&Path>) -> anyhow::Result<String> {
        let digest = match model_path {
            Some(path) => blake3::hash(&std::fs::read(path)?),
            None => blake3::hash(EMBEDDED_LAYOUT_MODEL),
        };
        Ok(format!("blake3:{}", digest.to_hex()))
    }

    /// Run detection on a page raster. Boxes are in page-pixel coordinates.
    pub fn detect(&self, page: &GrayImage) -> anyhow::Result<Vec<LayoutBox>> {
        let rgb = image::DynamicImage::ImageLuma8(page.clone()).to_rgb8();
        let detector = self
            .detector
            .lock()
            .map_err(|_| anyhow::anyhow!("layout detector lock poisoned"))?;
        let mut boxes = Vec::new();
        for detection in detector.detect_rgb(&rgb)? {
            let [x0, y0, x1, y1] = detection.bbox;
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            boxes.push(LayoutBox {
                label: detection.class_name.to_string(),
                confidence: Some(detection.confidence),
                bbox: [x0, y0, x1, y1],
            });
        }
        Ok(boxes)
    }

    /// Group OCR line results into layout boxes.
    pub fn group_ocr_lines(
        &self,
        page: &GrayImage,
        page_index: u32,
        lines: Vec<lege_ocr::types::OcrLineResult>,
        provider: &str,
        language: &str,
    ) -> anyhow::Result<(Vec<Region>, Vec<String>, LayoutStats)> {
        let boxes = self.detect(page)?;
        Ok(group_ocr_lines(
            page_index,
            page.width(),
            lines,
            &boxes,
            provider,
            language,
        ))
    }

    /// Group native PDF words into layout boxes. Requires a page raster for
    /// detection; the words themselves stay the evidence.
    pub fn group_native_words(
        &self,
        page: &GrayImage,
        page_index: u32,
        words: &[lege_pdf_read::NativeTextWord],
        language: &str,
    ) -> anyhow::Result<(Vec<Region>, Vec<String>, LayoutStats)> {
        let boxes = self.detect(page)?;
        Ok(group_native_words(
            page_index,
            page.width(),
            words,
            &boxes,
            language,
        ))
    }
}

/// Region kind for a text-like layout role.
fn region_kind(role: LayoutRole) -> RegionKind {
    match role {
        LayoutRole::Title => RegionKind::Title,
        LayoutRole::Heading => RegionKind::Heading,
        LayoutRole::Caption => RegionKind::Caption,
        LayoutRole::Table => RegionKind::Table,
        LayoutRole::Header => RegionKind::Header,
        LayoutRole::Footer => RegionKind::Footer,
        LayoutRole::Paragraph | LayoutRole::Drop | LayoutRole::Number | LayoutRole::Figure => {
            RegionKind::Paragraph
        }
    }
}

/// Index of the smallest-area box containing `(x, y)`, if any.
fn containing_box(boxes: &[LayoutBox], x: f32, y: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (index, box_) in boxes.iter().enumerate() {
        let [x0, y0, x1, y1] = box_.bbox;
        if x < x0 || x > x1 || y < y0 || y > y1 {
            continue;
        }
        let area = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
        if best.is_none_or(|(_, best_area)| area < best_area) {
            best = Some((index, area));
        }
    }
    best.map(|(index, _)| index)
}

fn union_bbox(boxes: &[[f32; 4]]) -> Option<[f32; 4]> {
    let mut acc: Option<[f32; 4]> = None;
    for bbox in boxes {
        acc = Some(match acc {
            None => *bbox,
            Some([x0, y0, x1, y1]) => [
                x0.min(bbox[0]),
                y0.min(bbox[1]),
                x1.max(bbox[2]),
                y1.max(bbox[3]),
            ],
        });
    }
    acc
}

fn line_bbox_f32(bbox: &[u32; 4]) -> [f32; 4] {
    [
        bbox[0] as f32,
        bbox[1] as f32,
        bbox[2] as f32,
        bbox[3] as f32,
    ]
}

fn mean_confidence(values: &[Option<f32>]) -> Option<f32> {
    let mut sum = 0.0_f32;
    let mut count = 0_u32;
    for value in values.iter().flatten() {
        sum += *value;
        count += 1;
    }
    (count > 0).then_some(sum / count as f32)
}

fn layout_provenance_with(provider: &str, language: &str, preprocessing: &str) -> Provenance {
    Provenance {
        provider: provider.to_string(),
        model: Some(LAYOUT_MODEL_NAME.to_string()),
        preprocessing: Some(preprocessing.to_string()),
        language: Some(language.to_string()),
    }
}

/// Provenance marker for a grouped box: uncertain page numbers carry their
/// own marker so the document pass can drop them by repeated margin
/// position. Keyed off the label (not the role) because low-confidence
/// numbers are already demoted to readable paragraphs by grouping time.
fn preprocessing_for(label: &str) -> &'static str {
    if label == "number" {
        LAYOUT_NUMBER_PREPROCESSING
    } else {
        LAYOUT_GROUP_PREPROCESSING
    }
}

fn placeholder_region(
    id: String,
    box_: &LayoutBox,
    language: &str,
) -> Region {
    let [x0, y0, x1, y1] = box_.bbox;
    let polygon = rect_polygon(x0, y0, x1, y1);
    let provenance = Provenance {
        provider: "pp-doclayout-m".to_string(),
        model: Some(LAYOUT_MODEL_NAME.to_string()),
        preprocessing: None,
        language: Some(language.to_string()),
    };
    let caption = placeholder_for_label(&box_.label).to_string();
    Region {
        id,
        kind: RegionKind::Figure,
        polygon: polygon.clone(),
        confidence: RegionConfidence {
            detection: box_.confidence,
            layout: box_.confidence,
            recognition: None,
        },
        content: RegionContent::Figure(Figure {
            source_crop: None,
            caption: Some(TextBlock {
                lines: vec![TextLine {
                    text: TextEvidence::raw(caption),
                    polygon,
                    confidence: RecognitionConfidence::default(),
                    words: Vec::new(),
                    provenance: provenance.clone(),
                }],
            }),
        }),
        provenance,
    }
}

/// Column-aware reading order over final region bboxes.
pub fn reading_order(regions: &[Region], page_width: u32) -> Vec<String> {
    order_regions(regions, page_width)
}

/// Column-aware reading order over final region bboxes.
fn order_regions(regions: &[Region], page_width: u32) -> Vec<String> {
    let bboxes = regions
        .iter()
        .map(|region| {
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for point in &region.polygon {
                x0 = x0.min(point.x);
                y0 = y0.min(point.y);
                x1 = x1.max(point.x);
                y1 = y1.max(point.y);
            }
            if x1 < x0 || y1 < y0 {
                return [0, 0, 0, 0];
            }
            [
                x0.max(0.0) as u32,
                y0.max(0.0) as u32,
                x1.max(0.0) as u32,
                y1.max(0.0) as u32,
            ]
        })
        .collect::<Vec<_>>();
    lege_ocr::reading_order::order_bboxes(&bboxes, page_width.max(1))
        .into_iter()
        .map(|index| regions[index].id.clone())
        .collect()
}

/// Group OCR lines into layout boxes (pure; no GPU needed).
pub fn group_ocr_lines(
    page_index: u32,
    page_width: u32,
    lines: Vec<lege_ocr::types::OcrLineResult>,
    boxes: &[LayoutBox],
    provider: &str,
    language: &str,
) -> (Vec<Region>, Vec<String>, LayoutStats) {
    let mut stats = LayoutStats::default();
    let mut members = vec![Vec::<usize>::new(); boxes.len()];
    let mut unassigned = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let [x0, y0, x1, y1] = line.bbox_highres;
        let center = (
            (x0.saturating_add(x1)) as f32 * 0.5,
            (y0.saturating_add(y1)) as f32 * 0.5,
        );
        match containing_box(boxes, center.0, center.1) {
            Some(box_index) => members[box_index].push(line_index),
            None => unassigned.push(line_index),
        }
    }
    let mut regions = Vec::new();
    let mut group_counter = 0_u32;
    for (box_index, box_) in boxes.iter().enumerate() {
        let role = role_for_label(&box_.label);
        let member_lines = &members[box_index];
        // Confident page numbers drop on sight; uncertain ones stay readable
        // for the document pass (a section number is worse to lose than a
        // page number is to keep).
        let role = match role {
            LayoutRole::Number
                if box_.confidence.is_some_and(|confidence| {
                    confidence >= NUMBER_DROP_CONFIDENCE
                }) =>
            {
                LayoutRole::Drop
            }
            LayoutRole::Number => LayoutRole::Paragraph,
            role => role,
        };
        match role {
            LayoutRole::Drop => {
                stats.dropped_furniture += member_lines.len();
            }
            LayoutRole::Figure => {
                stats.figures += 1;
                stats.dropped_image_text += member_lines.len();
                regions.push(placeholder_region(
                    format!("p{page_index}-g{group_counter}"),
                    box_,
                    language,
                ));
                group_counter += 1;
            }
            _ => {
                if member_lines.is_empty() {
                    continue;
                }
                // Keep input order: OCR lines already arrive in detector
                // reading order, and re-sorting by (y, x) would scramble
                // words along a line on baseline jitter.
                let ordered = member_lines.clone();
                let bboxes = ordered
                    .iter()
                    .map(|&index| line_bbox_f32(&lines[index].bbox_highres))
                    .collect::<Vec<_>>();
                let Some(union) = union_bbox(&bboxes) else {
                    continue;
                };
                let polygon = rect_polygon(union[0], union[1], union[2], union[3]);
                let confidences = ordered
                    .iter()
                    .map(|&index| lines[index].confidence)
                    .collect::<Vec<_>>();
                // Move the recognized lines into the grouped region,
                // re-homing word polygons is unnecessary: they are crop-local.
                let preprocessing = preprocessing_for(&box_.label);
                let mut text_lines = Vec::with_capacity(ordered.len());
                for &index in &ordered {
                    let [left, top, right, bottom] = lines[index].bbox_highres;
                    let line = &lines[index];
                    let words = line
                        .words
                        .iter()
                        .map(|word| lege_docir::TextWord {
                            text: TextEvidence::raw(word.text.clone()),
                            polygon: rect_polygon(
                                (left + word.bbox_crop_local[0]) as f32,
                                (top + word.bbox_crop_local[1]) as f32,
                                (left + word.bbox_crop_local[2]) as f32,
                                (top + word.bbox_crop_local[3]) as f32,
                            ),
                            confidence: RecognitionConfidence {
                                mean_token: word.confidence,
                                ..Default::default()
                            },
                            geometry_source: if provider == "paddle" {
                                GeometrySource::CtcEstimated
                            } else {
                                GeometrySource::OcrBackend
                            },
                        })
                        .collect();
                    text_lines.push(TextLine {
                        text: TextEvidence::raw(line.text.clone()),
                        polygon: rect_polygon(
                            left as f32,
                            top as f32,
                            right as f32,
                            bottom as f32,
                        ),
                        confidence: RecognitionConfidence {
                            mean_token: line.confidence,
                            ..Default::default()
                        },
                        words,
                        provenance: layout_provenance_with(provider, language, preprocessing),
                    });
                }
                let provenance = layout_provenance_with(provider, language, preprocessing);
                regions.push(Region {
                    id: format!("p{page_index}-g{group_counter}"),
                    kind: region_kind(role),
                    polygon,
                    confidence: RegionConfidence {
                        detection: None,
                        layout: box_.confidence,
                        recognition: mean_confidence(&confidences),
                    },
                    content: RegionContent::Text(TextBlock { lines: text_lines }),
                    provenance,
                });
                group_counter += 1;
                stats.grouped_regions += 1;
                if matches!(role, LayoutRole::Header | LayoutRole::Footer) {
                    stats.furniture_candidates += 1;
                } else if box_.label == "number" {
                    stats.number_candidates += 1;
                }
            }
        }
    }
    // Lines outside every box keep their evidence as plain paragraphs.
    for &line_index in &unassigned {
        let line = &lines[line_index];
        let [left, top, right, bottom] = line.bbox_highres;
        let provenance = Provenance {
            provider: provider.to_string(),
            model: None,
            preprocessing: None,
            language: Some(language.to_string()),
        };
        regions.push(single_ocr_region(
            format!("p{page_index}-r{line_index}"),
            line,
            rect_polygon(left as f32, top as f32, right as f32, bottom as f32),
            provider,
            provenance,
        ));
        stats.unassigned += 1;
    }
    let reading_order = order_regions(&regions, page_width);
    (regions, reading_order, stats)
}

fn single_ocr_region(
    id: String,
    line: &lege_ocr::types::OcrLineResult,
    polygon: Vec<lege_docir::Point>,
    provider: &str,
    provenance: Provenance,
) -> Region {
    let [left, top, right, bottom] = line.bbox_highres;
    let words = line
        .words
        .iter()
        .map(|word| lege_docir::TextWord {
            text: TextEvidence::raw(word.text.clone()),
            polygon: rect_polygon(
                (left + word.bbox_crop_local[0]) as f32,
                (top + word.bbox_crop_local[1]) as f32,
                (left + word.bbox_crop_local[2]) as f32,
                (top + word.bbox_crop_local[3]) as f32,
            ),
            confidence: RecognitionConfidence {
                mean_token: word.confidence,
                ..Default::default()
            },
            geometry_source: if provider == "paddle" {
                GeometrySource::CtcEstimated
            } else {
                GeometrySource::OcrBackend
            },
        })
        .collect();
    Region {
        id,
        kind: RegionKind::Paragraph,
        polygon,
        confidence: RegionConfidence {
            recognition: line.confidence,
            ..Default::default()
        },
        content: RegionContent::Text(TextBlock {
            lines: vec![TextLine {
                text: TextEvidence::raw(line.text.clone()),
                polygon: rect_polygon(left as f32, top as f32, right as f32, bottom as f32),
                confidence: RecognitionConfidence {
                    mean_token: line.confidence,
                    ..Default::default()
                },
                words,
                provenance: provenance.clone(),
            }],
        }),
        provenance,
    }
}

/// Group native PDF words into layout boxes (pure; no GPU needed).
pub fn group_native_words(
    page_index: u32,
    page_width: u32,
    words: &[lege_pdf_read::NativeTextWord],
    boxes: &[LayoutBox],
    language: &str,
) -> (Vec<Region>, Vec<String>, LayoutStats) {
    let mut stats = LayoutStats::default();
    let mut members = vec![Vec::<usize>::new(); boxes.len()];
    let mut unassigned = Vec::new();
    for (word_index, word) in words.iter().enumerate() {
        let [x0, y0, x1, y1] = word.bbox;
        let center = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        match containing_box(boxes, center.0, center.1) {
            Some(box_index) => members[box_index].push(word_index),
            None => unassigned.push(word_index),
        }
    }
    let mut regions = Vec::new();
    let mut group_counter = 0_u32;
    for (box_index, box_) in boxes.iter().enumerate() {
        let role = role_for_label(&box_.label);
        let member_words = &members[box_index];
        let role = match role {
            LayoutRole::Number
                if box_.confidence.is_some_and(|confidence| {
                    confidence >= NUMBER_DROP_CONFIDENCE
                }) =>
            {
                LayoutRole::Drop
            }
            LayoutRole::Number => LayoutRole::Paragraph,
            role => role,
        };
        match role {
            LayoutRole::Drop => {
                stats.dropped_furniture += member_words.len();
            }
            LayoutRole::Figure => {
                stats.figures += 1;
                stats.dropped_image_text += member_words.len();
                regions.push(placeholder_region(
                    format!("p{page_index}-g{group_counter}"),
                    box_,
                    language,
                ));
                group_counter += 1;
            }
            _ => {
                if member_words.is_empty() {
                    continue;
                }
                // Keep PDF content order within the box for the same reason.
                let ordered = member_words.clone();
                let bboxes = ordered
                    .iter()
                    .map(|&index| words[index].bbox)
                    .collect::<Vec<_>>();
                let Some(union) = union_bbox(&bboxes) else {
                    continue;
                };
                let polygon = rect_polygon(union[0], union[1], union[2], union[3]);
                let text = ordered
                    .iter()
                    .map(|&index| words[index].text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                let text_words = ordered
                    .iter()
                    .map(|&index| lege_docir::TextWord {
                        text: TextEvidence::raw(words[index].text.clone()),
                        polygon: rect_polygon(
                            words[index].bbox[0],
                            words[index].bbox[1],
                            words[index].bbox[2],
                            words[index].bbox[3],
                        ),
                        confidence: RecognitionConfidence::default(),
                        geometry_source: GeometrySource::NativePdf,
                    })
                    .collect();
                let provenance =
                    layout_provenance_with("native-pdf", language, preprocessing_for(&box_.label));
                regions.push(Region {
                    id: format!("p{page_index}-g{group_counter}"),
                    kind: region_kind(role),
                    polygon: polygon.clone(),
                    confidence: RegionConfidence {
                        detection: None,
                        layout: box_.confidence,
                        recognition: None,
                    },
                    content: RegionContent::Text(TextBlock {
                        lines: vec![TextLine {
                            text: TextEvidence::raw(text),
                            polygon,
                            confidence: RecognitionConfidence::default(),
                            words: text_words,
                            provenance: provenance.clone(),
                        }],
                    }),
                    provenance,
                });
                group_counter += 1;
                stats.grouped_regions += 1;
                if matches!(role, LayoutRole::Header | LayoutRole::Footer) {
                    stats.furniture_candidates += 1;
                } else if box_.label == "number" {
                    stats.number_candidates += 1;
                }
            }
        }
    }
    if !unassigned.is_empty() {
        let ordered = unassigned.clone();
        let text = ordered
            .iter()
            .map(|&index| words[index].text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if !text.trim().is_empty() {
            let bboxes = ordered
                .iter()
                .map(|&index| words[index].bbox)
                .collect::<Vec<_>>();
            let polygon = union_bbox(&bboxes).map_or_else(
                || rect_polygon(0.0, 0.0, page_width as f32, 1.0),
                |union| rect_polygon(union[0], union[1], union[2], union[3]),
            );
            let provenance = Provenance {
                provider: "native-pdf".to_string(),
                model: None,
                preprocessing: None,
                language: Some(language.to_string()),
            };
            let text_words = ordered
                .iter()
                .map(|&index| lege_docir::TextWord {
                    text: TextEvidence::raw(words[index].text.clone()),
                    polygon: rect_polygon(
                        words[index].bbox[0],
                        words[index].bbox[1],
                        words[index].bbox[2],
                        words[index].bbox[3],
                    ),
                    confidence: RecognitionConfidence::default(),
                    geometry_source: GeometrySource::NativePdf,
                })
                .collect();
            regions.push(Region {
                id: format!("p{page_index}-r0"),
                kind: RegionKind::Paragraph,
                polygon: polygon.clone(),
                confidence: RegionConfidence::default(),
                content: RegionContent::Text(TextBlock {
                    lines: vec![TextLine {
                        text: TextEvidence::raw(text),
                        polygon,
                        confidence: RecognitionConfidence::default(),
                        words: text_words,
                        provenance: provenance.clone(),
                    }],
                }),
                provenance,
            });
            stats.unassigned += unassigned.len();
        }
    }
    let reading_order = order_regions(&regions, page_width);
    (regions, reading_order, stats)
}

/// True when a region was classified by layout detection (rather than the
/// deterministic geometry heuristic or a raw OCR line).
pub fn is_layout_classified(region: &Region) -> bool {
    matches!(
        region.provenance.preprocessing.as_deref(),
        Some(LAYOUT_GROUP_PREPROCESSING | LAYOUT_NUMBER_PREPROCESSING)
    )
}

/// True when a region is an uncertain page-number candidate awaiting the
/// document-level position check.
fn is_number_candidate(region: &Region) -> bool {
    region.provenance.preprocessing.as_deref() == Some(LAYOUT_NUMBER_PREPROCESSING)
}

/// Placeholder caption of a layout illustration figure, if this region is one.
fn layout_placeholder_caption(region: &Region) -> Option<&str> {
    if region.kind != RegionKind::Figure
        || region.provenance.provider != "pp-doclayout-m"
        || region.provenance.preprocessing.is_some()
    {
        return None;
    }
    let RegionContent::Figure(figure) = &region.content else {
        return None;
    };
    let caption = figure
        .caption
        .as_ref()?
        .plain_text(lege_docir::TextView::Raw);
    if !matches!(caption.as_str(), "[image]" | "[chart]" | "[seal]") {
        return None;
    }
    figure
        .caption
        .as_ref()?
        .lines
        .first()
        .map(|line| line.text.raw.as_str())
}

/// Document-level furniture resolution.
///
/// Per-page grouping keeps possible running heads/footers as Header/Footer
/// candidates, because low-confidence singletons are usually title-page
/// titles the model mislabels. This pass removes the candidates that repeat
/// across pages (real running furniture) and demotes unique survivors to
/// paragraphs so their text stays in reading exports. Uncertain page numbers
/// are removed when the same margin spot repeats across pages (digits move,
/// positions do not). Placeholder figures repeating on enough pages (logos,
/// watermarks) are removed everywhere.
///
/// Returns a manifest-level warning, or `None` when nothing was resolved.
pub fn resolve_furniture(document: &mut lege_docir::Document) -> Option<String> {
    use std::collections::{HashMap, HashSet};
    let mut text_pages: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut figure_pages: HashMap<(String, [i32; 4]), HashSet<u32>> = HashMap::new();
    let mut number_pages: HashMap<[i32; 4], HashSet<u32>> = HashMap::new();
    for page in &document.pages {
        for region in &page.regions {
            if is_number_candidate(region) {
                number_pages
                    .entry(quantized_bbox(region))
                    .or_default()
                    .insert(page.index);
                continue;
            }
            match region.kind {
                RegionKind::Header | RegionKind::Footer
                    if is_layout_classified(region) =>
                {
                    if let Some(text) = region.content.plain_text(lege_docir::TextView::Raw) {
                        let normalized =
                            text.split_whitespace().collect::<Vec<_>>().join(" ");
                        if !normalized.is_empty() {
                            text_pages
                                .entry(normalized)
                                .or_default()
                                .insert(page.index);
                        }
                    }
                }
                _ => {
                    if let Some(caption) = layout_placeholder_caption(region) {
                        let key = (caption.to_string(), quantized_bbox(region));
                        figure_pages.entry(key).or_default().insert(page.index);
                    }
                }
            }
        }
    }
    if text_pages.is_empty() && figure_pages.is_empty() && number_pages.is_empty() {
        return None;
    }
    let mut removed_furniture = 0_usize;
    let mut removed_figures = 0_usize;
    let mut removed_numbers = 0_usize;
    let mut kept_candidates = 0_usize;
    for page in &mut document.pages {
        let mut kept = Vec::with_capacity(page.regions.len());
        for mut region in std::mem::take(&mut page.regions) {
            if is_number_candidate(&region) {
                let repeats = number_pages
                    .get(&quantized_bbox(&region))
                    .is_some_and(|pages| pages.len() >= FURNITURE_REPEAT_PAGES);
                if repeats {
                    removed_numbers += 1;
                } else {
                    kept.push(region);
                }
                continue;
            }
            match region.kind {
                RegionKind::Header | RegionKind::Footer
                    if is_layout_classified(&region) =>
                {
                    let normalized = region
                        .content
                        .plain_text(lege_docir::TextView::Raw)
                        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                        .unwrap_or_default();
                    let repeats = text_pages
                        .get(&normalized)
                        .is_some_and(|pages| pages.len() >= FURNITURE_REPEAT_PAGES);
                    if repeats {
                        removed_furniture += 1;
                    } else {
                        region.kind = RegionKind::Paragraph;
                        kept_candidates += 1;
                        kept.push(region);
                    }
                }
                _ => {
                    let repeated_figure = layout_placeholder_caption(&region).is_some_and(
                        |caption| {
                            figure_pages
                                .get(&(caption.to_string(), quantized_bbox(&region)))
                                .is_some_and(|pages| pages.len() >= FIGURE_REPEAT_PAGES)
                        },
                    );
                    if repeated_figure {
                        removed_figures += 1;
                    } else {
                        kept.push(region);
                    }
                }
            }
        }
        page.regions = kept;
        page.reading_order
            .retain(|id| page.regions.iter().any(|region| &region.id == id));
    }
    (removed_furniture > 0 || removed_figures > 0 || removed_numbers > 0 || kept_candidates > 0)
        .then(|| {
            format!(
                "layout resolved running furniture: removed {removed_furniture} repeated header/footer region(s), {removed_numbers} repeated page-number region(s) and {removed_figures} repeated placeholder figure(s); kept {kept_candidates} unique candidate(s) as body text"
            )
        })
}

/// Coarse bbox key so margin furniture at the same spot compares equal
/// across pages despite detector jitter. A 64px quantum absorbs a few pixels
/// of box wobble without merging distinct margin items (headers vs footers
/// are hundreds of pixels apart).
const QUANTUM: f32 = 64.0;

/// Coarse bbox key so logo placeholders at the same spot compare equal
/// across pages despite pixel-level detector jitter.
fn quantized_bbox(region: &Region) -> [i32; 4] {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for point in &region.polygon {
        x0 = x0.min((point.x / QUANTUM) as i32);
        y0 = y0.min((point.y / QUANTUM) as i32);
        x1 = x1.max((point.x / QUANTUM) as i32);
        y1 = y1.max((point.y / QUANTUM) as i32);
    }
    if x1 < x0 || y1 < y0 {
        return [0, 0, 0, 0];
    }
    [x0, y0, x1, y1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_box(label: &str, bbox: [f32; 4]) -> LayoutBox {
        LayoutBox {
            label: label.to_string(),
            confidence: Some(0.9),
            bbox,
        }
    }

    fn ocr_line(text: &str, bbox: [u32; 4]) -> lege_ocr::types::OcrLineResult {
        lege_ocr::types::OcrLineResult {
            text: text.to_string(),
            confidence: Some(0.95),
            words: Vec::new(),
            bbox_highres: bbox,
        }
    }

    fn native_word(text: &str, bbox: [f32; 4]) -> lege_pdf_read::NativeTextWord {
        lege_pdf_read::NativeTextWord {
            text: text.to_string(),
            bbox,
        }
    }

    #[test]
    fn every_doclayout_label_has_a_documented_role() {
        let labels = [
            "paragraph_title",
            "image",
            "text",
            "number",
            "abstract",
            "content",
            "figure_title",
            "formula",
            "table",
            "table_title",
            "reference",
            "doc_title",
            "footnote",
            "header",
            "algorithm",
            "footer",
            "seal",
            "chart_title",
            "chart",
            "formula_number",
            "header_image",
            "footer_image",
            "aside_text",
        ];
        assert_eq!(labels.len(), 23);
        for label in labels {
            let role = role_for_label(label);
            assert_ne!(
                role_for_label("definitely-not-a-layout-class"),
                LayoutRole::Drop,
                "unknown labels must stay readable"
            );
            let _ = (label, role);
        }
        assert_eq!(role_for_label("header"), LayoutRole::Header);
        assert_eq!(role_for_label("footer"), LayoutRole::Footer);
        assert_eq!(role_for_label("number"), LayoutRole::Number);
        assert_eq!(role_for_label("doc_title"), LayoutRole::Title);
        assert_eq!(role_for_label("paragraph_title"), LayoutRole::Heading);
        assert_eq!(role_for_label("image"), LayoutRole::Figure);
        assert_eq!(placeholder_for_label("chart"), "[chart]");
        assert_eq!(placeholder_for_label("image"), "[image]");
    }

    #[test]
    fn ocr_lines_group_into_boxes_and_flag_furniture() {
        let lines = vec![
            ocr_line("Chapter One", [100, 60, 300, 100]),
            ocr_line("Body text here", [100, 140, 500, 170]),
            ocr_line("Page 12", [480, 760, 560, 785]),
        ];
        let boxes = vec![
            layout_box("paragraph_title", [90.0, 50.0, 320.0, 110.0]),
            layout_box("text", [90.0, 130.0, 520.0, 180.0]),
            LayoutBox {
                label: "number".to_string(),
                confidence: Some(0.78),
                bbox: [470.0, 750.0, 570.0, 790.0],
            },
            layout_box("footer", [90.0, 700.0, 300.0, 740.0]),
        ];
        let (regions, order, stats) = group_ocr_lines(0, 600, lines, &boxes, "paddle", "eng");
        // Confident page number drops on sight; the footer box holds no lines.
        assert_eq!(stats.dropped_furniture, 1);
        assert_eq!(stats.grouped_regions, 2);
        let title = regions
            .iter()
            .find(|region| region.kind == RegionKind::Heading)
            .expect("chapter title becomes a heading");
        assert!(
            title
                .content
                .plain_text(lege_docir::TextView::Raw)
                .is_some_and(|text| text.contains("Chapter One"))
        );
        assert!(is_layout_classified(title));
        assert_eq!(order.len(), 2);
        assert_eq!(order[0], title.id);
    }

    #[test]
    fn uncertain_numbers_and_headers_stay_readable() {
        let lines = vec![
            ocr_line("The Title", [100, 60, 300, 100]),
            ocr_line("vii", [480, 760, 520, 785]),
        ];
        let boxes = vec![
            LayoutBox {
                label: "header".to_string(),
                confidence: Some(0.35),
                bbox: [90.0, 50.0, 320.0, 110.0],
            },
            LayoutBox {
                label: "number".to_string(),
                confidence: Some(0.42),
                bbox: [470.0, 750.0, 530.0, 790.0],
            },
        ];
        let (regions, _, stats) = group_ocr_lines(0, 600, lines, &boxes, "paddle", "eng");
        assert_eq!(stats.dropped_furniture, 0);
        assert!(
            regions
                .iter()
                .any(|region| region.kind == RegionKind::Header),
            "low-confidence header stays a candidate, it is not dropped"
        );
        assert_eq!(regions.len(), 2);
    }

    #[test]
    fn images_become_placeholders_and_unassigned_lines_survive() {
        let lines = vec![
            ocr_line("Caption above", [100, 60, 300, 90]),
            ocr_line("stray line", [100, 700, 260, 725]),
        ];
        let boxes = vec![layout_box("image", [90.0, 200.0, 520.0, 600.0])];
        let (regions, _, stats) = group_ocr_lines(0, 600, lines, &boxes, "paddle", "eng");
        assert_eq!(stats.figures, 1);
        assert_eq!(stats.unassigned, 2);
        let figure = regions
            .iter()
            .find(|region| region.kind == RegionKind::Figure)
            .expect("image becomes a figure");
        assert_eq!(
            figure.content.plain_text(lege_docir::TextView::Raw).as_deref(),
            Some("[image]")
        );
        assert_eq!(regions.len(), 3);
    }

    #[test]
    fn native_words_split_into_title_and_body() {
        let words = vec![
            native_word("Runs", [100.0, 50.0, 180.0, 80.0]),
            native_word("Body", [100.0, 150.0, 170.0, 175.0]),
            native_word("text", [180.0, 150.0, 240.0, 175.0]),
            native_word("7", [290.0, 770.0, 300.0, 785.0]),
        ];
        let boxes = vec![
            layout_box("doc_title", [90.0, 40.0, 320.0, 90.0]),
            layout_box("text", [90.0, 140.0, 520.0, 185.0]),
            layout_box("number", [90.0, 760.0, 520.0, 790.0]),
        ];
        let (regions, _, stats) = group_native_words(0, 600, &words, &boxes, "eng");
        assert_eq!(stats.dropped_furniture, 1);
        let title = regions
            .iter()
            .find(|region| region.kind == RegionKind::Title)
            .expect("doc title region");
        assert_eq!(
            title.content.plain_text(lege_docir::TextView::Raw).as_deref(),
            Some("Runs")
        );
        let body = regions
            .iter()
            .find(|region| region.kind == RegionKind::Paragraph)
            .expect("body region");
        assert_eq!(
            body.content.plain_text(lege_docir::TextView::Raw).as_deref(),
            Some("Body text")
        );
    }

    fn document_with_pages(pages: Vec<lege_docir::Page>) -> lege_docir::Document {
        let mut document = lege_docir::Document::new(
            "test",
            lege_docir::SourceIdentity {
                path: "test.pdf".to_string(),
                content_hash: "blake3:test".to_string(),
                byte_len: 1,
                mime_type: "application/pdf".to_string(),
            },
            lege_docir::ProcessingManifest {
                pipeline_version: "test".to_string(),
                profile: lege_docir::ProcessingProfile::Search,
                quality: lege_docir::QualityMode::Thorough,
                configuration_hash: "test".to_string(),
                models: Vec::new(),
                warnings: Vec::new(),
            },
        );
        document.pages = pages;
        document
    }

    fn grouped_page(
        index: u32,
        lines: Vec<lege_ocr::types::OcrLineResult>,
        boxes: &[LayoutBox],
    ) -> lege_docir::Page {
        let (regions, reading_order, _) = group_ocr_lines(index, 600, lines, boxes, "test", "eng");
        lege_docir::Page {
            index,
            source_size: lege_docir::Size {
                width: 600,
                height: 800,
            },
            page_size_points: lege_docir::SizeF {
                width: 600.0,
                height: 800.0,
            },
            source_to_page: lege_docir::Transform::IDENTITY,
            source_kind: lege_docir::PageSourceKind::Rendered,
            image: None,
            regions,
            reading_order,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn repeated_headers_drop_while_unique_ones_survive_as_text() {
        let head_box = || layout_box("header", [90.0, 40.0, 520.0, 90.0]);
        let body_box = || layout_box("text", [90.0, 120.0, 520.0, 700.0]);
        let page0 = grouped_page(
            0,
            vec![
                ocr_line("Running Head", [100, 50, 300, 80]),
                ocr_line("Unique Title", [100, 130, 300, 170]),
            ],
            &[head_box(), layout_box("text", [90.0, 120.0, 520.0, 200.0])],
        );
        let page1 = grouped_page(
            1,
            vec![
                ocr_line("Running Head", [100, 50, 300, 80]),
                ocr_line("Body words", [100, 130, 300, 170]),
            ],
            &[head_box(), body_box()],
        );
        let mut document = document_with_pages(vec![page0, page1]);
        let warning = resolve_furniture(&mut document).expect("resolves something");
        assert!(warning.contains("removed 2 repeated"), "{warning}");
        for page in &document.pages {
            assert!(
                !page.regions.iter().any(|region| region
                    .content
                    .plain_text(lege_docir::TextView::Raw)
                    .is_some_and(|text| text.contains("Running Head"))),
                "repeated running head is gone from page {}",
                page.index
            );
            assert_eq!(
                page.reading_order.len(),
                page.regions.len(),
                "reading order tracks surviving regions"
            );
        }
        let title = document.pages[0]
            .regions
            .iter()
            .find(|region| {
                region.content.plain_text(lege_docir::TextView::Raw)
                    .is_some_and(|text| text.contains("Unique Title"))
            })
            .expect("unique title survives");
        assert_eq!(title.kind, lege_docir::RegionKind::Paragraph);
        assert!(
            document.pages[0].reading_order.contains(&title.id),
            "survivor stays in reading order"
        );
    }

    #[test]
    fn page_numbers_repeating_in_the_margin_are_removed() {
        let number_at = |confidence: f32| LayoutBox {
            label: "number".to_string(),
            confidence: Some(confidence),
            bbox: [1758.0, 2749.0, 1778.0, 2780.0],
        };
        let pages = (0..2)
            .map(|index| {
                grouped_page(
                    index,
                    vec![
                        ocr_line("Body", [100, 300, 300, 330]),
                        ocr_line("7", [1760, 2755, 1775, 2775]),
                    ],
                    &[number_at(0.42), layout_box("text", [90.0, 280.0, 520.0, 360.0])],
                )
            })
            .collect();
        let mut document = document_with_pages(pages);
        let warning = resolve_furniture(&mut document).expect("resolves something");
        assert!(warning.contains("2 repeated page-number"), "{warning}");
        for page in &document.pages {
            assert!(
                !page.regions.iter().any(|region| {
                    region.content.plain_text(lege_docir::TextView::Raw).as_deref() == Some("7")
                }),
                "repeated margin number is gone from page {}",
                page.index
            );
        }
    }

    #[test]
    fn logos_repeating_on_three_pages_are_removed() {        let logo = || layout_box("image", [90.0, 40.0, 200.0, 120.0]);
        let pages = (0..3)
            .map(|index| {
                grouped_page(
                    index,
                    vec![ocr_line("Body", [100, 300, 300, 330])],
                    &[logo(), layout_box("text", [90.0, 280.0, 520.0, 360.0])],
                )
            })
            .collect();
        let mut document = document_with_pages(pages);
        resolve_furniture(&mut document).expect("resolves something");
        for page in &document.pages {
            assert!(
                !page
                    .regions
                    .iter()
                    .any(|region| region.kind == lege_docir::RegionKind::Figure),
                "repeated logo placeholder is gone from page {}",
                page.index
            );
        }
    }
}
