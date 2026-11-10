//! Page 1 through the lege-document-ocr pipeline (Thorough quality, layout
//! detection on), flattened to the lines the heuristics read.

use std::sync::Arc;

use lege_docir::{Page, Point, RegionContent, RegionKind, TextView};
use lege_document_pipeline::correction::CorrectionMode;
use lege_document_pipeline::{BackendChoice, DocumentProcessor, PipelineConfig};

use crate::heuristics::{Line, LineKind, PageLines};

#[derive(Debug, Clone)]
pub struct OcrSettings {
    pub backend: BackendChoice,
    pub render_dpi: u32,
    pub layout: bool,
    /// Use a trustworthy embedded text layer instead of OCR.
    pub use_text_layer: bool,
    pub tensorrt_paddle: Option<lege_document_pipeline::TensorRtPaddleConfig>,
}

pub struct PageReader {
    processor: DocumentProcessor,
}

impl std::fmt::Debug for PageReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageReader")
            .field("backend", &self.processor.resolved_backend_name())
            .finish_non_exhaustive()
    }
}

impl PageReader {
    pub fn new(settings: &OcrSettings) -> Result<Self, String> {
        let config = PipelineConfig {
            quality: lege_docir::QualityMode::Thorough,
            backend: settings.backend,
            render_dpi: settings.render_dpi,
            force_ocr: !settings.use_text_layer,
            // Identity text should read as printed; spelling repair belongs to
            // running text, and with no dictionary it would only add warnings.
            correction_mode: CorrectionMode::Disabled,
            layout_enabled: settings.layout,
            // A cover is often one big "image" box; its text is what we want.
            keep_figure_text: true,
            tensorrt_paddle: settings.tensorrt_paddle.clone(),
            ..PipelineConfig::default()
        };
        let processor = DocumentProcessor::new(config).map_err(|error| error.to_string())?;
        Ok(Self { processor })
    }

    /// Start the OCR backend now so a missing runtime fails before the batch.
    pub fn start(&self) -> Result<&'static str, String> {
        self.processor
            .initialize_backend()
            .map_err(|error| error.to_string())
    }

    pub fn warnings(&self) -> Vec<String> {
        self.processor
            .layout_warning()
            .into_iter()
            .chain(self.processor.backend_selection_warning())
            .map(str::to_string)
            .collect()
    }

    pub fn first_page(&self, bytes: &Arc<[u8]>) -> Result<PageLines, String> {
        let page = self
            .processor
            .process_single_page(Arc::clone(bytes), 0)
            .map_err(|error| error.to_string())?;
        Ok(page_lines(&page))
    }
}

pub fn page_lines(page: &Page) -> PageLines {
    let mut lines = Vec::new();
    for (region_index, region) in page.regions.iter().enumerate() {
        let RegionContent::Text(block) = &region.content else {
            continue;
        };
        let kind = match region.kind {
            RegionKind::Title => LineKind::Title,
            RegionKind::Heading => LineKind::Heading,
            RegionKind::Header => LineKind::Header,
            RegionKind::Footer => LineKind::Footer,
            _ => LineKind::Other,
        };
        for line in &block.lines {
            let Some(bbox) = polygon_bounds(&line.polygon) else {
                continue;
            };
            lines.push(Line {
                text: line.text.select(TextView::Raw),
                bbox,
                confidence: line.confidence.mean_token,
                kind,
                layout_confidence: region.confidence.layout,
                region: Some(region_index),
            });
        }
    }
    PageLines {
        width: page.source_size.width as f32,
        height: page.source_size.height as f32,
        lines,
    }
}

fn polygon_bounds(polygon: &[Point]) -> Option<[f32; 4]> {
    let first = polygon.first()?;
    let mut bounds = [first.x, first.y, first.x, first.y];
    for point in polygon {
        bounds[0] = bounds[0].min(point.x);
        bounds[1] = bounds[1].min(point.y);
        bounds[2] = bounds[2].max(point.x);
        bounds[3] = bounds[3].max(point.y);
    }
    Some(bounds)
}
