//! lege-render-probe: measurement harness for the Lege PDF renderer.
//!
//! probe run <pdf> [--scale S] [--pages SPEC] [--repeat K] [--measure-from N]
//!           [--system-fonts] [--render-only] [--fresh-backend] [--hash]
//!           [--ppm DIR] [--quiet] [--profile-out FILE]
//!     Single-threaded sequential compile+render. One TSV row per render.
//!     Renders with sequence index < N go through `probe_warmup_render`,
//!     the rest through `probe_measured_render` (callgrind --toggle-collect).
//!
//! probe doc <pdf> [--scale S] [--compile-workers C] [--render-workers R]
//!           [--runs N] [--system-fonts]
//!     Whole document through RenderScheduler::render_range.
//!
//! probe fontscan
//!     Cold cost of FolderFontProvider::system() + first lookup.
//!
//! probe seq <pdf> (--scales S1,S2,... | --sizes W1xH1,W2xH2,...) [--page P]
//!           [--fresh-compare] [--ppm DIR] [--system-fonts]
//!     (P1 probe addition) Compile page P once, then render it at each step of
//!     the sequence through ONE CpuBackend (one SharedImageCache), printing a
//!     hash per step. `--sizes` builds the matrix exactly like lege-pdf-read's
//!     `page_to_device_matrix` (independent x/y scale to fill WxH); `--scales`
//!     uses the harness's uniform `request_for_page`. With --fresh-compare,
//!     each step is re-rendered on a brand-new backend and compared.
//!
//! probe doc ... [--hash] [--ppm DIR] [--pages-end N]
//!     (P1 probe addition) per-page hash / PPM of the scheduler output.

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pdf_content::PageCompiler;
use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex, ParseContext};
use pdf_page_ir::{CompiledPage, DeviceSize, Matrix};
use pdf_render_api::{
    AnnotationMode, Background, HostPage, OutputFormat, OutputResidency, PageTransform,
    RenderBackend, RenderError, RenderLimits, RenderQuality, RenderRequest, RenderedPage,
};
use pdf_render_cpu::{CpuBackend, CpuWorkerContext, RenderStats};
use pdf_render_scheduler::{PipelineOutput, RenderScheduler, SchedulerOptions};
use pdf_source::{MmapSource, PdfSource};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("doc") => doc(&args[1..]),
        Some("fontscan") => fontscan(),
        Some("seq") => seq(&args[1..]),
        _ => {
            eprintln!("usage: probe run|doc|fontscan|seq ... (see source header)");
            std::process::exit(2);
        }
    }
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

struct Args(Vec<String>);

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        if let Some(i) = self.0.iter().position(|a| a == name) {
            self.0.remove(i);
            true
        } else {
            false
        }
    }
    fn value(&mut self, name: &str) -> Option<String> {
        let i = self.0.iter().position(|a| a == name)?;
        if i + 1 >= self.0.len() {
            eprintln!("{name} needs a value");
            std::process::exit(2);
        }
        let v = self.0.remove(i + 1);
        self.0.remove(i);
        Some(v)
    }
    fn positional(&self) -> &str {
        match self.0.first() {
            Some(p) => p,
            None => {
                eprintln!("missing <pdf>");
                std::process::exit(2);
            }
        }
    }
}

fn parse_pages(spec: &str, count: u32) -> Vec<u32> {
    if spec == "all" {
        return (0..count).collect();
    }
    let mut out = Vec::new();
    for part in spec.split(',') {
        if let Some((a, b)) = part.split_once('-') {
            let a: u32 = a.parse().expect("page");
            let b: u32 = b.parse().expect("page");
            out.extend(a..=b.min(count.saturating_sub(1)));
        } else {
            out.push(part.parse().expect("page"));
        }
    }
    out
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn proc_status_kib(key: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find_map(|l| l.strip_prefix(key))?;
    line.split_whitespace().next()?.parse().ok()
}

fn open_snapshot(path: &str) -> (DocumentSnapshot, Duration) {
    let t = Instant::now();
    let source: Arc<dyn PdfSource> = Arc::new(MmapSource::open(path).expect("map input"));
    let snap = DocumentSnapshot::open(source, DocumentLimits::default()).expect("open document");
    (snap, t.elapsed())
}

fn compiler(system_fonts: Option<Arc<dyn pdf_font::SystemFontProvider>>) -> PageCompiler {
    let c = PageCompiler::new().with_annotations(true);
    match system_fonts {
        Some(p) => c.with_system_fonts(p),
        None => c,
    }
}

/// Same request shape as lege-render-demo / pdfium-diff (RGBA premultiplied,
/// white background, static annotation appearances).
fn request_for_page(page: Arc<CompiledPage>, scale: f64) -> RenderRequest {
    let crop = page.bounds.crop;
    let (width, height) = match page.bounds.rotate {
        90 | 270 => (
            (crop.height() * scale).ceil() as u32,
            (crop.width() * scale).ceil() as u32,
        ),
        _ => (
            (crop.width() * scale).ceil() as u32,
            (crop.height() * scale).ceil() as u32,
        ),
    };
    let matrix = match page.bounds.rotate {
        90 => Matrix { a: 0.0, b: scale, c: scale, d: 0.0, e: -crop.y0 * scale, f: -crop.x0 * scale },
        180 => Matrix { a: -scale, b: 0.0, c: 0.0, d: scale, e: crop.x1 * scale, f: -crop.y0 * scale },
        270 => Matrix { a: 0.0, b: -scale, c: -scale, d: 0.0, e: crop.y1 * scale, f: crop.x1 * scale },
        _ => Matrix { a: scale, b: 0.0, c: 0.0, d: -scale, e: -crop.x0 * scale, f: crop.y1 * scale },
    };
    RenderRequest {
        page,
        transform: PageTransform { matrix },
        crop: None,
        output_size: DeviceSize { width, height },
        // PROBE_GRAY8=1 renders Gray8 (the lege base render on gray pages).
        output_format: if std::env::var_os("PROBE_GRAY8").is_some() {
            OutputFormat::Gray8
        } else {
            OutputFormat::Rgba8PremultipliedSrgb
        },
        background: Background::White,
        annotations: AnnotationMode::StaticAppearances,
        color_policy: pdf_render_api::RenderColorPolicy::Original,
        quality: RenderQuality::Normal,
        limits: RenderLimits::default(),
        residency: OutputResidency::HostRequired,
    }
}

// ---------------------------------------------------------------------------
// run: single-threaded sequential
// ---------------------------------------------------------------------------

struct One {
    compile: Duration,
    stats: RenderStats,
    render: Duration,
    host: HostPage,
    #[cfg(feature = "profiling")]
    profile: pdf_profiling::ProfileReport,
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn do_one(
    snap: &DocumentSnapshot,
    comp: &PageCompiler,
    backend: &CpuBackend,
    worker: &mut CpuWorkerContext,
    page: u32,
    scale: f64,
    cached: Option<Arc<CompiledPage>>,
) -> (One, Arc<CompiledPage>) {
    let t0 = Instant::now();
    #[cfg(feature = "profiling")]
    let mut profile = pdf_profiling::ProfileReport::new();
    let compiled = match cached {
        Some(c) => c,
        None => {
            let mut ctx = ParseContext::new();
            #[cfg(feature = "profiling")]
            {
                let (c, p) = comp
                    .compile_profiled(snap, PageIndex(page), &mut ctx)
                    .expect("compile page");
                profile.merge(&p);
                Arc::new(c)
            }
            #[cfg(not(feature = "profiling"))]
            {
                Arc::new(comp.compile(snap, PageIndex(page), &mut ctx).expect("compile page"))
            }
        }
    };
    let compile = t0.elapsed();
    let request = request_for_page(Arc::clone(&compiled), scale);
    let t1 = Instant::now();
    #[cfg(feature = "profiling")]
    let (host, stats) = {
        let (host, stats, p) = backend.render_profiled_with(&request, worker).expect("render page");
        profile.merge(&p);
        (host, stats)
    };
    #[cfg(not(feature = "profiling"))]
    let (host, stats) = backend.render_with(&request, worker).expect("render page");
    let render = t1.elapsed();
    (
        One {
            compile,
            stats,
            render,
            host,
            #[cfg(feature = "profiling")]
            profile,
        },
        compiled,
    )
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn probe_warmup_render(
    snap: &DocumentSnapshot,
    comp: &PageCompiler,
    backend: &CpuBackend,
    worker: &mut CpuWorkerContext,
    page: u32,
    scale: f64,
    cached: Option<Arc<CompiledPage>>,
) -> (One, Arc<CompiledPage>) {
    std::hint::black_box(0x5741_524du32);
    std::hint::black_box(do_one(snap, comp, backend, worker, page, scale, cached))
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn probe_measured_render(
    snap: &DocumentSnapshot,
    comp: &PageCompiler,
    backend: &CpuBackend,
    worker: &mut CpuWorkerContext,
    page: u32,
    scale: f64,
    cached: Option<Arc<CompiledPage>>,
) -> (One, Arc<CompiledPage>) {
    std::hint::black_box(0x4d45_4153u32);
    let r = std::hint::black_box(do_one(snap, comp, backend, worker, page, scale, cached));
    std::hint::black_box(0x4d45_4154u32);
    r
}

fn hash_host(host: &HostPage) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    host.width.hash(&mut h);
    host.height.hash(&mut h);
    host.pixels.hash(&mut h);
    h.finish()
}

fn write_ppm(path: &std::path::Path, page: &HostPage) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).expect("create ppm"));
    write!(f, "P6\n{} {}\n255\n", page.width, page.height).unwrap();
    let bpp = page.format.bytes_per_pixel();
    let mut row_buf = Vec::with_capacity(page.width as usize * 3);
    for y in 0..page.height as usize {
        let row = &page.pixels[y * page.stride..y * page.stride + page.width as usize * bpp];
        row_buf.clear();
        match page.format {
            OutputFormat::Rgba8PremultipliedSrgb => {
                for px in row.chunks_exact(4) {
                    row_buf.extend_from_slice(&px[..3]);
                }
            }
            OutputFormat::Gray8 => {
                for &g in row {
                    row_buf.extend_from_slice(&[g, g, g]);
                }
            }
        }
        f.write_all(&row_buf).unwrap();
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { 0.5 * (v[n / 2 - 1] + v[n / 2]) }
}

fn run(args: &[String]) {
    let mut a = Args(args.to_vec());
    let scale: f64 = a.value("--scale").map(|v| v.parse().expect("scale")).unwrap_or(1.0);
    let pages_spec = a.value("--pages").unwrap_or_else(|| "all".into());
    let repeat: usize = a.value("--repeat").map(|v| v.parse().expect("repeat")).unwrap_or(1);
    let measure_from: usize =
        a.value("--measure-from").map(|v| v.parse().expect("measure-from")).unwrap_or(0);
    let system_fonts = a.flag("--system-fonts");
    let render_only = a.flag("--render-only");
    let fresh_backend = a.flag("--fresh-backend");
    let do_hash = a.flag("--hash");
    let quiet = a.flag("--quiet");
    let ppm_dir = a.value("--ppm");
    let profile_out = a.value("--profile-out");
    let path = a.positional().to_owned();

    let t_proc = Instant::now();
    let (snap, open) = open_snapshot(&path);
    let count = snap.page_count();
    let pages = parse_pages(&pages_spec, count);
    let provider: Option<Arc<dyn pdf_font::SystemFontProvider>> = if system_fonts {
        Some(Arc::new(pdf_font::FolderFontProvider::system()))
    } else {
        None
    };
    let comp = compiler(provider);
    let mut backend = CpuBackend::default();
    let mut worker = CpuWorkerContext::new();
    let mut compiled_cache: std::collections::HashMap<u32, Arc<CompiledPage>> =
        std::collections::HashMap::new();
    #[cfg(feature = "profiling")]
    let mut prof_writer = profile_out
        .as_ref()
        .map(|p| std::io::BufWriter::new(std::fs::File::create(p).expect("profile out")));
    #[cfg(not(feature = "profiling"))]
    if profile_out.is_some() {
        eprintln!("--profile-out needs --features profiling");
        std::process::exit(2);
    }

    if !quiet {
        println!(
            "#pass\tpage\tcompile_ms\tlower_ms\tsurface_ms\texecute_ms\toutput_ms\trender_ms\ttotal_ms\tw\th\tops\tcmds\tedges\tcovered_px\thash"
        );
    }
    let mut seq = 0usize;
    // per (pass==0 / pass>0) per page totals
    let mut first_totals: Vec<f64> = Vec::new();
    let mut first_compile: Vec<f64> = Vec::new();
    let mut first_render: Vec<f64> = Vec::new();
    let mut warm_totals: Vec<f64> = Vec::new();
    let mut warm_compile: Vec<f64> = Vec::new();
    let mut warm_render: Vec<f64> = Vec::new();
    let mut warm_lower: Vec<f64> = Vec::new();
    let mut warm_exec: Vec<f64> = Vec::new();
    let mut pass_sums: Vec<f64> = Vec::new();
    let t_all = Instant::now();
    for pass in 0..repeat {
        let mut pass_sum = 0.0;
        for &page in &pages {
            if fresh_backend {
                backend = CpuBackend::default();
                worker = CpuWorkerContext::new();
            }
            let cached = if render_only { compiled_cache.get(&page).cloned() } else { None };
            let (one, compiled) = if seq < measure_from {
                probe_warmup_render(&snap, &comp, &backend, &mut worker, page, scale, cached)
            } else {
                probe_measured_render(&snap, &comp, &backend, &mut worker, page, scale, cached)
            };
            if render_only {
                compiled_cache.entry(page).or_insert(compiled);
            }
            seq += 1;
            let total = ms(one.compile) + ms(one.render);
            pass_sum += total;
            if pass == 0 {
                first_totals.push(total);
                first_compile.push(ms(one.compile));
                first_render.push(ms(one.render));
            } else {
                warm_totals.push(total);
                warm_compile.push(ms(one.compile));
                warm_render.push(ms(one.render));
                warm_lower.push(ms(one.stats.lower));
                warm_exec.push(ms(one.stats.raster));
            }
            let h = if do_hash { format!("{:016x}", hash_host(&one.host)) } else { "-".into() };
            if !quiet {
                println!(
                    "{pass}\t{page}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{}\t{h}",
                    ms(one.compile),
                    ms(one.stats.lower),
                    ms(one.stats.prep),
                    ms(one.stats.raster),
                    ms(one.stats.output),
                    ms(one.render),
                    total,
                    one.host.width,
                    one.host.height,
                    one.stats.ops_total,
                    one.stats.commands,
                    one.stats.edges,
                    one.stats.covered_pixels,
                );
            }
            if one.stats.degraded_draws > 0 {
                eprintln!(
                    "WARNING page {page}: {} degraded draws: {:?}",
                    one.stats.degraded_draws, one.stats.recovery_notes
                );
            }
            if pass == 0
                && let Some(dir) = &ppm_dir
            {
                std::fs::create_dir_all(dir).ok();
                write_ppm(&std::path::Path::new(dir).join(format!("page-{page:03}.ppm")), &one.host);
            }
            #[cfg(feature = "profiling")]
            if let Some(w) = prof_writer.as_mut() {
                let mut json = format!("{{\"pass\":{pass},\"page\":{page},\"scale\":{scale},");
                one.profile.write_json_fields(&mut json);
                json.push_str("}\n");
                w.write_all(json.as_bytes()).unwrap();
            }
            drop(one);
        }
        pass_sums.push(pass_sum);
    }
    let wall = t_all.elapsed();
    #[cfg(feature = "profiling")]
    if let Some(mut w) = prof_writer {
        w.flush().unwrap();
    }
    let n = pages.len();
    println!(
        "#summary file={path} scale={scale} pages={n} repeat={repeat} open_ms={:.3} wall_ms={:.1} proc_ms={:.1} \
         first_pass_ms={:.2} first_med_total={:.3} first_med_compile={:.3} first_med_render={:.3} \
         warm_med_total={:.3} warm_med_compile={:.3} warm_med_render={:.3} warm_med_lower={:.3} warm_med_execute={:.3} \
         vmhwm_kib={} vmrss_kib={}",
        ms(open),
        ms(wall),
        ms(t_proc.elapsed()),
        pass_sums.first().copied().unwrap_or(0.0),
        median(first_totals),
        median(first_compile),
        median(first_render),
        median(warm_totals),
        median(warm_compile),
        median(warm_render),
        median(warm_lower),
        median(warm_exec),
        proc_status_kib("VmHWM:").unwrap_or(0),
        proc_status_kib("VmRSS:").unwrap_or(0),
    );
}

// ---------------------------------------------------------------------------
// doc: whole document through the scheduler
// ---------------------------------------------------------------------------

fn doc(args: &[String]) {
    let mut a = Args(args.to_vec());
    let scale: f64 = a.value("--scale").map(|v| v.parse().expect("scale")).unwrap_or(1.0);
    let defaults = SchedulerOptions::default();
    let cw: usize = a
        .value("--compile-workers")
        .map(|v| v.parse().expect("compile workers"))
        .unwrap_or(defaults.compile_workers);
    let rw: usize = a
        .value("--render-workers")
        .map(|v| v.parse().expect("render workers"))
        .unwrap_or(defaults.render_workers);
    let runs: usize = a.value("--runs").map(|v| v.parse().expect("runs")).unwrap_or(1);
    let system_fonts = a.flag("--system-fonts");
    let do_hash = a.flag("--hash");
    let ppm_dir = a.value("--ppm");
    let pages_end: Option<u32> = a.value("--pages-end").map(|v| v.parse().expect("pages-end"));
    let path = a.positional().to_owned();

    let (snap, open) = open_snapshot(&path);
    let count = pages_end.map_or(snap.page_count(), |n| n.min(snap.page_count()));
    let provider: Option<Arc<dyn pdf_font::SystemFontProvider>> = if system_fonts {
        Some(Arc::new(pdf_font::FolderFontProvider::system()))
    } else {
        None
    };
    let options = SchedulerOptions {
        compile_workers: cw,
        render_workers: rw,
        reorder_window_depth: SchedulerOptions::reorder_window_for(cw, rw),
        ..defaults
    };
    for run in 0..runs {
        let backend: Arc<dyn RenderBackend> = Arc::new(CpuBackend::default());
        let scheduler = RenderScheduler::new(backend, options.clone());
        let provider = provider.clone();
        let make = move |s: &DocumentSnapshot, page: PageIndex| -> Result<RenderRequest, RenderError> {
            let mut ctx = ParseContext::new();
            let compiled = compiler(provider.clone())
                .compile(s, page, &mut ctx)
                .map_err(|e| RenderError::Backend(e.to_string()))?;
            Ok(request_for_page(Arc::new(compiled), scale))
        };
        let mut ok = 0u32;
        let mut failed = 0u32;
        let mut pixels = 0u64;
        let t = Instant::now();
        let mut first_ms = None;
        scheduler.render_range(&snap, 0..count, &make, None, &mut |out: PipelineOutput| {
            match out.result {
                Ok(RenderedPage::Host(host)) => {
                    ok += 1;
                    pixels += u64::from(host.width) * u64::from(host.height);
                    if first_ms.is_none() {
                        first_ms = Some(ms(t.elapsed()));
                    }
                    if do_hash {
                        println!(
                            "#dochash run={run} page={} w={} h={} hash={:016x}",
                            out.page.0,
                            host.width,
                            host.height,
                            hash_host(&host)
                        );
                    }
                    if let Some(dir) = &ppm_dir {
                        std::fs::create_dir_all(dir).ok();
                        write_ppm(
                            &std::path::Path::new(dir)
                                .join(format!("doc-run{run}-page-{:03}.ppm", out.page.0)),
                            &host,
                        );
                    }
                    drop(std::hint::black_box(host));
                }
                Ok(_) => failed += 1,
                Err(e) => {
                    failed += 1;
                    eprintln!("page {}: {e}", out.page.0);
                }
            }
        });
        let wall = t.elapsed();
        println!(
            "#doc file={path} scale={scale} compile_workers={cw} render_workers={rw} run={run} pages={ok} failed={failed} \
             open_ms={:.2} first_page_ms={:.2} wall_ms={:.2} pages_per_s={:.2} ms_per_page={:.3} mpx_per_s={:.1} vmhwm_kib={} vmrss_kib={}",
            ms(open),
            first_ms.unwrap_or(f64::NAN),
            ms(wall),
            f64::from(ok) / wall.as_secs_f64(),
            ms(wall) / f64::from(ok.max(1)),
            pixels as f64 / 1e6 / wall.as_secs_f64(),
            proc_status_kib("VmHWM:").unwrap_or(0),
            proc_status_kib("VmRSS:").unwrap_or(0),
        );
    }
}

// ---------------------------------------------------------------------------
// seq: one page at a sequence of scales/sizes through ONE backend (P1 probe)
// ---------------------------------------------------------------------------

/// Same matrix as lege-pdf-read `page_to_device_matrix`: independent x/y
/// scales that map the displayed crop box onto exactly `w`x`h` pixels.
fn request_for_size(page: Arc<CompiledPage>, w: u32, h: u32) -> RenderRequest {
    let crop = page.bounds.crop;
    let rotation = page.bounds.rotate % 360;
    let (dw, dh) = match rotation {
        90 | 270 => (crop.height(), crop.width()),
        _ => (crop.width(), crop.height()),
    };
    let sx = w as f64 / dw.max(f64::EPSILON);
    let sy = h as f64 / dh.max(f64::EPSILON);
    let matrix = match rotation {
        90 => Matrix { a: 0.0, b: sy, c: sx, d: 0.0, e: -crop.y0 * sx, f: -crop.x0 * sy },
        180 => Matrix { a: -sx, b: 0.0, c: 0.0, d: sy, e: crop.x1 * sx, f: -crop.y0 * sy },
        270 => Matrix { a: 0.0, b: -sy, c: -sx, d: 0.0, e: crop.y1 * sx, f: crop.x1 * sy },
        _ => Matrix { a: sx, b: 0.0, c: 0.0, d: -sy, e: -crop.x0 * sx, f: crop.y1 * sy },
    };
    let mut r = request_for_page(page, 1.0);
    r.transform = PageTransform { matrix };
    r.output_size = DeviceSize { width: w, height: h };
    r
}

#[derive(Clone, Copy)]
enum Step {
    Scale(f64),
    Size(u32, u32),
}

impl Step {
    fn label(&self) -> String {
        match *self {
            Step::Scale(s) => format!("s{s}"),
            Step::Size(w, h) => format!("{w}x{h}"),
        }
    }
    fn request(&self, page: &Arc<CompiledPage>) -> RenderRequest {
        match *self {
            Step::Scale(s) => request_for_page(Arc::clone(page), s),
            Step::Size(w, h) => request_for_size(Arc::clone(page), w, h),
        }
    }
}

/// A `fmt::Write` sink that keeps only the first `cap` bytes and then aborts
/// formatting (so Debug-printing a PreparedImage never formats its samples).
struct Prefix {
    buf: String,
    cap: usize,
}

impl std::fmt::Write for Prefix {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        if self.buf.len() >= self.cap {
            return Err(std::fmt::Error);
        }
        self.buf.push_str(s);
        Ok(())
    }
}

/// Decoded source dimensions of every image draw the backend prepares for
/// `req` (probing the backend's image cache exactly like a render would; on a
/// backend that just rendered `req` this is a cache hit returning the payload
/// that render used).
fn decoded_image_dims(backend: &CpuBackend, req: &RenderRequest) -> Vec<String> {
    use std::fmt::Write as _;
    let Ok(prepared) = backend.prepare_attribution(req) else {
        return vec!["prepare-failed".into()];
    };
    let mut out = Vec::new();
    for op in &prepared.ops {
        let mut p = Prefix { buf: String::new(), cap: 2048 };
        let _ = write!(p, "{op:?}");
        if !p.buf.starts_with("Image(") {
            continue;
        }
        // PreparedImage field order: origin, bounds (a DeviceRect that also
        // has width/height), clip, clip_has_mask, inv, width, height, ...
        // so search after `inv:` for the decoded raster's own dimensions.
        let after_inv = p.buf.find(" inv: ").unwrap_or(0);
        let tail = &p.buf[after_inv..];
        let grab = |key: &str| -> String {
            tail
                .find(key)
                .map(|i| {
                    tail[i + key.len()..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>()
                })
                .unwrap_or_else(|| "?".into())
        };
        out.push(format!("{}x{}", grab(" width: "), grab(" height: ")));
    }
    out
}

fn seq(args: &[String]) {
    let mut a = Args(args.to_vec());
    let scales = a.value("--scales");
    let sizes = a.value("--sizes");
    let steps: Vec<Step> = match (scales, sizes) {
        (Some(s), None) => s
            .split(',')
            .map(|v| Step::Scale(v.trim().parse().expect("scale")))
            .collect(),
        (None, Some(s)) => s
            .split(',')
            .map(|v| {
                let (w, h) = v.trim().split_once('x').expect("size must be WxH");
                Step::Size(w.parse().expect("width"), h.parse().expect("height"))
            })
            .collect(),
        _ => {
            eprintln!("seq needs exactly one of --scales S1,S2,... or --sizes W1xH1,...");
            std::process::exit(2);
        }
    };
    let page: u32 = a.value("--page").map(|v| v.parse().expect("page")).unwrap_or(0);
    let fresh_compare = a.flag("--fresh-compare");
    let show_decode = a.flag("--show-decode");
    let ppm_dir = a.value("--ppm");
    let system_fonts = a.flag("--system-fonts");
    let path = a.positional().to_owned();

    let (snap, _open) = open_snapshot(&path);
    let provider: Option<Arc<dyn pdf_font::SystemFontProvider>> = if system_fonts {
        Some(Arc::new(pdf_font::FolderFontProvider::system()))
    } else {
        None
    };
    let comp = compiler(provider);
    let mut ctx = ParseContext::new();
    // Compiled once and rendered at every step, like lege-pdf-read's
    // RenderSession (compiled-page cache + one CpuBackend per document).
    let compiled = Arc::new(comp.compile(&snap, PageIndex(page), &mut ctx).expect("compile page"));
    if let Some(dir) = &ppm_dir {
        std::fs::create_dir_all(dir).ok();
    }
    println!("#mode\tstep\tlabel\tpage\tw\th\trender_ms\tdegraded\tdecoded\thash");
    let mut seq_hashes = Vec::new();
    {
        let backend = CpuBackend::default();
        let mut worker = CpuWorkerContext::new();
        for (i, step) in steps.iter().enumerate() {
            let req = step.request(&compiled);
            let t = Instant::now();
            let (host, stats) = backend.render_with(&req, &mut worker).expect("render page");
            let dt = t.elapsed();
            let h = hash_host(&host);
            let dims = if show_decode { decoded_image_dims(&backend, &req).join(",") } else { "-".into() };
            println!(
                "seq\t{i}\t{}\t{page}\t{}\t{}\t{:.3}\t{}\t{dims}\t{h:016x}",
                step.label(),
                host.width,
                host.height,
                ms(dt),
                stats.degraded_draws
            );
            if let Some(dir) = &ppm_dir {
                write_ppm(
                    &std::path::Path::new(dir).join(format!("seq{i}_{}_p{page}.ppm", step.label())),
                    &host,
                );
            }
            seq_hashes.push(h);
        }
        eprintln!("#cache-stats-follow backend=seq");
    }
    if fresh_compare {
        let mut all_equal = true;
        for (i, step) in steps.iter().enumerate() {
            let backend = CpuBackend::default();
            let mut worker = CpuWorkerContext::new();
            let req = step.request(&compiled);
            let t = Instant::now();
            let (host, stats) = backend.render_with(&req, &mut worker).expect("render page");
            let dt = t.elapsed();
            let h = hash_host(&host);
            let dims = if show_decode { decoded_image_dims(&backend, &req).join(",") } else { "-".into() };
            println!(
                "fresh\t{i}\t{}\t{page}\t{}\t{}\t{:.3}\t{}\t{dims}\t{h:016x}",
                step.label(),
                host.width,
                host.height,
                ms(dt),
                stats.degraded_draws
            );
            let eq = h == seq_hashes[i];
            all_equal &= eq;
            println!(
                "#cmp step={i} label={} seq={:016x} fresh={h:016x} {}",
                step.label(),
                seq_hashes[i],
                if eq { "EQUAL" } else { "DIFFERENT" }
            );
            if let Some(dir) = &ppm_dir {
                write_ppm(
                    &std::path::Path::new(dir).join(format!("fresh{i}_{}_p{page}.ppm", step.label())),
                    &host,
                );
            }
            eprintln!("#cache-stats-follow backend=fresh step={i}");
        }
        println!("#summary all_equal={all_equal}");
    }
}

// ---------------------------------------------------------------------------
// fontscan: cold system-font index cost
// ---------------------------------------------------------------------------

fn fontscan() {
    let t0 = Instant::now();
    let provider = pdf_font::FolderFontProvider::system();
    let construct = t0.elapsed();
    use pdf_font::SystemFontProvider;
    let req = |family: &'static [u8]| pdf_font::SystemFontRequest {
        family,
        bold: false,
        italic: false,
        serif: true,
        fixed_pitch: false,
        charset: pdf_font::Charset::Ansi,
    };
    let t1 = Instant::now();
    let first = provider.lookup(&req(b"Times-Roman"));
    let first_lookup = t1.elapsed();
    let t2 = Instant::now();
    let second = provider.lookup(&req(b"Helvetica"));
    let second_lookup = t2.elapsed();
    let t3 = Instant::now();
    let third = provider.lookup(&req(b"LiberationSerif"));
    let third_lookup = t3.elapsed();
    println!(
        "#fontscan construct_ms={:.3} first_lookup_ms={:.3} (found={}) second_lookup_ms={:.3} (found={}) third_lookup_ms={:.3} (found={}, bytes={}) vmhwm_kib={}",
        ms(construct),
        ms(first_lookup),
        first.is_some(),
        ms(second_lookup),
        second.is_some(),
        ms(third_lookup),
        third.is_some(),
        third.as_ref().map(|f| f.data.len()).unwrap_or(0),
        proc_status_kib("VmHWM:").unwrap_or(0),
    );
}
