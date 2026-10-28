use crate::pagerender::prelude::{PdfRenderer, RasterConfig as PdfRasterConfig};
use crate::pipeline::config::{PipelineConfig, RenderedPageData};
use crate::pipeline::policies::build_inference_image;
use crate::progress::ProgressTracker;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures_util::stream::{FuturesUnordered, StreamExt};
use image::RgbImage;
use lege_pdf_read::RenderSession;
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
        _cancellation: lege_pdf_read::CancellationToken,
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
        cancellation: lege_pdf_read::CancellationToken,
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
        let mut raster_cfg = PdfRasterConfig::default();
        raster_cfg.render_forms = false;
        raster_cfg.target_width = config.target_width();
        let renderer = Arc::new(PdfRenderer::new_from_bytes(pdf_bytes, raster_cfg)?);
        Ok(Self { renderer, config })
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
        cancellation: lege_pdf_read::CancellationToken,
    ) -> Result<SourcePage> {
        self.load_pdf_page(page_index, Some(cancellation)).await
    }

    async fn load_analysis_page(
        &self,
        page_index: usize,
        analysis_width: u32,
        cancellation: lege_pdf_read::CancellationToken,
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
        cancellation: Option<lege_pdf_read::CancellationToken>,
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
    cancellation: lege_pdf_read::CancellationToken,
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
    decode_image_bytes(Path::new(&selected.name), &bytes)
        .with_context(|| format!("Failed to decode ZIP image: {}", selected.name))
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
}
