use crate::pagerender::prelude::{PdfRenderer, RasterConfig as PdfRasterConfig};
use crate::pipeline::config::{PipelineConfig, RenderedPageData};
use crate::pipeline::policies::build_inference_image;
use crate::progress::ProgressTracker;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use exprim::RenderSession;
use futures_util::stream::{FuturesUnordered, StreamExt};
use image::RgbImage;
use std::cmp::Ordering as CmpOrdering;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::mpsc;

const SUPPORTED_IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "ppm", "pbm", "pgm", "pnm", "tiff", "tif", "bmp", "jp2",
];

pub struct SourcePage {
    pub image: RgbImage,
    pub original_width_pts: f32,
    pub original_height_pts: f32,
}

#[async_trait]
pub trait PageSource: Send + Sync {
    fn page_count(&self) -> usize;
    fn source_concurrency(&self) -> usize;
    fn document_session(&self) -> Option<Arc<RenderSession>> {
        None
    }
    /// Pixel dimensions the source decodes or renders before any downscale:
    /// the peak input allocation a page job must admit. PDF pages render
    /// directly at the target size; image files decode at native size and are
    /// resized afterwards, so an estimate from the target size alone does not
    /// cover the decode. `None` means unknown without doing the work (for
    /// example a JPEG 2000 stream the header sniffer cannot parse); callers
    /// fall back to the configured-size estimate plus a post-load top-up.
    fn native_page_size(&self, page_index: usize) -> Option<(u32, u32)> {
        let _ = page_index;
        None
    }
    /// True when the source decodes at native size and resizes afterwards
    /// (image folders, ZIPs), so admission must cover the native decode on
    /// top of the target working set. False when the source renders directly
    /// at the target size (PDF), where the native size instead refines the
    /// working-set estimate.
    fn decodes_at_native_size(&self) -> bool {
        false
    }
    async fn load_page(&self, page_index: usize) -> Result<SourcePage>;

    async fn load_page_cancellable(
        &self,
        page_index: usize,
        _cancellation: exprim::CancellationToken,
    ) -> Result<SourcePage> {
        self.load_page(page_index).await
    }

    /// Load a page already reduced to `analysis_width` for the low-resolution
    /// margin pass. The default loads the full page and lets the caller
    /// downscale it; PDF sources override this to render directly at analysis
    /// resolution instead of allocating the full raster first.
    async fn load_analysis_page(
        &self,
        page_index: usize,
        analysis_width: u32,
        cancellation: exprim::CancellationToken,
    ) -> Result<SourcePage> {
        let _ = analysis_width;
        self.load_page_cancellable(page_index, cancellation).await
    }
}

pub struct PdfPageSource {
    renderer: Arc<PdfRenderer>,
    config: Arc<PipelineConfig>,
}

impl PdfPageSource {
    pub fn new(pdf_bytes: Arc<[u8]>, config: Arc<PipelineConfig>) -> Result<Self> {
        let document = Arc::new(
            RenderSession::open(pdf_bytes, None)
                .map_err(|error| anyhow!("Failed to read PDF document: {error}"))?,
        );
        Ok(Self::from_session(document, config))
    }

    /// A page source over a session the caller already opened (the pipeline's
    /// page-count session), so the document is not parsed a second time.
    pub fn from_session(document: Arc<RenderSession>, config: Arc<PipelineConfig>) -> Self {
        let mut raster_cfg = PdfRasterConfig::default();
        raster_cfg.render_forms = false;
        raster_cfg.target_width = config.target_width();
        let renderer = Arc::new(PdfRenderer::from_session(document, raster_cfg));
        Self { renderer, config }
    }
}

#[async_trait]
impl PageSource for PdfPageSource {
    fn page_count(&self) -> usize {
        self.renderer.page_count() as usize
    }

    fn source_concurrency(&self) -> usize {
        std::thread::available_parallelism()
            .map(|threads| threads.get().saturating_sub(1).max(1))
            .unwrap_or(1)
    }

    fn document_session(&self) -> Option<Arc<RenderSession>> {
        Some(self.renderer.document_session())
    }

    fn native_page_size(&self, page_index: usize) -> Option<(u32, u32)> {
        let geometry = self
            .renderer
            .document_session()
            .page_geometry(page_index as u32)
            .ok()?;
        // Same dimension resolution as the render call below: an explicit
        // width renders exactly, otherwise width follows the page aspect.
        let target_width = self
            .config
            .source_render_width()
            .or(self.config.target_width());
        crate::pagerender::render_dimensions(
            geometry.display_width(),
            geometry.display_height(),
            self.config.source_render_height(),
            target_width,
        )
        .ok()
    }

    async fn load_page(&self, page_index: usize) -> Result<SourcePage> {
        self.load_pdf_page(page_index, None).await
    }

    async fn load_page_cancellable(
        &self,
        page_index: usize,
        cancellation: exprim::CancellationToken,
    ) -> Result<SourcePage> {
        self.load_pdf_page(page_index, Some(cancellation)).await
    }

    async fn load_analysis_page(
        &self,
        page_index: usize,
        analysis_width: u32,
        cancellation: exprim::CancellationToken,
    ) -> Result<SourcePage> {
        // Render directly at analysis resolution: the margin pass only ever
        // sees the 640-wide image, so rendering full-size first would allocate
        // the large raster purely to downscale it. Height mirrors the
        // analysis downscale's aspect formula; downstream stages scale by the
        // actual dimensions, so a one-pixel rounding difference is harmless.
        let aspect = self
            .renderer
            .document_session()
            .page_geometry(page_index as u32)
            .ok()
            .map(|geometry| geometry.display_width() / geometry.display_height().max(f64::EPSILON))
            .filter(|aspect| aspect.is_finite() && *aspect > 0.0);
        let Some(aspect) = aspect else {
            return self.load_page_cancellable(page_index, cancellation).await;
        };
        let analysis_height = (f64::from(analysis_width.max(1)) / aspect)
            .round()
            .clamp(1.0, f64::from(u32::MAX)) as u32;
        let rgb_page = self
            .renderer
            .render_page_rgb_cancellable(
                page_index as u32,
                analysis_height,
                Some(analysis_width.max(1)),
                Some(cancellation),
            )
            .await
            .map_err(|error| anyhow!("Failed to render page {page_index}: {error}"))?;
        source_page_from_rgb(page_index, rgb_page)
    }
}

fn source_page_from_rgb(
    page_index: usize,
    rgb_page: crate::pagerender::RgbPage,
) -> Result<SourcePage> {
    let image =
        RgbImage::from_raw(rgb_page.width, rgb_page.height, rgb_page.data).ok_or_else(|| {
            anyhow!(
                "Failed to create ImageBuffer for page {} ({}x{})",
                page_index,
                rgb_page.width,
                rgb_page.height
            )
        })?;

    Ok(SourcePage {
        image,
        // The renderer already read and returned this geometry while
        // calculating raster dimensions. Avoid a second page-tree lookup
        // for every source page.
        original_width_pts: rgb_page.original_width_pts,
        original_height_pts: rgb_page.original_height_pts,
    })
}

impl PdfPageSource {
    async fn load_pdf_page(
        &self,
        page_index: usize,
        cancellation: Option<exprim::CancellationToken>,
    ) -> Result<SourcePage> {
        // "Render high, resize low": render at the highest resolution any stage
        // needs (OCR when slow-OCR is enabled), then downstream stages resize
        // down to target_height for encoding. Equals target_height otherwise.
        let target_height = self.config.source_render_height();
        let target_width = self.config.source_render_width();
        let rgb_page = self
            .renderer
            .render_page_rgb_cancellable(
                page_index as u32,
                target_height,
                target_width,
                cancellation,
            )
            .await
            .map_err(|error| anyhow!("Failed to render page {page_index}: {error}"))?;
        source_page_from_rgb(page_index, rgb_page)
    }
}

pub struct ImageFolderPageSource {
    files: Arc<Vec<PathBuf>>,
    concurrency: usize,
    target_height: u32,
    target_width: Option<u32>,
}

#[derive(Clone, Debug)]
struct ZipImageEntry {
    archive_index: usize,
    name: String,
    uncompressed_size: u64,
}

/// Streams image entries directly from a ZIP into the image pipeline.
///
/// Each in-flight source task opens its own archive reader, extracts one entry
/// into memory, and decodes it on that same blocking worker. This lets JP2
/// decompression overlap with later pipeline stages without first materializing
/// an Archive.org `_jp2.zip` tree on disk.
pub struct ZipImagePageSource {
    zip_path: PathBuf,
    entries: Arc<Vec<ZipImageEntry>>,
    concurrency: usize,
    target_height: u32,
    target_width: Option<u32>,
}

impl ZipImagePageSource {
    pub fn open(
        zip_path: PathBuf,
        concurrency: usize,
        target_height: u32,
        target_width: Option<u32>,
    ) -> Result<Self> {
        let file = std::fs::File::open(&zip_path)
            .with_context(|| format!("Failed to open ZIP: {}", zip_path.display()))?;
        let mut archive = zip::ZipArchive::new(file)
            .with_context(|| format!("Failed to read ZIP: {}", zip_path.display()))?;
        let mut entries = Vec::new();
        for archive_index in 0..archive.len() {
            let entry = archive.by_index_raw(archive_index).with_context(|| {
                format!(
                    "Failed to inspect ZIP entry {} in {}",
                    archive_index,
                    zip_path.display()
                )
            })?;
            if !entry.is_dir() && is_supported_image(Path::new(entry.name())) {
                entries.push(ZipImageEntry {
                    archive_index,
                    name: entry.name().to_string(),
                    uncompressed_size: entry.size(),
                });
            }
        }
        let entries = select_primary_zip_image_set(entries);
        if entries.is_empty() {
            return Err(anyhow!(
                "No supported image files found in ZIP {}",
                zip_path.display()
            ));
        }
        Ok(Self {
            zip_path,
            entries: Arc::new(entries),
            concurrency: concurrency.max(1),
            target_height,
            target_width,
        })
    }
}

impl ImageFolderPageSource {
    pub fn new(
        files: Vec<PathBuf>,
        concurrency: usize,
        target_height: u32,
        target_width: Option<u32>,
    ) -> Self {
        Self {
            files: Arc::new(files),
            concurrency: concurrency.max(1),
            target_height,
            target_width,
        }
    }
}

#[async_trait]
impl PageSource for ImageFolderPageSource {
    fn page_count(&self) -> usize {
        self.files.len()
    }

    fn source_concurrency(&self) -> usize {
        self.concurrency
    }

    fn native_page_size(&self, page_index: usize) -> Option<(u32, u32)> {
        // Header-only read: no pixel allocation. JPEG 2000 files, which the
        // `image` crate cannot parse, go through the JP2 header sniffer.
        let path = self.files.get(page_index)?;
        file_image_dimensions(path)
    }

    fn decodes_at_native_size(&self) -> bool {
        true
    }

    async fn load_page(&self, page_index: usize) -> Result<SourcePage> {
        let path = self
            .files
            .get(page_index)
            .cloned()
            .ok_or_else(|| anyhow!("Image page index {} out of bounds", page_index))?;

        let decode_path = path.clone();
        let mut image =
            crate::runtime_stats::spawn_blocking(move || decode_folder_image(&decode_path))
                .await
                .map_err(|e| anyhow!("Image decode task panicked: {}", e))??;
        let (width, height) = image.dimensions();
        maybe_dump_folder_source_image("decoded", page_index, &image)?;
        image = normalize_folder_image_to_target(
            image,
            self.target_height,
            self.target_width,
            page_index,
        )?;
        maybe_dump_folder_source_image("normalized", page_index, &image)?;

        Ok(SourcePage {
            image,
            original_width_pts: width as f32,
            original_height_pts: height as f32,
        })
    }
}

#[async_trait]
impl PageSource for ZipImagePageSource {
    fn page_count(&self) -> usize {
        self.entries.len()
    }

    fn source_concurrency(&self) -> usize {
        self.concurrency
    }

    fn native_page_size(&self, page_index: usize) -> Option<(u32, u32)> {
        let entry = self.entries.get(page_index)?;
        zip_image_dimensions(&self.zip_path, entry)
    }

    fn decodes_at_native_size(&self) -> bool {
        true
    }

    async fn load_page(&self, page_index: usize) -> Result<SourcePage> {
        let entry = self
            .entries
            .get(page_index)
            .cloned()
            .ok_or_else(|| anyhow!("ZIP image page index {} out of bounds", page_index))?;
        let zip_path = self.zip_path.clone();
        let mut image =
            crate::runtime_stats::spawn_blocking(move || decode_zip_image(&zip_path, &entry))
                .await
                .map_err(|e| anyhow!("ZIP image decode task panicked: {}", e))??;
        let (width, height) = image.dimensions();
        image = normalize_folder_image_to_target(
            image,
            self.target_height,
            self.target_width,
            page_index,
        )?;

        Ok(SourcePage {
            image,
            original_width_pts: width as f32,
            original_height_pts: height as f32,
        })
    }
}

/// Target working-set bytes per target pixel: render RGB + inference RGB +
/// adjusted RGB + gray/binary planes + codec scratch. Deliberately
/// overestimates ordinary pages so admission happens before the large
/// allocations, not after them.
const WORKING_BYTES_PER_TARGET_PIXEL: u64 = 16;

/// Decode bytes per native pixel for sources that decode at native size and
/// resize afterwards: the RGB plane plus decoder scratch.
const DECODE_BYTES_PER_NATIVE_PIXEL: u64 = 6;

/// One page job sized against the page-memory budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageMemoryCharge {
    /// Unclamped estimate, for logging.
    pub estimated_mb: usize,
    /// Megabytes to acquire (clamped to the budget, at least 1).
    pub acquire_mb: u32,
    /// Working-set pixels the charge covers, for post-load top-up.
    pub charged_work_pixels: u64,
    /// True when the estimate exceeds the whole budget: the page is admitted
    /// alone (see [`note_oversized_page`]) rather than silently clamped.
    pub oversized: bool,
}

/// Size one page job against the page-memory budget.
///
/// `target_width`/`target_height` are the working dimensions downstream
/// stages operate at; `native` is the source's pre-downscale size when known.
/// Directly-rendered sources (PDF) report exact render dims, which replace
/// the configured-size guess; decode-then-resize sources (image folders,
/// ZIPs) additionally charge the native decode peak they allocate before
/// the resize.
pub(crate) fn estimate_page_charge_mb(
    target_width: u32,
    target_height: u32,
    native: Option<(u32, u32)>,
    decodes_at_native_size: bool,
    budget_mb: usize,
) -> PageMemoryCharge {
    let (work_width, work_height) = match (decodes_at_native_size, native) {
        (false, Some((width, height))) => (width.max(1), height.max(1)),
        _ => (target_width.max(1), target_height.max(1)),
    };
    let work_pixels = u64::from(work_width) * u64::from(work_height);
    let mut bytes = work_pixels.saturating_mul(WORKING_BYTES_PER_TARGET_PIXEL);
    if decodes_at_native_size {
        if let Some((width, height)) = native {
            let native_pixels = u64::from(width) * u64::from(height);
            bytes = bytes.saturating_add(
                native_pixels
                    .saturating_sub(work_pixels)
                    .saturating_mul(DECODE_BYTES_PER_NATIVE_PIXEL),
            );
        }
    }
    let estimated_mb = bytes.div_ceil(1024 * 1024).max(1).min(usize::MAX as u64) as usize;
    let budget = budget_mb.max(1);
    PageMemoryCharge {
        estimated_mb,
        acquire_mb: estimated_mb.min(budget).min(u32::MAX as usize) as u32,
        charged_work_pixels: work_pixels,
        oversized: estimated_mb > budget,
    }
}

/// Additional megabytes to admit when the loaded page is bigger than the
/// pre-admission charge covered (unknown native size, wider aspect). Zero
/// when the charge already covers the actual dimensions.
pub(crate) fn top_up_charge_mb(
    charge: &PageMemoryCharge,
    actual_width: u32,
    actual_height: u32,
) -> u32 {
    let actual_pixels = u64::from(actual_width.max(1)) * u64::from(actual_height.max(1));
    let extra_pixels = actual_pixels.saturating_sub(charge.charged_work_pixels);
    extra_pixels
        .saturating_mul(WORKING_BYTES_PER_TARGET_PIXEL)
        .div_ceil(1024 * 1024)
        .min(u32::MAX as u64) as u32
}

/// Working dimensions downstream stages operate at for a full page job:
/// the source render height (scaled up when slow OCR is active) with the
/// configured width, or a 3:2 fallback when no width is configured.
pub(crate) fn configured_render_dims(config: &PipelineConfig) -> (u32, u32) {
    let height = config.source_render_height().max(1);
    let width = config
        .source_render_width()
        .unwrap_or_else(|| height.saturating_mul(3).div_ceil(2))
        .max(1);
    (width, height)
}

/// Page working-set admission budget in MiB. This is an admission setting,
/// not a promise that RSS stays below it: inference sessions are resident
/// before admission, the reader keeps document-scoped state, and encoded
/// payloads outlive the page permit until the writer consumes them. Size the
/// budget for the page working sets, not the whole process.
pub(crate) fn page_memory_budget_mb() -> usize {
    if let Some(override_mb) = std::env::var("LEGE_MEMORY_BUDGET_MB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&value| value > 0)
    {
        return override_mb;
    }
    crate::pipeline::helper_functions::get_available_ram_gb()
        .saturating_mul(512)
        .clamp(1024, 8192)
}

static OVERSIZED_PAGE_WARNED: AtomicBool = AtomicBool::new(false);

/// Log an oversized page: admitted alone because its estimate exceeds the
/// whole budget. The clamp that makes this possible does not make the
/// allocation fit; the first occurrence warns with the remedy and the rest
/// log at info so a uniformly-oversized document stays readable.
pub(crate) fn note_oversized_page(page_index: usize, charge: &PageMemoryCharge, budget_mb: usize) {
    if OVERSIZED_PAGE_WARNED.swap(true, Ordering::SeqCst) {
        crate::info_log!(
            "Page {} needs ~{} MiB, over the {} MiB page budget; admitting it alone",
            page_index + 1,
            charge.estimated_mb,
            budget_mb.max(1),
        );
    } else {
        crate::warn_log!(
            "Page {} needs ~{} MiB, over the {} MiB page budget; admitting it alone. \
             Raise LEGE_MEMORY_BUDGET_MB or lower the render height if this recurs.",
            page_index + 1,
            charge.estimated_mb,
            budget_mb.max(1),
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn source_stage(
    source: Arc<dyn PageSource>,
    config: Arc<PipelineConfig>,
    page_range: std::ops::Range<usize>,
    cancellation: exprim::CancellationToken,
    tx: mpsc::Sender<RenderedPageData>,
    render_count: Arc<AtomicUsize>,
    detect_count: Arc<AtomicUsize>,
    encode_count: Arc<AtomicUsize>,
    progress: ProgressTracker,
    total_pages: usize,
    layout_enabled: bool,
    concurrency_cap: usize,
) -> Result<()> {
    // The source advertises what it can parallelize (PDF: CPUs minus one);
    // the pipeline caps that at what downstream can consume, so a wide
    // source cannot stage more decoded pages than the policy allows.
    let concurrency = source
        .source_concurrency()
        .max(1)
        .min(concurrency_cap.max(1));
    crate::info_log!(
        "[SourceStage] Starting source stage for {} pages with concurrency={}",
        total_pages,
        concurrency
    );

    let mut in_flight = FuturesUnordered::new();
    let mut next_page = page_range.start;
    let page_end = page_range.end;

    loop {
        while next_page < page_end && in_flight.len() < concurrency {
            let source = source.clone();
            let config = config.clone();
            let cancellation = cancellation.clone();
            let page_index = next_page;
            next_page += 1;

            in_flight.push(tokio::spawn(crate::runtime_stats::track_future(
                crate::runtime_stats::Stage::Render,
                async move {
                    crate::info_log!("[SourceStage] Loading page {}", page_index);
                    if cancellation.is_cancelled() {
                        return Err(anyhow!("Source stage cancelled before page {page_index}"));
                    }
                    let source_page = source
                        .load_page_cancellable(page_index, cancellation.clone())
                        .await
                        .map_err(|e| {
                            #[cfg(feature = "debug-logging")]
                            crate::error_println!(
                                "[SourceStage] Page {} load failed: {:#}",
                                page_index,
                                e
                            );
                            e
                        })?;
                    if cancellation.is_cancelled() {
                        return Err(anyhow!(
                            "Source stage cancelled after page {page_index} render"
                        ));
                    }
                    let SourcePage {
                        image,
                        original_width_pts,
                        original_height_pts,
                    } = source_page;

                    crate::pipeline::set_standard_dimensions_once(image.width(), image.height());

                    let high_res_arc = Arc::new(image);
                    let page_layout_enabled = config.layout_detection_enabled_for_page(page_index);
                    // In no-layout mode the inference image is never used; share the
                    // high-res Arc instead of deep-cloning a full RGB page per file.
                    let inference_image = if page_layout_enabled {
                        let spec = config.inference_resize_spec();
                        let img = build_inference_image(high_res_arc.as_ref(), &spec)
                            .unwrap_or_else(|_| (*high_res_arc).clone());
                        Arc::new(img)
                    } else {
                        high_res_arc.clone()
                    };
                    if cancellation.is_cancelled() {
                        return Err(anyhow!(
                            "Source stage cancelled after page {page_index} preparation"
                        ));
                    }

                    Ok::<_, anyhow::Error>(RenderedPageData {
                        index: page_index,
                        high_res_image: high_res_arc,
                        inference_image,
                        layout_detection_enabled: page_layout_enabled,
                        original_width_pts,
                        original_height_pts,
                    })
                },
            )));
        }

        let Some(result) = in_flight.next().await else {
            break;
        };
        let rendered = result.map_err(|e| anyhow!("Source stage task panicked: {}", e))??;
        if tx.send(rendered).await.is_err() {
            break;
        }

        let rendered_val = render_count.fetch_add(1, Ordering::Relaxed) + 1;
        if layout_enabled {
            let detected_val = detect_count.load(Ordering::Relaxed);
            let encoded_val = encode_count.load(Ordering::Relaxed);
            progress.publish_layout_progress(rendered_val, detected_val, encoded_val, total_pages);
        } else {
            progress.publish_no_layout_render_progress(rendered_val, total_pages);
        }
    }

    crate::info_log!("[SourceStage] Source stage complete");
    Ok(())
}

pub fn collect_largest_sequential_image_run(folder: &Path) -> Result<Vec<PathBuf>> {
    let mut all_files: Vec<PathBuf> = Vec::new();
    collect_supported_images_recursive(folder, &mut all_files)?;

    if all_files.is_empty() {
        return Ok(all_files);
    }

    let total_found = all_files.len();

    let mut by_ext: std::collections::HashMap<String, Vec<PathBuf>> =
        std::collections::HashMap::new();
    for file in all_files {
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        by_ext.entry(ext).or_default().push(file);
    }

    let mut best_run = by_ext
        .into_iter()
        .max_by(|(ext_a, a), (ext_b, b)| {
            a.len()
                .cmp(&b.len())
                .then_with(|| (ext_a == "jp2").cmp(&(ext_b == "jp2")))
        })
        .map(|(_, files)| files)
        .unwrap_or_default();
    best_run.sort_by(|a, b| compare_image_names(a, b));

    if best_run.len() < total_found {
        // Prefer the dominant format so thumbnails/covers in another format do
        // not enter the page stream. Within that format, retain every page even
        // when Archive.org numbering has gaps.
        crate::error_println!(
            "Warning: {} of {} image files in {} were skipped because another image format \
formed the dominant page set. Set LEGE_FOLDER_SOURCE_DEBUG=1 to list the selected files.",
            total_found - best_run.len(),
            total_found,
            folder.display()
        );
    }

    if folder_source_debug_enabled() {
        crate::info_log!(
            "[ImageFolderPageSource] Selected {} files from {}",
            best_run.len(),
            folder.display()
        );
        for (idx, path) in best_run.iter().take(5).enumerate() {
            crate::info_log!(
                "[ImageFolderPageSource] selected[{}]={}",
                idx,
                path.display()
            );
        }
        if best_run.len() > 5 {
            for (idx, path) in best_run
                .iter()
                .enumerate()
                .skip(best_run.len().saturating_sub(5))
            {
                crate::info_log!(
                    "[ImageFolderPageSource] selected[{}]={}",
                    idx,
                    path.display()
                );
            }
        }
    }

    Ok(best_run)
}

fn folder_source_debug_enabled() -> bool {
    std::env::var("LEGE_FOLDER_SOURCE_DEBUG")
        .map(|value| value != "0")
        .unwrap_or(false)
}

fn maybe_dump_folder_source_image(stage: &str, page_index: usize, image: &RgbImage) -> Result<()> {
    if !folder_source_debug_enabled() {
        return Ok(());
    }

    let out_dir = crate::app_dirs::data_dir().join("folder_source_debug");
    std::fs::create_dir_all(&out_dir).with_context(|| {
        format!(
            "Failed to create folder source debug directory: {:?}",
            out_dir
        )
    })?;
    let out_path = out_dir.join(format!("page_{:04}_{}.png", page_index + 1, stage));
    image
        .save(&out_path)
        .with_context(|| format!("Failed to write folder source debug image: {:?}", out_path))?;
    Ok(())
}

fn normalize_folder_image_to_target(
    image: RgbImage,
    target_height: u32,
    target_width: Option<u32>,
    page_index: usize,
) -> Result<RgbImage> {
    if target_height == 0 || image.height() == target_height {
        return Ok(image);
    }

    let current_w = image.width();
    let current_h = image.height();
    let aspect_ratio = current_w as f32 / current_h as f32;
    let target_w =
        target_width.unwrap_or_else(|| (target_height as f32 * aspect_ratio).round() as u32);
    if target_w == 0 {
        return Ok(image);
    }

    let params = crate::resize::ResizeParams {
        target_width: target_w,
        target_height,
        method: crate::resize::ResizeMethod::Lanczos3,
        letterbox: false,
        border_value: 0.0,
        swap_rb: false,
    };

    match crate::resize::resize_bytes(image.as_raw(), current_w, current_h, &params, 3) {
        Ok(bytes) => RgbImage::from_raw(target_w, target_height, bytes).ok_or_else(|| {
            anyhow!(
                "Failed to create normalized image buffer for page {} ({}x{})",
                page_index,
                target_w,
                target_height
            )
        }),
        Err(e) => {
            crate::warn_log!(
                "Page {}: folder source resize failed: {}; using CPU image fallback",
                page_index,
                e
            );
            Ok(image::imageops::resize(
                &image,
                target_w,
                target_height,
                image::imageops::FilterType::Lanczos3,
            ))
        }
    }
}

fn collect_supported_images_recursive(folder: &Path, all_files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_supported_images_recursive(&path, all_files)?;
        } else if path.is_file() && is_supported_image(&path) {
            all_files.push(path);
        }
    }
    Ok(())
}

fn decode_folder_image(image_path: &Path) -> Result<RgbImage> {
    let is_jp2 = image_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("jp2"))
        .unwrap_or(false);

    if is_jp2 {
        #[cfg(feature = "jp2-lam")]
        {
            let bytes = std::fs::read(image_path)
                .with_context(|| format!("Failed to read JP2: {}", image_path.display()))?;
            let (width, height, rgb_data) = crate::encoding::jp2::decode_jp2_bytes(&bytes)
                .map_err(|e| {
                    anyhow!(
                        "JP2 cannot be decoded, job aborted: {} - {}",
                        image_path.display(),
                        e
                    )
                })?;
            return RgbImage::from_raw(width, height, rgb_data).ok_or_else(|| {
                anyhow!(
                    "JP2 cannot be decoded, job aborted: {} - invalid image dimensions or buffer",
                    image_path.display()
                )
            });
        }
        #[cfg(not(feature = "jp2-lam"))]
        return Err(anyhow!(
            "JP2 support not compiled in (enable jp2-lam feature): {}",
            image_path.display()
        ));
    }

    let dynamic = image::open(image_path)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("Failed to open image: {}", image_path.display()))?;
    Ok(dynamic.to_rgb8())
}

const MAX_ZIP_IMAGE_BYTES: u64 = 512 * 1024 * 1024;

/// How much of a ZIP entry to inflate when sniffing its pixel dimensions.
/// Image headers live at the start of the stream; anything past this that the
/// sniffer needs reports `None` and falls back to the configured estimate.
const IMAGE_HEADER_PREFIX_BYTES: u64 = 64 * 1024;

/// Read one ZIP entry's pixel dimensions without decoding it: inflate a small
/// prefix and parse the container header. JPEG 2000 entries (which the
/// `image` crate cannot parse) go through [`jp2_header_dimensions`]. Returns
/// `None` for anything the header parser cannot identify from the prefix.
fn zip_image_dimensions(zip_path: &Path, selected: &ZipImageEntry) -> Option<(u32, u32)> {
    let file = std::fs::File::open(zip_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let entry = archive.by_index(selected.archive_index).ok()?;
    let mut prefix = Vec::new();
    entry
        .take(IMAGE_HEADER_PREFIX_BYTES)
        .read_to_end(&mut prefix)
        .ok()?;
    if let Some(dims) = jp2_header_dimensions(&prefix) {
        return Some(dims);
    }
    image::ImageReader::new(std::io::Cursor::new(prefix))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Pixel dimensions of a JPEG 2000 file (JP2 wrapper or bare J2K codestream)
/// from a header prefix, without decoding. A JP2 file reads the `ihdr` box
/// inside `jp2h`; a bare codestream (or a `jp2c` box reached before any
/// `ihdr`) reads the SIZ marker as `Xsiz - XOsiz` by `Ysiz - YOsiz`. Returns
/// `None` when the prefix is not JPEG 2000 or ends before the header.
fn jp2_header_dimensions(prefix: &[u8]) -> Option<(u32, u32)> {
    const JP2_SIGNATURE: [u8; 12] = [
        0x00, 0x00, 0x00, 0x0C, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A,
    ];
    fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
        let slice = bytes.get(at..at.checked_add(4)?)?;
        Some(u32::from_be_bytes(slice.try_into().ok()?))
    }
    fn siz_dimensions(codestream: &[u8]) -> Option<(u32, u32)> {
        // SOC, SIZ, Lsiz(2), Rsiz(2), Xsiz, Ysiz, XOsiz, YOsiz.
        if codestream.get(0..4)? != [0xFF, 0x4F, 0xFF, 0x51] {
            return None;
        }
        let x = be_u32(codestream, 8)?;
        let y = be_u32(codestream, 12)?;
        let x_offset = be_u32(codestream, 16)?;
        let y_offset = be_u32(codestream, 20)?;
        let width = x.checked_sub(x_offset)?;
        let height = y.checked_sub(y_offset)?;
        (width > 0 && height > 0).then_some((width, height))
    }
    /// Walk boxes in `bytes`; descend into `jp2h`, answer from `ihdr` or `jp2c`.
    fn walk_boxes(mut bytes: &[u8]) -> Option<(u32, u32)> {
        while bytes.len() >= 8 {
            let declared = be_u32(bytes, 0)? as u64;
            let kind = &bytes[4..8];
            let (header_len, box_len) = match declared {
                0 => (8usize, bytes.len() as u64),
                1 => {
                    let slice = bytes.get(8..16)?;
                    (16usize, u64::from_be_bytes(slice.try_into().ok()?))
                }
                n => (8usize, n),
            };
            if box_len < header_len as u64 {
                return None;
            }
            // A box may extend past the prefix; read what the prefix holds.
            let end = usize::try_from(box_len)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            let content = &bytes[header_len..end];
            match kind {
                b"jp2h" => return walk_boxes(content),
                b"ihdr" => {
                    let height = be_u32(content, 0)?;
                    let width = be_u32(content, 4)?;
                    return (width > 0 && height > 0).then_some((width, height));
                }
                b"jp2c" => return siz_dimensions(content),
                _ => {}
            }
            if end >= bytes.len() {
                return None;
            }
            bytes = &bytes[end..];
        }
        None
    }

    if prefix.starts_with(&JP2_SIGNATURE) {
        walk_boxes(&prefix[JP2_SIGNATURE.len()..])
    } else {
        siz_dimensions(prefix)
    }
}

/// Header-only dimensions of an image file on disk: the `image` crate's
/// container probe, then the JPEG 2000 header sniffer for what it cannot parse.
fn file_image_dimensions(path: &Path) -> Option<(u32, u32)> {
    if let Ok(dims) = image::image_dimensions(path) {
        return Some(dims);
    }
    let file = std::fs::File::open(path).ok()?;
    let mut prefix = Vec::new();
    file.take(IMAGE_HEADER_PREFIX_BYTES)
        .read_to_end(&mut prefix)
        .ok()?;
    jp2_header_dimensions(&prefix)
}

fn decode_zip_image(zip_path: &Path, selected: &ZipImageEntry) -> Result<RgbImage> {
    let bytes = read_zip_entry(zip_path, selected)?;
    decode_image_bytes(Path::new(&selected.name), &bytes)
        .with_context(|| format!("Failed to decode ZIP image: {}", selected.name))
}

/// One ZIP entry's bytes, extracted into memory under the size limit.
fn read_zip_entry(zip_path: &Path, selected: &ZipImageEntry) -> Result<Vec<u8>> {
    if selected.uncompressed_size > MAX_ZIP_IMAGE_BYTES {
        return Err(anyhow!(
            "ZIP image is too large to extract safely ({} bytes, limit {}): {}",
            selected.uncompressed_size,
            MAX_ZIP_IMAGE_BYTES,
            selected.name
        ));
    }
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("Failed to reopen ZIP: {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("Failed to read ZIP: {}", zip_path.display()))?;
    let entry = archive.by_index(selected.archive_index).with_context(|| {
        format!(
            "Failed to extract ZIP entry {} from {}",
            selected.name,
            zip_path.display()
        )
    })?;
    let mut bytes = Vec::with_capacity(selected.uncompressed_size.min(64 * 1024 * 1024) as usize);
    entry
        .take(MAX_ZIP_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("Failed to extract ZIP image: {}", selected.name))?;
    if bytes.len() as u64 > MAX_ZIP_IMAGE_BYTES {
        return Err(anyhow!(
            "ZIP image exceeded the {} byte safety limit while extracting: {}",
            MAX_ZIP_IMAGE_BYTES,
            selected.name
        ));
    }
    Ok(bytes)
}

fn decode_image_bytes(name: &Path, bytes: &[u8]) -> Result<RgbImage> {
    let is_jp2 = name
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jp2"));
    if is_jp2 {
        #[cfg(feature = "jp2-lam")]
        {
            let (width, height, rgb_data) = crate::encoding::jp2::decode_jp2_bytes(bytes)
                .map_err(|e| anyhow!("JP2 cannot be decoded: {e}"))?;
            return RgbImage::from_raw(width, height, rgb_data)
                .ok_or_else(|| anyhow!("JP2 decoder returned an invalid RGB buffer"));
        }
        #[cfg(not(feature = "jp2-lam"))]
        return Err(anyhow!(
            "JP2 support not compiled in (enable jp2-lam feature)"
        ));
    }
    image::load_from_memory(bytes)
        .map_err(anyhow::Error::msg)
        .map(|image| image.to_rgb8())
}

fn select_primary_zip_image_set(entries: Vec<ZipImageEntry>) -> Vec<ZipImageEntry> {
    let mut by_ext: std::collections::HashMap<String, Vec<ZipImageEntry>> =
        std::collections::HashMap::new();
    for entry in entries {
        let ext = Path::new(&entry.name)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        by_ext.entry(ext).or_default().push(entry);
    }
    let mut selected = by_ext
        .into_iter()
        .max_by(|(ext_a, a), (ext_b, b)| {
            a.len()
                .cmp(&b.len())
                .then_with(|| (ext_a == "jp2").cmp(&(ext_b == "jp2")))
        })
        .map(|(_, entries)| entries)
        .unwrap_or_default();
    selected.sort_by(|a, b| compare_image_names(Path::new(&a.name), Path::new(&b.name)));
    selected
}

fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| SUPPORTED_IMAGE_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn trailing_number(s: &str) -> Option<u64> {
    let rev_digits: String = s.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    if rev_digits.is_empty() {
        return None;
    }
    let digits: String = rev_digits.chars().rev().collect();
    digits.parse().ok()
}

fn compare_image_names(a: &Path, b: &Path) -> CmpOrdering {
    let a_number = a
        .file_stem()
        .and_then(|value| value.to_str())
        .and_then(trailing_number);
    let b_number = b
        .file_stem()
        .and_then(|value| value.to_str())
        .and_then(trailing_number);
    match (a_number, b_number) {
        (Some(a_number), Some(b_number)) => a_number.cmp(&b_number).then_with(|| a.cmp(b)),
        (Some(_), None) => CmpOrdering::Less,
        (None, Some(_)) => CmpOrdering::Greater,
        (None, None) => a.cmp(b),
    }
}

// ════════════════════════════════════════════════════════════════════════════════
// Flatten page: photographed pages, flattened before anything else sees them
// ════════════════════════════════════════════════════════════════════════════════

/// One decoded photo, upright (its EXIF orientation applied), at native size.
pub struct Photo {
    pub image: RgbImage,
    /// 35 mm equivalent focal length from EXIF, for the page aspect estimate.
    pub focal_length_35mm: Option<f64>,
}

/// The photos behind a flattened source: a folder's files or a ZIP's entries.
pub trait PhotoSet: Send + Sync {
    fn len(&self) -> usize;
    /// Header-only pixel dimensions, as [`PageSource::native_page_size`].
    fn dimensions(&self, index: usize) -> Option<(u32, u32)>;
    /// Decode one photo. Blocking.
    fn decode(&self, index: usize) -> Result<Photo>;
    /// Page points per pixel of the decoded photo. Image files have no
    /// physical size, so a pixel is a point; a rendered PDF page keeps its own.
    fn points_per_pixel(&self, index: usize) -> f32 {
        let _ = index;
        1.0
    }
}

/// The pages of a PDF as photos: a PDF whose pages are photographs (a phone
/// scan, a camera book scan) is flattened like a folder of them. Each page is
/// rendered above the target height so the one resampling of the flatten
/// render has pixels to work with.
pub struct PdfPhotos {
    renderer: PdfRenderer,
    /// Zero-based document pages, in order: the job's page range.
    pages: Vec<u32>,
    render_height: u32,
}

impl PdfPhotos {
    /// `page_range` is the job's one-based inclusive range; the photos are
    /// its pages, so the job has no further range to apply.
    pub fn open(
        pdf_bytes: Arc<[u8]>,
        page_range: Option<&crate::pipeline::config::PageRange>,
        render_height: u32,
    ) -> Result<Self> {
        let document = Arc::new(
            RenderSession::open(pdf_bytes, None)
                .map_err(|error| anyhow!("Failed to read PDF document: {error}"))?,
        );
        let mut raster_cfg = PdfRasterConfig::default();
        raster_cfg.render_forms = false;
        let renderer = PdfRenderer::from_session(document, raster_cfg);
        let count = renderer.page_count();
        let pages: Vec<u32> = match page_range {
            Some(range) => {
                (range.start.saturating_sub(1) as u32..(range.end as u32).min(count)).collect()
            }
            None => (0..count).collect(),
        };
        if pages.is_empty() {
            return Err(anyhow!("No pages to process in the PDF"));
        }
        Ok(Self {
            renderer,
            pages,
            render_height: render_height.max(1),
        })
    }

    /// (width, height) in points of the page as displayed.
    fn display_size(&self, index: usize) -> Option<(f32, f32)> {
        let page = *self.pages.get(index)?;
        let geometry = self.renderer.document_session().page_geometry(page).ok()?;
        let (width, height) = (
            geometry.display_width() as f32,
            geometry.display_height() as f32,
        );
        (width > 0.0 && height > 0.0).then_some((width, height))
    }
}

impl PhotoSet for PdfPhotos {
    fn len(&self) -> usize {
        self.pages.len()
    }
    fn dimensions(&self, index: usize) -> Option<(u32, u32)> {
        let (width, height) = self.display_size(index)?;
        Some((
            ((self.render_height as f32 * width / height).round() as u32).max(1),
            self.render_height,
        ))
    }
    fn decode(&self, index: usize) -> Result<Photo> {
        let page = *self
            .pages
            .get(index)
            .ok_or_else(|| anyhow!("PDF page index {index} out of bounds"))?;
        let rendered = self
            .renderer
            .render_page_rgb_sync(page, self.render_height, None)
            .map_err(|error| anyhow!("Failed to render page {}: {error}", page + 1))?;
        let image = RgbImage::from_raw(rendered.width, rendered.height, rendered.data)
            .ok_or_else(|| anyhow!("Rendered page {} has an invalid buffer", page + 1))?;
        Ok(Photo {
            image,
            focal_length_35mm: None,
        })
    }
    fn points_per_pixel(&self, index: usize) -> f32 {
        self.display_size(index)
            .map(|(_, height)| height / self.render_height as f32)
            .unwrap_or(1.0)
    }
}

/// The photos as pages with nothing flattened: what a flattened source falls
/// back to when the model cannot run.
pub fn unflattened_photos(
    photos: Arc<dyn PhotoSet>,
    concurrency: usize,
    target_height: u32,
    target_width: Option<u32>,
) -> Arc<dyn PageSource> {
    let plans = (0..photos.len())
        .map(|_| (crate::dewarp::PhotoPlan::Untouched, None))
        .collect();
    Arc::new(FlattenedPageSource::from_plans(
        photos,
        plans,
        concurrency,
        target_height,
        target_width,
    ))
}

struct FolderPhotos(Arc<Vec<PathBuf>>);

impl PhotoSet for FolderPhotos {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn dimensions(&self, index: usize) -> Option<(u32, u32)> {
        file_image_dimensions(self.0.get(index)?)
    }
    fn decode(&self, index: usize) -> Result<Photo> {
        let path = self
            .0
            .get(index)
            .ok_or_else(|| anyhow!("Image index {index} out of bounds"))?;
        let is_jp2 = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jp2"));
        if is_jp2 {
            return Ok(Photo {
                image: decode_folder_image(path)?,
                focal_length_35mm: None,
            });
        }
        let reader = image::ImageReader::open(path)
            .with_context(|| format!("Failed to open image: {}", path.display()))?;
        decode_upright(reader).with_context(|| format!("Failed to decode: {}", path.display()))
    }
}

struct ZipPhotos {
    zip_path: PathBuf,
    entries: Arc<Vec<ZipImageEntry>>,
}

impl PhotoSet for ZipPhotos {
    fn len(&self) -> usize {
        self.entries.len()
    }
    fn dimensions(&self, index: usize) -> Option<(u32, u32)> {
        zip_image_dimensions(&self.zip_path, self.entries.get(index)?)
    }
    fn decode(&self, index: usize) -> Result<Photo> {
        let entry = self
            .entries
            .get(index)
            .ok_or_else(|| anyhow!("ZIP image index {index} out of bounds"))?;
        let is_jp2 = Path::new(&entry.name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jp2"));
        if is_jp2 {
            return Ok(Photo {
                image: decode_zip_image(&self.zip_path, entry)?,
                focal_length_35mm: None,
            });
        }
        let bytes = read_zip_entry(&self.zip_path, entry)?;
        decode_upright(image::ImageReader::new(std::io::Cursor::new(bytes)))
            .with_context(|| format!("Failed to decode ZIP image: {}", entry.name))
    }
}

/// Decode upright (EXIF orientation applied), keeping the focal length.
fn decode_upright<R: std::io::BufRead + std::io::Seek>(
    reader: image::ImageReader<R>,
) -> Result<Photo> {
    let (image, focal_length_35mm) = crate::dewarp::decode_upright(reader)?;
    Ok(Photo {
        image,
        focal_length_35mm,
    })
}

impl ImageFolderPageSource {
    /// The folder's files as photos for [`flatten_photos`].
    pub fn photos(&self) -> Arc<dyn PhotoSet> {
        Arc::new(FolderPhotos(Arc::clone(&self.files)))
    }
}

impl ZipImagePageSource {
    /// The ZIP's entries as photos for [`flatten_photos`].
    pub fn photos(&self) -> Arc<dyn PhotoSet> {
        Arc::new(ZipPhotos {
            zip_path: self.zip_path.clone(),
            entries: Arc::clone(&self.entries),
        })
    }
}

/// A source whose pages are the flattened pages of a set of photos.
///
/// One photographed spread becomes two pages, while [`PageSource`] promises
/// a fixed page count. So every photo is surveyed at construction: decoded,
/// run through the model's passes, and reduced to its plan (untouched, or one
/// or two small grids); the pixels are dropped. `load_page` decodes the photo
/// again and renders that one page from its stored grid, at the photo's own
/// resolution, before the resize to the target height.
pub struct FlattenedPageSource {
    photos: Arc<dyn PhotoSet>,
    plans: Arc<Vec<(crate::dewarp::PhotoPlan, Option<f64>)>>,
    /// Output page -> (photo, page of that photo).
    pages: Vec<(usize, usize)>,
    /// The last photo decoded for a spread: its two pages are asked for back
    /// to back, so the second one is usually served from here.
    last_photo: Arc<std::sync::Mutex<Option<(usize, Arc<Photo>)>>>,
    concurrency: usize,
    target_height: u32,
    target_width: Option<u32>,
}

impl FlattenedPageSource {
    fn from_plans(
        photos: Arc<dyn PhotoSet>,
        plans: Vec<(crate::dewarp::PhotoPlan, Option<f64>)>,
        concurrency: usize,
        target_height: u32,
        target_width: Option<u32>,
    ) -> Self {
        let pages = plans
            .iter()
            .enumerate()
            .flat_map(|(photo, (plan, _))| (0..plan.page_count()).map(move |page| (photo, page)))
            .collect();
        Self {
            photos,
            plans: Arc::new(plans),
            pages,
            last_photo: Arc::new(std::sync::Mutex::new(None)),
            concurrency: concurrency.max(1),
            target_height,
            target_width,
        }
    }

    /// (photos, photos left as they came, photos that became two pages).
    fn summary(&self) -> (usize, usize, usize) {
        let untouched = self
            .plans
            .iter()
            .filter(|(plan, _)| matches!(plan, crate::dewarp::PhotoPlan::Untouched))
            .count();
        let spreads = self
            .plans
            .iter()
            .filter(|(plan, _)| plan.page_count() == 2)
            .count();
        (self.plans.len(), untouched, spreads)
    }
}

/// Wrap a folder or ZIP source so that its photographed pages come out flat
/// ("Flatten page"). Surveys every photo first; that is one extra decode per
/// photo and its model calls before the first page appears.
///
/// Flattening needs a hardware GPU and the model. Without them the step
/// switches itself off: one line says so and `plain` is returned unchanged.
pub fn flatten_photos(
    plain: Arc<dyn PageSource>,
    photos: Arc<dyn PhotoSet>,
    settings: &crate::dewarp::Settings,
    concurrency: usize,
    target_height: u32,
    target_width: Option<u32>,
    progress: Option<&ProgressTracker>,
) -> Result<Arc<dyn PageSource>> {
    let message = |text: String| {
        if let Some(tracker) = progress {
            tracker.update(crate::progress::ProcessingStatus::PipelineMessage {
                stage: "Flatten page".to_string(),
                message: text,
            });
        }
    };
    message("Loading the flatten model onto the GPU...".to_string());
    let flattener = match crate::dewarp::Flattener::shared() {
        Ok(flattener) => flattener,
        Err(reason) => {
            let text = format!("Flatten page is skipped, pages are used as they are: {reason}");
            eprintln!("{text}");
            message(text);
            return Ok(plain);
        }
    };
    let count = photos.len();
    let mut plans = Vec::with_capacity(count);
    for index in 0..count {
        crate::progress::cancellation_checkpoint("while flattening pages")?;
        message(format!("Looking at photo {} of {count}", index + 1));
        let photo = photos.decode(index)?;
        let plan = flattener.survey(&photo.image, photo.focal_length_35mm, settings);
        plans.push((plan, photo.focal_length_35mm));
    }
    let source =
        FlattenedPageSource::from_plans(photos, plans, concurrency, target_height, target_width);
    let (photos, untouched, spreads) = source.summary();
    let text = format!(
        "Flatten page: {photos} photos, {} pages ({untouched} left as they are, {spreads} spreads)",
        source.pages.len()
    );
    crate::info_println!("{text}");
    message(text);
    Ok(Arc::new(source))
}

#[async_trait]
impl PageSource for FlattenedPageSource {
    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn source_concurrency(&self) -> usize {
        self.concurrency
    }

    fn native_page_size(&self, page_index: usize) -> Option<(u32, u32)> {
        // The photo's size: its decode is the peak allocation of a page.
        let (photo, _) = *self.pages.get(page_index)?;
        self.photos.dimensions(photo)
    }

    fn decodes_at_native_size(&self) -> bool {
        true
    }

    async fn load_page(&self, page_index: usize) -> Result<SourcePage> {
        let (photo_index, side) = *self
            .pages
            .get(page_index)
            .ok_or_else(|| anyhow!("Flattened page index {} out of bounds", page_index))?;
        let photos = Arc::clone(&self.photos);
        let plans = Arc::clone(&self.plans);
        let last_photo = Arc::clone(&self.last_photo);
        let points_per_pixel = self.photos.points_per_pixel(photo_index);
        let image = crate::runtime_stats::spawn_blocking(move || -> Result<RgbImage> {
            let (plan, focal) = &plans[photo_index];
            let crate::dewarp::PhotoPlan::Pages(grids) = plan else {
                return Ok(photos.decode(photo_index)?.image);
            };
            let cached = last_photo
                .lock()
                .ok()
                .and_then(|mut slot| match slot.as_ref() {
                    Some((index, _)) if *index == photo_index => slot.take().map(|(_, p)| p),
                    _ => None,
                });
            let photo = match cached {
                Some(photo) => photo,
                None => Arc::new(photos.decode(photo_index)?),
            };
            let page = crate::dewarp::render_page(&photo.image, &grids[side], *focal);
            if side + 1 < grids.len() {
                if let Ok(mut slot) = last_photo.lock() {
                    *slot = Some((photo_index, Arc::clone(&photo)));
                }
            }
            match page {
                Some(page) => Ok(page),
                // The survey ruled this out; a page is never failed for it.
                None => Ok(Arc::try_unwrap(photo)
                    .map(|photo| photo.image)
                    .unwrap_or_else(|shared| shared.image.clone())),
            }
        })
        .await
        .map_err(|e| anyhow!("Flatten task panicked: {}", e))??;
        // The flattened page is what is normalised to the target height, and
        // its size, not the photo's, is the page's.
        let (width, height) = image.dimensions();
        let image = normalize_folder_image_to_target(
            image,
            self.target_height,
            self.target_width,
            page_index,
        )?;
        Ok(SourcePage {
            image,
            original_width_pts: width as f32 * points_per_pixel,
            original_height_pts: height as f32 * points_per_pixel,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat};
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    fn temp_dir_with(names: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lege_source_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for name in names {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        dir
    }

    fn stems(paths: &[PathBuf]) -> Vec<String> {
        let mut out: Vec<String> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    fn ordered_names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn write_png(path: &Path, width: u32, height: u32) {
        let image = RgbImage::from_pixel(width, height, image::Rgb([9, 9, 9]));
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image)
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        std::fs::write(path, png.into_inner()).unwrap();
    }

    #[test]
    fn folder_source_reports_native_dimensions() {
        let dir = temp_dir_with(&[]);
        let path = dir.join("scan_01.png");
        write_png(&path, 37, 53);
        let source = ImageFolderPageSource::new(vec![path], 2, 1200, None);
        assert_eq!(source.native_page_size(0), Some((37, 53)));
        assert_eq!(source.native_page_size(7), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn zip_source_sniffs_entry_dimensions_without_decoding() {
        let zip_path = write_image_zip(&["a/page_1.png", "a/page_2.png"]);
        let source = ZipImagePageSource::open(zip_path.clone(), 2, 2, None).unwrap();
        assert_eq!(source.native_page_size(0), Some((2, 2)));
        assert_eq!(source.native_page_size(1), Some((2, 2)));
        assert_eq!(source.native_page_size(9), None);
        std::fs::remove_dir_all(zip_path.parent().unwrap()).ok();
    }

    #[test]
    fn jp2_header_sniffer_reads_bare_codestream_siz() {
        // SOC, SIZ, Lsiz, Rsiz, Xsiz=130, Ysiz=95, XOsiz=10, YOsiz=5.
        let mut codestream = vec![0xFF, 0x4F, 0xFF, 0x51, 0x00, 0x29, 0x00, 0x00];
        for value in [130u32, 95, 10, 5] {
            codestream.extend_from_slice(&value.to_be_bytes());
        }
        assert_eq!(jp2_header_dimensions(&codestream), Some((120, 90)));
        assert_eq!(jp2_header_dimensions(&codestream[..14]), None);
        assert_eq!(jp2_header_dimensions(b"\x89PNG\r\n\x1a\n"), None);
    }

    #[cfg(feature = "jp2-lam")]
    fn encoded_jp2(width: u32, height: u32) -> Vec<u8> {
        let rgb = vec![128u8; (width * height * 3) as usize];
        crate::encoding::jp2::encode_rgb(&rgb, width, height, 60).unwrap()
    }

    #[cfg(feature = "jp2-lam")]
    #[test]
    fn jp2_sources_report_native_dimensions_from_the_header() {
        let jp2 = encoded_jp2(41, 67);
        assert_eq!(jp2_header_dimensions(&jp2), Some((41, 67)));

        let dir = temp_dir_with(&[]);
        let path = dir.join("page_0001.jp2");
        std::fs::write(&path, &jp2).unwrap();
        let folder = ImageFolderPageSource::new(vec![path], 2, 1200, None);
        assert_eq!(folder.native_page_size(0), Some((41, 67)));

        let zip_path = dir.join("book_jp2.zip");
        let mut archive = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        for name in ["book_jp2/page_0001.jp2", "book_jp2/page_0002.jp2"] {
            archive
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&jp2).unwrap();
        }
        archive.finish().unwrap();
        let zip = ZipImagePageSource::open(zip_path, 2, 1200, None).unwrap();
        assert_eq!(zip.native_page_size(0), Some((41, 67)));
        assert_eq!(zip.native_page_size(1), Some((41, 67)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn native_decode_peak_is_charged_above_target_working_set() {
        // A 6000x8000 native RGB image decoded, then resized to a 1200-high
        // target: the charge must cover the decode (~137 MiB plus scratch),
        // not just the small target working set (~33 MiB).
        let target_only = estimate_page_charge_mb(1800, 1200, None, true, 8192);
        let with_native = estimate_page_charge_mb(1800, 1200, Some((6000, 8000)), true, 8192);
        assert!(with_native.estimated_mb > target_only.estimated_mb);
        assert!(with_native.estimated_mb > 137);
        assert!(!with_native.oversized);
    }

    #[test]
    fn directly_rendered_source_uses_exact_render_dims() {
        // A PDF reports exact render dims: no decode extra, working set from
        // the real aspect instead of the configured-size guess.
        let charge = estimate_page_charge_mb(1800, 1200, Some((900, 1200)), false, 8192);
        assert_eq!(charge.charged_work_pixels, 900 * 1200);
        assert!(!charge.oversized);
    }

    #[test]
    fn oversized_page_is_flagged_and_clamped_to_budget() {
        let charge = estimate_page_charge_mb(1800, 1200, Some((6000, 8000)), true, 64);
        assert!(charge.oversized);
        assert_eq!(charge.acquire_mb, 64);
        assert!(charge.estimated_mb > 64);
    }

    #[test]
    fn top_up_covers_actual_dims_beyond_charge() {
        let charge = estimate_page_charge_mb(100, 100, None, false, 8192);
        assert_eq!(top_up_charge_mb(&charge, 100, 100), 0);
        assert_eq!(top_up_charge_mb(&charge, 50, 50), 0);
        assert!(top_up_charge_mb(&charge, 2000, 2000) > 0);
    }

    fn write_image_zip(names: &[&str]) -> PathBuf {
        let dir = temp_dir_with(&[]);
        let zip_path = dir.join("archive_jp2.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        for (index, name) in names.iter().enumerate() {
            let image = RgbImage::from_pixel(2, 2, image::Rgb([index as u8, 20, 30]));
            let mut png = Cursor::new(Vec::new());
            DynamicImage::ImageRgb8(image)
                .write_to(&mut png, ImageFormat::Png)
                .unwrap();
            archive
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            archive.write_all(png.get_ref()).unwrap();
        }
        archive.finish().unwrap();
        zip_path
    }

    #[test]
    fn non_consecutive_numbering_keeps_every_page() {
        let dir = temp_dir_with(&["page_28.png", "page_50.png", "page_100.png"]);
        let run = collect_largest_sequential_image_run(&dir).unwrap();
        assert_eq!(
            ordered_names(&run),
            vec!["page_28.png", "page_50.png", "page_100.png"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn consecutive_numbering_keeps_all_pages() {
        let dir = temp_dir_with(&["page_1.png", "page_2.png", "page_3.png"]);
        let run = collect_largest_sequential_image_run(&dir).unwrap();
        assert_eq!(stems(&run), vec!["page_1.png", "page_2.png", "page_3.png"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_format_cover_is_retained_without_silent_data_loss() {
        let dir = temp_dir_with(&["cover.png", "page_10.png", "page_11.png", "page_12.png"]);
        let run = collect_largest_sequential_image_run(&dir).unwrap();
        assert_eq!(
            stems(&run),
            vec!["cover.png", "page_10.png", "page_11.png", "page_12.png"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn zip_source_streams_nested_images_in_numeric_order() {
        let zip_path = write_image_zip(&[
            "book_jp2/page_100.png",
            "book_jp2/page_28.png",
            "book_jp2/page_50.png",
            "cover.jpg",
        ]);
        let source = ZipImagePageSource::open(zip_path.clone(), 3, 2, None).unwrap();

        assert_eq!(source.page_count(), 3);
        assert_eq!(
            source
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "book_jp2/page_28.png",
                "book_jp2/page_50.png",
                "book_jp2/page_100.png"
            ]
        );
        let page = source.load_page(0).await.unwrap();
        assert_eq!(page.image.dimensions(), (2, 2));
        assert_eq!(page.image.get_pixel(0, 0).0, [1, 20, 30]);

        std::fs::remove_dir_all(zip_path.parent().unwrap()).ok();
    }

    struct TestPhotos(Vec<RgbImage>);

    impl PhotoSet for TestPhotos {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn dimensions(&self, index: usize) -> Option<(u32, u32)> {
            self.0.get(index).map(|image| image.dimensions())
        }
        fn decode(&self, index: usize) -> Result<Photo> {
            Ok(Photo {
                image: self.0[index].clone(),
                focal_length_35mm: None,
            })
        }
    }

    /// An identity grid over the columns `low..high` (-1 to 1) of a photo.
    fn column_grid(low: f64, high: f64) -> crate::dewarp::geom::Grid {
        let (rows, cols) = (9usize, 7usize);
        let mut nodes = Vec::new();
        for i in 0..rows {
            for j in 0..cols {
                nodes.push([
                    low + (high - low) * j as f64 / (cols - 1) as f64,
                    i as f64 / (rows - 1) as f64 * 2.0 - 1.0,
                ]);
            }
        }
        crate::dewarp::geom::Grid { rows, cols, nodes }
    }

    /// A flat scan, a spread and a single curved page: the document has four
    /// pages in source order, the spread as its left then its right half, and
    /// each page reports its own size.
    #[test]
    fn a_spread_becomes_two_pages_in_source_order() {
        use crate::dewarp::PhotoPlan;
        let photo = |w: u32, h: u32, left: u8, right: u8| {
            RgbImage::from_fn(w, h, |x, _| {
                let v = if x < w / 2 { left } else { right };
                image::Rgb([v, v, v])
            })
        };
        let photos = Arc::new(TestPhotos(vec![
            photo(40, 60, 10, 10),
            photo(120, 80, 100, 200),
            photo(40, 64, 30, 30),
        ]));
        let plans = vec![
            (PhotoPlan::Untouched, None),
            (
                PhotoPlan::Pages(vec![column_grid(-1.0, 0.0), column_grid(0.0, 1.0)]),
                None,
            ),
            (PhotoPlan::Pages(vec![column_grid(-1.0, 1.0)]), None),
        ];
        // Target height 0 keeps every page at its flattened size.
        let source = FlattenedPageSource::from_plans(photos, plans, 2, 0, None);
        assert_eq!(source.page_count(), 4);
        assert_eq!(source.summary(), (3, 1, 1));
        assert_eq!(source.native_page_size(1), Some((120, 80)));
        assert_eq!(source.native_page_size(2), Some((120, 80)));
        assert!(source.decodes_at_native_size());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let pages: Vec<SourcePage> = (0..4)
            .map(|index| runtime.block_on(source.load_page(index)).unwrap())
            .collect();
        let sizes: Vec<(u32, u32)> = pages.iter().map(|p| p.image.dimensions()).collect();
        assert_eq!(sizes, vec![(40, 60), (60, 80), (60, 80), (40, 64)]);
        let tone = |page: &SourcePage| page.image.get_pixel(5, 5).0[0];
        assert_eq!(
            pages.iter().map(tone).collect::<Vec<_>>(),
            vec![10, 100, 200, 30]
        );
        assert_eq!(pages[1].original_width_pts, 60.0);
        assert_eq!(pages[1].original_height_pts, 80.0);
        // The spread's photo was handed from its first page to its second.
        assert!(source.last_photo.lock().unwrap().is_none());
        assert!(runtime.block_on(source.load_page(4)).is_err());
    }
}
