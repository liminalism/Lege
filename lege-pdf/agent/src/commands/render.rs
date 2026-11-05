use std::io::Write;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use pdf_document::{DocumentSnapshot, PageIndex};
use pdf_page_ir::{DeviceSize, Matrix};
use pdf_render_api::{
    AnnotationMode, Background, HostPage, OutputFormat, OutputResidency, PageTransform,
    RenderQuality, RenderRequest,
};
use pdf_render_cpu::{CpuBackend, CpuBackendOptions};

use crate::bounds::Bounds;
use crate::commands::{
    PageRun, default_annotations, page_compiler, parse_context, render_limits, resolve_snapshot,
    run_pages,
};
use crate::open::DocumentIdentity;
use crate::pages::{PageZero, parse_page_range};
use crate::schema::{Envelope, OutputMode};
use crate::views::render::RenderPageData;

/// `--output -`: write the single rendered image to stdout.
const STDOUT_TEMPLATE: &str = "-";
/// Longest edge of a `--thumbnail`, in pixels.
const THUMBNAIL_EDGE: u32 = 512;
/// A fit-to-size render never magnifies beyond this many pixels per point
/// (576 dpi): fitting a thumbnail-sized crop to a large edge should not ask
/// the rasterizer for a poster.
const MAX_FIT_SCALE: f64 = 8.0;
/// Rendered pages held in memory at once across all workers, in bytes.
const RENDER_MEMORY_BUDGET: u64 = 768 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ImageFormat {
    Png,
    Ppm,
    /// Grayscale PNM; the page is rendered in gray.
    Pgm,
    /// Lossy, a fraction of the size of PNG for scans and photographs.
    Jpeg,
}

impl ImageFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Ppm => "ppm",
            Self::Pgm => "pgm",
            Self::Jpeg => "jpeg",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Ppm => "image/x-portable-pixmap",
            Self::Pgm => "image/x-portable-graymap",
            Self::Jpeg => "image/jpeg",
        }
    }
}

/// What to draw and how large, independent of where the image goes.
#[derive(Debug, Clone, Copy)]
pub struct RenderSpec {
    pub dpi: Option<f64>,
    pub scale: Option<f64>,
    /// Fit the longest edge to this many pixels. Alone, it sets the size (a
    /// crop is magnified to fill it); beside `dpi`/`scale` it only caps it.
    pub max_edge: Option<u32>,
    /// PDF-space rectangle X0,Y0,X1,Y1 to render instead of the whole page.
    pub crop: Option<[f64; 4]>,
    /// Draft quality at 72 dpi, longest edge capped at 512 pixels.
    pub thumbnail: bool,
    /// Render in grayscale.
    pub gray: bool,
    pub format: ImageFormat,
    /// JPEG quality, 1-100.
    pub quality: u8,
    /// When the format is PNG and the PNG comes out large, send JPEG
    /// instead if that is less than half the size. For images returned to
    /// be looked at: a scan or a photograph is several megabytes as PNG and
    /// a few hundred kilobytes as JPEG, while a page of text is small and
    /// sharper as PNG.
    pub prefer_small: bool,
}

impl Default for RenderSpec {
    fn default() -> Self {
        Self {
            dpi: None,
            scale: None,
            max_edge: None,
            crop: None,
            thumbnail: false,
            gray: false,
            format: ImageFormat::Png,
            quality: 85,
            prefer_small: false,
        }
    }
}

#[derive(Debug)]
pub struct RenderArgs<'a> {
    pub path: &'a Path,
    pub password: Option<&'a str>,
    pub pages: Option<&'a str>,
    pub output: &'a str,
    pub spec: RenderSpec,
    /// Tile the selected pages into one image instead of one image each.
    pub contact_sheet: bool,
    /// Columns of a contact sheet (0 = chosen from the page count).
    pub columns: u32,
    pub system_fonts: bool,
    pub bounds: Bounds,
    pub fail_fast: bool,
    pub output_mode: OutputMode,
    pub snapshot: Option<Arc<DocumentSnapshot>>,
    pub identity: Option<DocumentIdentity>,
}

pub fn run(args: RenderArgs<'_>) -> Result<i32> {
    let (identity, snapshot) =
        resolve_snapshot(args.path, args.password, args.snapshot, args.identity)?;
    let document = identity.display_path();
    let (page_indices, global_warnings) =
        parse_page_range(args.pages, snapshot.page_count(), args.bounds.max_pages)?;

    if page_indices.is_empty() {
        bail!("no pages selected");
    }
    let to_stdout = args.output == STDOUT_TEMPLATE;
    if to_stdout {
        // Records and the image would share one stream, and two images
        // concatenated on it could not be told apart.
        if page_indices.len() != 1 && !args.contact_sheet {
            bail!(
                "--output - writes one image to stdout; select a single page (got {})",
                page_indices.len()
            );
        }
        if !matches!(args.output_mode, OutputMode::Human) {
            bail!("--output - cannot be combined with --json or --jsonl: both write to stdout");
        }
    }
    let spec = args.spec;
    spec.validate()?;
    let stem = identity
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("page");
    let backend = CpuBackend::new(CpuBackendOptions::default());

    if args.contact_sheet {
        let mut warnings = global_warnings;
        let sheet = contact_sheet(
            &snapshot,
            &page_indices,
            &spec,
            args.columns,
            args.system_fonts,
            args.bounds.threads,
            &mut warnings,
        )?;
        let first = page_indices[0];
        let path = expand_template(args.output, first.one_based(), first.0, stem);
        write_image(&path, &sheet.image.bytes)?;
        let data = sheet.view(path.clone());
        let env = Envelope::ok(&document, serde_json::to_value(&data)?).with_warnings(warnings);
        return match args.output_mode {
            OutputMode::Human => {
                let line = format!(
                    "wrote {path} ({}x{}, {} pages in {} columns)",
                    data.width,
                    data.height,
                    data.pages.len(),
                    data.columns
                );
                if to_stdout {
                    eprintln!("{line}");
                } else {
                    println!("{line}");
                }
                for warning in &env.warnings {
                    eprintln!("warning: {warning}");
                }
                Ok(0)
            }
            OutputMode::Json => env.write_json().map(|()| 0),
            OutputMode::Jsonl => env.write_jsonl().map(|()| 0),
        };
    }

    let multi = page_indices.len() > 1 || matches!(args.output_mode, OutputMode::Jsonl);
    let run = PageRun {
        document: &document,
        mode: args.output_mode,
        multi,
        fail_fast: args.fail_fast,
        threads: render_workers(&snapshot, &page_indices, &spec, args.bounds.threads),
    };
    let system_fonts = args.system_fonts;
    run_pages(
        run,
        &page_indices,
        global_warnings,
        || page_compiler(system_fonts, default_annotations()),
        |compiler, pz, warnings| {
            let raster = rasterize(&snapshot, compiler, &backend, pz, &spec, warnings)?;
            let image = encode(&raster.host, spec.format, spec.quality)?;
            let path = expand_template(args.output, pz.one_based(), pz.0, stem);
            write_image(&path, &image.bytes)?;
            Ok(raster.view(path, &image))
        },
        |pz, data, warnings| {
            let env = Envelope::page_ok(&document, pz.one_based(), serde_json::to_value(&data)?)
                .with_warnings(warnings);
            emit(&env, &data, args.output_mode, multi)
        },
    )
}

/// One page rendered for a service request: the manifest, and the encoded
/// image itself when no output path was given.
#[derive(Debug)]
pub struct RenderedPage {
    pub page: PageZero,
    pub data: RenderPageData,
    pub image: Option<EncodedImage>,
    pub warnings: Vec<String>,
}

/// Render `pages` for `serve`/`mcp`: to files when `output` is a path
/// template, otherwise into memory for the caller to return inline.
///
/// Pages come back in order; a page that fails carries its error and does not
/// stop the others.
pub fn render_pages(
    snapshot: &DocumentSnapshot,
    pages: &[PageZero],
    spec: &RenderSpec,
    output: Option<&str>,
    stem: &str,
    system_fonts: bool,
    threads: u32,
) -> Result<Vec<(PageZero, Result<RenderedPage>)>> {
    if output == Some(STDOUT_TEMPLATE) {
        bail!(
            "output \"-\" is not available here: stdout carries the protocol; give a file path, or leave output out to get the image inline"
        );
    }
    spec.validate()?;
    let backend = CpuBackend::new(CpuBackendOptions::default());
    let workers =
        crate::parallel::worker_count(render_workers(snapshot, pages, spec, threads), pages.len());
    let mut out = Vec::with_capacity(pages.len());
    crate::parallel::ordered(
        pages,
        workers,
        || page_compiler(system_fonts, default_annotations()),
        |compiler, _, pz| -> Result<RenderedPage> {
            if crate::deadline::expired() {
                bail!("{}", crate::deadline::message(0, pages.len()));
            }
            let mut warnings = Vec::new();
            let raster = rasterize(snapshot, compiler, &backend, *pz, spec, &mut warnings)?;
            let image = spec.encode(pack(&raster.host), raster.host.width, raster.host.height)?;
            match output {
                Some(template) => {
                    let path = expand_template(template, pz.one_based(), pz.0, stem);
                    write_image(&path, &image.bytes)?;
                    Ok(RenderedPage {
                        page: *pz,
                        data: raster.view(path, &image),
                        image: None,
                        warnings,
                    })
                }
                None => Ok(RenderedPage {
                    page: *pz,
                    data: raster.view(String::new(), &image),
                    image: Some(image),
                    warnings,
                }),
            }
        },
        |_, pz, result| {
            out.push((*pz, result));
            ControlFlow::Continue(())
        },
    );
    Ok(out)
}

/// A PNG up to this size is sent as it is, whatever JPEG would save.
const SMALL_ENOUGH: usize = 300 * 1024;

impl RenderSpec {
    /// Encode in the format asked for, or the smaller one where
    /// [`Self::prefer_small`] allows the choice.
    fn encode(&self, packed: Packed, width: u32, height: u32) -> Result<EncodedImage> {
        if !self.prefer_small || self.format != ImageFormat::Png {
            return encode_packed(packed, width, height, self.format, self.quality);
        }
        let again = Packed {
            pixels: packed.pixels.clone(),
            channels: packed.channels,
        };
        let png = encode_packed(packed, width, height, ImageFormat::Png, self.quality)?;
        if png.bytes.len() <= SMALL_ENOUGH {
            return Ok(png);
        }
        let jpeg = encode_packed(again, width, height, ImageFormat::Jpeg, self.quality)?;
        Ok(if jpeg.bytes.len() * 2 < png.bytes.len() {
            jpeg
        } else {
            png
        })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        self.base_scale().map(|_| ())?;
        if self.max_edge == Some(0) {
            bail!("max-edge must be at least 1 pixel");
        }
        if !(1..=100).contains(&self.quality) {
            bail!("quality must be between 1 and 100");
        }
        Ok(())
    }

    /// Pixels per point before any fit-to-edge adjustment, and whether the
    /// caller chose it (as opposed to it being the default).
    fn base_scale(&self) -> Result<(f64, bool)> {
        if self.dpi.is_some() && self.scale.is_some() {
            bail!("specify only one of --dpi or --scale");
        }
        let default_dpi = if self.thumbnail { 72.0 } else { 150.0 };
        let dpi = self.dpi.unwrap_or(default_dpi);
        if !dpi.is_finite() || dpi <= 0.0 {
            bail!("dpi must be a positive number");
        }
        let scale = self.scale.unwrap_or(dpi / 72.0);
        if !scale.is_finite() || scale <= 0.0 {
            bail!("scale must be a positive number");
        }
        Ok((scale, self.dpi.is_some() || self.scale.is_some()))
    }

    /// The longest edge the image may have, if it is limited.
    fn edge_limit(&self) -> Option<u32> {
        match (self.max_edge, self.thumbnail) {
            (Some(edge), true) => Some(edge.min(THUMBNAIL_EDGE)),
            (Some(edge), false) => Some(edge),
            (None, true) => Some(THUMBNAIL_EDGE),
            (None, false) => None,
        }
    }
}

/// How many pages to render at once: as many as there are threads for, but
/// no more than fit the memory budget at this size.
pub(crate) fn render_workers(
    snapshot: &DocumentSnapshot,
    pages: &[PageZero],
    spec: &RenderSpec,
    threads: u32,
) -> u32 {
    let workers = crate::parallel::worker_count(threads, pages.len()) as u64;
    let Some(first) = pages.first() else {
        return 1;
    };
    let Ok(page) = snapshot.page(PageIndex(first.0)) else {
        return workers as u32;
    };
    let [x0, y0, x1, y1] = page.crop_box;
    let (scale, chosen) = spec.base_scale().unwrap_or((150.0 / 72.0, false));
    let (mut width, mut height) = ((x1 - x0).abs() * scale, (y1 - y0).abs() * scale);
    if let Some(edge) = spec.edge_limit() {
        let longest = width.max(height).max(1.0);
        if longest > f64::from(edge) || (!chosen && !spec.thumbnail) {
            let factor = f64::from(edge) / longest;
            width *= factor;
            height *= factor;
        }
    }
    // The surface, the copy handed to the encoder, and the encoder's own
    // working space.
    let per_page = ((width * height) as u64).saturating_mul(4 * 3).max(1);
    workers.min((RENDER_MEMORY_BUDGET / per_page).max(1)) as u32
}

pub(crate) fn expand_template(
    template: &str,
    page_one: u32,
    page_index: u32,
    stem: &str,
) -> String {
    template
        .replace("{page}", &page_one.to_string())
        .replace("{page_index}", &page_index.to_string())
        .replace("{stem}", stem)
}

/// Write encoded image bytes to `path`, or to stdout for `-`.
pub(crate) fn write_image(path: &str, bytes: &[u8]) -> Result<()> {
    if path == STDOUT_TEMPLATE {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(bytes).context("writing image to stdout")?;
        return stdout.flush().context("flushing stdout");
    }
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// A rasterized page before encoding.
pub(crate) struct Raster {
    pub host: HostPage,
    pub dpi: f64,
    pub scale: f64,
    pub region: Option<[f64; 4]>,
    pub degraded_draws: u32,
    pub recovery_notes: Vec<String>,
}

impl Raster {
    fn view(&self, path: String, image: &EncodedImage) -> RenderPageData {
        RenderPageData {
            unit: "device_pixels",
            path,
            width: self.host.width,
            height: self.host.height,
            dpi: crate::views::round2(self.dpi),
            scale: (self.scale * 1e4).round() / 1e4,
            format: image.format.name().to_owned(),
            color: image.color,
            bytes: image.bytes.len() as u64,
            degraded_draws: self.degraded_draws,
            recovery_notes: self.recovery_notes.clone(),
            region: self.region,
        }
    }
}

/// Rasterize one page, or the cropped part of it, at the size `spec` asks for.
pub(crate) fn rasterize(
    snapshot: &DocumentSnapshot,
    compiler: &pdf_content::PageCompiler,
    backend: &CpuBackend,
    pz: PageZero,
    spec: &RenderSpec,
    warnings: &mut Vec<String>,
) -> Result<Raster> {
    let mut ctx = parse_context();
    let page = Arc::new(compiler.compile(snapshot, PageIndex(pz.0), &mut ctx)?);

    let crop_rect = page.bounds.crop;
    let rotate = page.bounds.rotate;
    let (mut scale, chosen) = spec.base_scale()?;
    let mut frame = output_frame(rotate, crop_rect, scale, spec.crop)?;

    if let Some(limit) = spec.edge_limit() {
        let limit = limit.max(1);
        // With no size chosen, the limit is the size: fill it. Otherwise it
        // is a ceiling only.
        let fill = !chosen && !spec.thumbnail;
        for _ in 0..4 {
            let edge = frame.width.max(frame.height).max(1);
            if edge == limit || (edge < limit && !fill) {
                break;
            }
            let wanted = scale * (f64::from(limit) - 0.5) / f64::from(edge);
            let next = if edge < limit {
                wanted.min(MAX_FIT_SCALE)
            } else {
                wanted
            };
            if (next - scale).abs() < 1e-9 {
                break;
            }
            scale = next;
            frame = output_frame(rotate, crop_rect, scale, spec.crop)?;
        }
    }
    let OutputFrame {
        matrix: device,
        width,
        height,
        region,
    } = frame;

    // The CPU backend does not read `RenderRequest::crop`; a region is
    // rendered by moving the page under a smaller output surface instead.
    let request = RenderRequest {
        page,
        transform: PageTransform { matrix: device },
        crop: None,
        output_size: DeviceSize { width, height },
        output_format: if spec.gray {
            OutputFormat::Gray8
        } else {
            OutputFormat::Rgba8PremultipliedSrgb
        },
        background: Background::White,
        annotations: if default_annotations() {
            AnnotationMode::StaticAppearances
        } else {
            AnnotationMode::None
        },
        color_policy: pdf_render_api::RenderColorPolicy::Original,
        quality: if spec.thumbnail {
            RenderQuality::Draft
        } else {
            RenderQuality::Normal
        },
        limits: render_limits(),
        residency: OutputResidency::HostRequired,
    };

    let (host, stats) = backend.render_to_host(&request)?;
    if stats.degraded_draws > 0 {
        warnings.push(format!("{} image draw(s) degraded", stats.degraded_draws));
        for note in &stats.recovery_notes {
            warnings.push(note.clone());
        }
    }
    Ok(Raster {
        host,
        dpi: scale * 72.0,
        scale,
        region,
        degraded_draws: stats.degraded_draws,
        recovery_notes: stats.recovery_notes.clone(),
    })
}

/// Where the page lands on the output surface: the PDF→pixel matrix, the
/// surface size, and — for a cropped render — the PDF-space rectangle the
/// surface actually covers.
struct OutputFrame {
    matrix: Matrix,
    width: u32,
    height: u32,
    region: Option<[f64; 4]>,
}

/// Lay the whole page, or the part of it inside `crop` (PDF points), onto an
/// output surface at `scale` pixels per point.
///
/// The crop is clamped to the page and snapped outward to whole pixels, so
/// the reported region can be slightly larger than the one asked for; a crop
/// that misses the page is an error rather than a blank image.
fn output_frame(
    rotate: u16,
    page_box: pdf_page_ir::Rect,
    scale: f64,
    crop: Option<[f64; 4]>,
) -> Result<OutputFrame> {
    let (page_width, page_height) = match rotate {
        90 | 270 => (page_box.height() * scale, page_box.width() * scale),
        _ => (page_box.width() * scale, page_box.height() * scale),
    };
    let page_width = page_width.ceil().max(1.0);
    let page_height = page_height.ceil().max(1.0);
    let mut matrix = display_matrix(rotate, page_box, scale);
    let Some(crop) = crop else {
        return Ok(OutputFrame {
            matrix,
            width: page_width as u32,
            height: page_height as u32,
            region: None,
        });
    };

    let device = matrix.transform_rect(pdf_page_ir::Rect {
        x0: crop[0].min(crop[2]),
        y0: crop[1].min(crop[3]),
        x1: crop[0].max(crop[2]),
        y1: crop[1].max(crop[3]),
    });
    let x0 = device.x0.min(device.x1).floor().max(0.0);
    let y0 = device.y0.min(device.y1).floor().max(0.0);
    let x1 = device.x0.max(device.x1).ceil().min(page_width);
    let y1 = device.y0.max(device.y1).ceil().min(page_height);
    if x1 <= x0 || y1 <= y0 {
        bail!(
            "crop {crop:?} does not intersect the page box [{}, {}, {}, {}]",
            page_box.x0,
            page_box.y0,
            page_box.x1,
            page_box.y1
        );
    }

    let inverse = matrix
        .invert()
        .context("page transform is not invertible")?;
    let covered = inverse.transform_rect(pdf_page_ir::Rect { x0, y0, x1, y1 });
    matrix.e -= x0;
    matrix.f -= y0;
    Ok(OutputFrame {
        matrix,
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
        region: Some([
            covered.x0.min(covered.x1),
            covered.y0.min(covered.y1),
            covered.x0.max(covered.x1),
            covered.y0.max(covered.y1),
        ]),
    })
}

/// The PDF-space rectangle under a rectangle of pixels `[left, top, right,
/// bottom]` of the page as displayed at `scale` pixels per point.
pub(crate) fn pixel_region(
    rotate: u16,
    page_box: [f64; 4],
    scale: f64,
    pixels: [f64; 4],
) -> Option<[f64; 4]> {
    let page_box = pdf_page_ir::Rect {
        x0: page_box[0].min(page_box[2]),
        y0: page_box[1].min(page_box[3]),
        x1: page_box[0].max(page_box[2]),
        y1: page_box[1].max(page_box[3]),
    };
    let inverse = display_matrix(rotate, page_box, scale).invert()?;
    let region = inverse.transform_rect(pdf_page_ir::Rect {
        x0: pixels[0],
        y0: pixels[1],
        x1: pixels[2],
        y1: pixels[3],
    });
    Some([
        region.x0.min(region.x1),
        region.y0.min(region.y1),
        region.x0.max(region.x1),
        region.y0.max(region.y1),
    ])
}

/// Rasterize one page and encode it as `spec` asks: a render that has not
/// been given a destination yet.
pub(crate) fn render_encoded(
    snapshot: &DocumentSnapshot,
    compiler: &pdf_content::PageCompiler,
    backend: &CpuBackend,
    pz: PageZero,
    spec: &RenderSpec,
    warnings: &mut Vec<String>,
) -> Result<EncodedImage> {
    let raster = rasterize(snapshot, compiler, backend, pz, spec, warnings)?;
    spec.encode(pack(&raster.host), raster.host.width, raster.host.height)
}

pub(crate) fn display_matrix(rotate: u16, crop: pdf_page_ir::Rect, s: f64) -> Matrix {
    match rotate {
        90 => Matrix {
            a: 0.0,
            b: s,
            c: s,
            d: 0.0,
            e: -crop.y0 * s,
            f: -crop.x0 * s,
        },
        180 => Matrix {
            a: -s,
            b: 0.0,
            c: 0.0,
            d: s,
            e: crop.x1 * s,
            f: -crop.y0 * s,
        },
        270 => Matrix {
            a: 0.0,
            b: -s,
            c: -s,
            d: 0.0,
            e: crop.y1 * s,
            f: crop.x1 * s,
        },
        _ => Matrix {
            a: s,
            b: 0.0,
            c: 0.0,
            d: -s,
            e: -crop.x0 * s,
            f: crop.y1 * s,
        },
    }
}

/// Render a page into a tightly packed grayscale buffer for the in-process OCR
/// backend. The returned matrix maps PDF points to top-left-origin pixels.
///
/// Takes the `CpuBackend` by reference so callers doing this once per page
/// (e.g. `lege-pdf text --ocr always --pages all`) can share one backend —
/// and its font/glyph/image/soft-mask/prepared-page caches — across the
/// whole run instead of paying `CpuBackend::new` on every page.
pub(crate) fn render_gray_for_ocr(
    snapshot: &DocumentSnapshot,
    compiler: &pdf_content::PageCompiler,
    pz: PageZero,
    dpi: f64,
    max_pixels: u64,
    backend: &CpuBackend,
) -> Result<(image::GrayImage, Matrix)> {
    let mut ctx = parse_context();
    let page = Arc::new(compiler.compile(snapshot, PageIndex(pz.0), &mut ctx)?);
    let crop = page.bounds.crop;
    let mut scale = dpi / 72.0;
    let dimensions = |scale: f64| match page.bounds.rotate {
        90 | 270 => (
            (crop.height() * scale).ceil().max(1.0) as u32,
            (crop.width() * scale).ceil().max(1.0) as u32,
        ),
        _ => (
            (crop.width() * scale).ceil().max(1.0) as u32,
            (crop.height() * scale).ceil().max(1.0) as u32,
        ),
    };
    let (mut width, mut height) = dimensions(scale);
    let pixels = u64::from(width) * u64::from(height);
    if max_pixels > 0 && pixels > max_pixels {
        scale *= (max_pixels as f64 / pixels as f64).sqrt();
        (width, height) = dimensions(scale);
    }
    let device = display_matrix(page.bounds.rotate, crop, scale);
    let request = RenderRequest {
        page,
        transform: PageTransform { matrix: device },
        crop: None,
        output_size: DeviceSize { width, height },
        output_format: OutputFormat::Gray8,
        background: Background::White,
        annotations: AnnotationMode::StaticAppearances,
        color_policy: pdf_render_api::RenderColorPolicy::Original,
        quality: RenderQuality::Normal,
        limits: render_limits(),
        residency: OutputResidency::HostRequired,
    };
    let (host, _stats) = backend.render_to_host(&request)?;
    let mut pixels = Vec::with_capacity(width as usize * height as usize);
    for row in host.pixels.chunks(host.stride).take(height as usize) {
        pixels.extend_from_slice(&row[..width as usize]);
    }
    let image = image::GrayImage::from_raw(width, height, pixels)
        .context("constructing tightly packed OCR page")?;
    Ok((image, device))
}

/// An image ready to be written or returned.
#[derive(Debug, Clone)]
pub struct EncodedImage {
    pub bytes: Vec<u8>,
    pub format: ImageFormat,
    /// `gray`, `rgb` or `rgba`: what the file actually stores.
    pub color: &'static str,
}

/// Tightly packed pixels in the cheapest layout that loses nothing.
struct Packed {
    pixels: Vec<u8>,
    channels: u8,
}

/// Repack a rendered surface, dropping what carries no information.
///
/// A page is rendered onto an opaque white background, so its alpha channel
/// is constant, and most pages of text are black on white, so three equal
/// colour channels say what one would. Writing RGBA for those made every
/// PNG larger and slower to compress than it had to be.
fn pack(page: &HostPage) -> Packed {
    let (width, height) = (page.width as usize, page.height as usize);
    let stride = page.stride.max(1);
    match page.format {
        OutputFormat::Gray8 => {
            let mut pixels = Vec::with_capacity(width * height);
            for row in page.pixels.chunks(stride).take(height) {
                pixels.extend_from_slice(&row[..width]);
            }
            Packed {
                pixels,
                channels: 1,
            }
        }
        OutputFormat::Rgba8PremultipliedSrgb => {
            let (mut opaque, mut gray) = (true, true);
            for row in page.pixels.chunks(stride).take(height) {
                for px in row[..width * 4].chunks_exact(4) {
                    opaque &= px[3] == 255;
                    gray &= px[0] == px[1] && px[1] == px[2];
                }
                if !opaque {
                    break;
                }
            }
            let channels: u8 = match (opaque, gray) {
                (false, _) => 4,
                (true, true) => 1,
                (true, false) => 3,
            };
            let mut pixels = Vec::with_capacity(width * height * usize::from(channels));
            for row in page.pixels.chunks(stride).take(height) {
                let row = &row[..width * 4];
                match channels {
                    4 => pixels.extend_from_slice(row),
                    3 => {
                        for px in row.chunks_exact(4) {
                            pixels.extend_from_slice(&px[..3]);
                        }
                    }
                    _ => pixels.extend(row.chunks_exact(4).map(|px| px[0])),
                }
            }
            Packed { pixels, channels }
        }
    }
}

/// Encode a rendered surface as `format`.
pub(crate) fn encode(page: &HostPage, format: ImageFormat, quality: u8) -> Result<EncodedImage> {
    encode_packed(pack(page), page.width, page.height, format, quality)
}

fn encode_packed(
    packed: Packed,
    width: u32,
    height: u32,
    format: ImageFormat,
    quality: u8,
) -> Result<EncodedImage> {
    let mut bytes = Vec::new();
    let color = match format {
        ImageFormat::Png => {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            let (color, name) = match packed.channels {
                1 => (png::ColorType::Grayscale, "gray"),
                3 => (png::ColorType::Rgb, "rgb"),
                // Premultiplied; only reached when a surface is not opaque.
                _ => (png::ColorType::Rgba, "rgba"),
            };
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().context("writing PNG header")?;
            writer
                .write_image_data(&packed.pixels)
                .context("writing PNG pixels")?;
            writer.finish().context("finishing PNG")?;
            name
        }
        ImageFormat::Ppm => {
            write!(bytes, "P6\n{width} {height}\n255\n")?;
            match packed.channels {
                1 => bytes.extend(packed.pixels.iter().flat_map(|&g| [g, g, g])),
                3 => bytes.extend_from_slice(&packed.pixels),
                _ => bytes.extend(
                    packed
                        .pixels
                        .chunks_exact(4)
                        .flat_map(|px| [px[0], px[1], px[2]]),
                ),
            }
            "rgb"
        }
        ImageFormat::Pgm => {
            write!(
                bytes,
                "P5
{width} {height}
255
"
            )?;
            let luma = |px: &[u8]| {
                ((u32::from(px[0]) * 299 + u32::from(px[1]) * 587 + u32::from(px[2]) * 114) / 1000)
                    as u8
            };
            match packed.channels {
                1 => bytes.extend_from_slice(&packed.pixels),
                3 => bytes.extend(packed.pixels.chunks_exact(3).map(luma)),
                _ => bytes.extend(packed.pixels.chunks_exact(4).map(luma)),
            }
            "gray"
        }
        ImageFormat::Jpeg => {
            let (Ok(jpeg_width), Ok(jpeg_height)) = (u16::try_from(width), u16::try_from(height))
            else {
                bail!(
                    "a {width}x{height} image is too large for JPEG (65535 pixels per side); use png or a lower dpi"
                );
            };
            let (pixels, color, name) = match packed.channels {
                1 => (packed.pixels, jpeg_encoder::ColorType::Luma, "gray"),
                3 => (packed.pixels, jpeg_encoder::ColorType::Rgb, "rgb"),
                _ => (
                    packed
                        .pixels
                        .chunks_exact(4)
                        .flat_map(|px| [px[0], px[1], px[2]])
                        .collect(),
                    jpeg_encoder::ColorType::Rgb,
                    "rgb",
                ),
            };
            jpeg_encoder::Encoder::new(&mut bytes, quality)
                .encode(&pixels, jpeg_width, jpeg_height, color)
                .map_err(|err| anyhow::anyhow!("encoding JPEG: {err}"))?;
            name
        }
    };
    Ok(EncodedImage {
        bytes,
        format,
        color,
    })
}

/// Several pages tiled into one image, each under its page number.
#[derive(Debug)]
pub struct ContactSheet {
    pub image: EncodedImage,
    pub width: u32,
    pub height: u32,
    pub columns: u32,
    pub rows: u32,
    pub cells: Vec<crate::views::render::SheetCell>,
}

impl ContactSheet {
    pub fn view(&self, path: String) -> crate::views::render::ContactSheetData {
        crate::views::render::ContactSheetData {
            unit: "device_pixels",
            path,
            width: self.width,
            height: self.height,
            format: self.image.format.name().to_owned(),
            color: self.image.color,
            bytes: self.image.bytes.len() as u64,
            columns: self.columns,
            rows: self.rows,
            pages: self.cells.clone(),
        }
    }
}

/// Longest edge of a contact sheet when none is asked for: what a vision
/// model takes without downscaling.
const SHEET_EDGE: u32 = 1568;
/// Space between cells and around the sheet, in pixels.
const SHEET_GAP: u32 = 10;
/// Height of the page-number band above each cell.
const SHEET_LABEL: u32 = 18;
/// Most pages one sheet may hold; beyond it the cells are unreadable.
const SHEET_MAX_PAGES: usize = 64;

/// Tile `pages` into one image: a way to look at a dozen pages for the
/// price of one.
///
/// `spec.max_edge` is the longest edge of the whole sheet; `columns == 0`
/// picks a near-square grid. Each page is drawn under its one-based number.
/// A page that fails to render leaves its cell empty and a warning behind.
pub(crate) fn contact_sheet(
    snapshot: &DocumentSnapshot,
    pages: &[PageZero],
    spec: &RenderSpec,
    columns: u32,
    system_fonts: bool,
    threads: u32,
    warnings: &mut Vec<String>,
) -> Result<ContactSheet> {
    if pages.is_empty() {
        bail!("no pages selected");
    }
    let pages = if pages.len() > SHEET_MAX_PAGES {
        warnings.push(format!(
            "contact sheet limited to the first {SHEET_MAX_PAGES} of {} selected pages; pages from {} on are not shown",
            pages.len(),
            pages[SHEET_MAX_PAGES].one_based()
        ));
        &pages[..SHEET_MAX_PAGES]
    } else {
        pages
    };
    let count = pages.len() as u32;
    let columns = match columns {
        0 => (f64::from(count).sqrt().ceil() as u32).max(1),
        columns => columns.min(count),
    };
    let rows = count.div_ceil(columns);
    let sheet_edge = spec.max_edge.unwrap_or(SHEET_EDGE).max(64);
    let across = columns.max(rows);
    let cell_edge = (sheet_edge.saturating_sub(SHEET_GAP * (across + 1)) / across)
        .saturating_sub(SHEET_LABEL)
        .max(16);

    let cell_spec = RenderSpec {
        dpi: None,
        scale: None,
        max_edge: Some(cell_edge),
        crop: None,
        thumbnail: false,
        ..*spec
    };
    let backend = CpuBackend::new(CpuBackendOptions::default());
    let workers = crate::parallel::worker_count(threads, pages.len());
    let mut rendered: Vec<Option<Packed3>> = Vec::with_capacity(pages.len());
    crate::parallel::ordered(
        pages,
        workers,
        || page_compiler(system_fonts, default_annotations()),
        |compiler, _, pz| -> Result<Packed3> {
            if crate::deadline::expired() {
                bail!("{}", crate::deadline::message(0, pages.len()));
            }
            let mut ignored = Vec::new();
            let raster = rasterize(snapshot, compiler, &backend, *pz, &cell_spec, &mut ignored)?;
            Ok(Packed3::from_host(&raster.host))
        },
        |_, pz, result| {
            match result {
                Ok(cell) => rendered.push(Some(cell)),
                Err(err) => {
                    warnings.push(format!("page {} not drawn: {err:#}", pz.one_based()));
                    rendered.push(None);
                }
            }
            ControlFlow::Continue(())
        },
    );
    if crate::deadline::expired() {
        bail!("{}", crate::deadline::message(0, pages.len()));
    }

    let cell_width = rendered
        .iter()
        .flatten()
        .map(|c| c.width)
        .max()
        .unwrap_or(1);
    let cell_height = rendered
        .iter()
        .flatten()
        .map(|c| c.height)
        .max()
        .unwrap_or(1);
    let width = SHEET_GAP + columns * (cell_width + SHEET_GAP);
    let height = SHEET_GAP + rows * (cell_height + SHEET_LABEL + SHEET_GAP);
    let mut canvas = Packed3 {
        width,
        height,
        pixels: vec![255u8; width as usize * height as usize * 3],
    };

    let mut cells = Vec::with_capacity(pages.len());
    for (index, (pz, cell)) in pages.iter().zip(&rendered).enumerate() {
        let (column, row) = (index as u32 % columns, index as u32 / columns);
        let left = SHEET_GAP + column * (cell_width + SHEET_GAP);
        let top = SHEET_GAP + row * (cell_height + SHEET_LABEL + SHEET_GAP);
        canvas.draw_number(left, top + 2, pz.one_based());
        let Some(cell) = cell else {
            continue;
        };
        let (x, y) = (left, top + SHEET_LABEL);
        canvas.blit(cell, x, y);
        canvas.outline(x, y, cell.width, cell.height);
        cells.push(crate::views::render::SheetCell {
            page: pz.one_based(),
            x,
            y,
            width: cell.width,
            height: cell.height,
        });
    }

    let gray = canvas
        .pixels
        .chunks_exact(3)
        .all(|px| px[0] == px[1] && px[1] == px[2]);
    let packed = if gray {
        Packed {
            pixels: canvas.pixels.chunks_exact(3).map(|px| px[0]).collect(),
            channels: 1,
        }
    } else {
        Packed {
            pixels: canvas.pixels,
            channels: 3,
        }
    };
    let image = spec.encode(packed, width, height)?;
    Ok(ContactSheet {
        image,
        width,
        height,
        columns,
        rows,
        cells,
    })
}

/// An RGB canvas for composing a contact sheet.
struct Packed3 {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// 5x7 digits, one row per byte, most significant of the low five bits left.
const DIGITS: [[u8; 7]; 10] = [
    [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
    [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
    [0x1e, 0x01, 0x01, 0x0e, 0x01, 0x01, 0x1e],
    [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
    [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
    [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
    [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
    [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
    [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
];

impl Packed3 {
    fn from_host(page: &HostPage) -> Self {
        let packed = pack(page);
        let pixels = match packed.channels {
            1 => packed.pixels.iter().flat_map(|&g| [g, g, g]).collect(),
            3 => packed.pixels,
            _ => packed
                .pixels
                .chunks_exact(4)
                .flat_map(|px| [px[0], px[1], px[2]])
                .collect(),
        };
        Self {
            width: page.width,
            height: page.height,
            pixels,
        }
    }

    fn put(&mut self, x: u32, y: u32, rgb: [u8; 3]) {
        if x < self.width && y < self.height {
            let at = (y as usize * self.width as usize + x as usize) * 3;
            self.pixels[at..at + 3].copy_from_slice(&rgb);
        }
    }

    fn blit(&mut self, source: &Packed3, left: u32, top: u32) {
        let span = source.width.min(self.width.saturating_sub(left)) as usize * 3;
        for row in 0..source.height.min(self.height.saturating_sub(top)) as usize {
            let from = row * source.width as usize * 3;
            let to = ((top as usize + row) * self.width as usize + left as usize) * 3;
            self.pixels[to..to + span].copy_from_slice(&source.pixels[from..from + span]);
        }
    }

    /// A one-pixel frame, so a white page does not dissolve into the sheet.
    fn outline(&mut self, left: u32, top: u32, width: u32, height: u32) {
        const EDGE: [u8; 3] = [150, 150, 150];
        for x in left..left + width {
            self.put(x, top, EDGE);
            self.put(x, top + height.saturating_sub(1), EDGE);
        }
        for y in top..top + height {
            self.put(left, y, EDGE);
            self.put(left + width.saturating_sub(1), y, EDGE);
        }
    }

    /// Draw `number` in 10x14 digits with its top-left corner at (`left`, `top`).
    fn draw_number(&mut self, left: u32, top: u32, number: u32) {
        const INK: [u8; 3] = [40, 40, 40];
        for (place, digit) in number.to_string().bytes().enumerate() {
            let glyph = DIGITS[usize::from(digit - b'0')];
            let origin = left + place as u32 * 12;
            for (row, bits) in glyph.iter().enumerate() {
                for column in 0..5u32 {
                    if bits & (0x10 >> column) != 0 {
                        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            self.put(origin + column * 2 + dx, top + row as u32 * 2 + dy, INK);
                        }
                    }
                }
            }
        }
    }
}

fn emit(env: &Envelope, data: &RenderPageData, mode: OutputMode, multi: bool) -> Result<()> {
    match mode {
        OutputMode::Human => {
            let line = format!(
                "wrote {} ({}x{} dpi={:.1})",
                data.path, data.width, data.height, data.dpi
            );
            // With `--output -` stdout is the image itself.
            if data.path == STDOUT_TEMPLATE {
                eprintln!("{line}");
            } else {
                println!("{line}");
            }
            for w in &env.warnings {
                eprintln!("warning: {w}");
            }
            Ok(())
        }
        OutputMode::Json if !multi => env.write_json(),
        OutputMode::Json | OutputMode::Jsonl => env.write_jsonl(),
    }
}
