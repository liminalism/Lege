//! The headless backend: writes sheets to disk and records the job it was
//! given. It is the CI backend and how every layout test asserts without a
//! printer attached.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{
    DeviceCapabilities, JobId, JobStatus, PrinterId, PrinterInfo, SpoolJob, SpoolPayload, Spooler,
};
use crate::compose::SheetRaster;
use crate::paper::Margins;
use crate::{ComposeOptions, PrintError, PrintOptions};

/// The synthetic queue name the file backend reports.
pub const FILE_PRINTER: &str = "file";

/// What a `FileSpooler` recorded about one submission.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedJob {
    pub id: JobId,
    pub printer: PrinterId,
    pub title: String,
    /// Files written for this job, in sheet order.
    pub files: Vec<PathBuf>,
    /// True when the job took the pass-through path.
    pub pass_through: bool,
    /// The options the caller handed the backend, so a test can assert what
    /// actually reached the spooler.
    pub options: PrintOptions,
    /// The composition settings used, or `None` for a pass-through job.
    pub compose: Option<ComposeOptions>,
}

/// Writes each sheet to `dir` as PNG.
///
/// File names are `sheet-0001.png`, `sheet-0002.png`, … The counter is per
/// spooler rather than per job, so a second submission into the same
/// directory continues the sequence instead of overwriting the first.
/// Pass-through jobs write `document-0001.pdf`, numbered by job.
#[derive(Debug)]
pub struct FileSpooler {
    dir: PathBuf,
    jobs: Mutex<Vec<RecordedJob>>,
    /// Sheets written so far, so names never collide across submissions.
    sheets_written: Mutex<u32>,
}

impl FileSpooler {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            jobs: Mutex::new(Vec::new()),
            sheets_written: Mutex::new(0),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every job submitted so far, oldest first.
    #[must_use]
    pub fn recorded(&self) -> Vec<RecordedJob> {
        lock(&self.jobs).clone()
    }

    /// The most recent submission, if any.
    #[must_use]
    pub fn last_job(&self) -> Option<RecordedJob> {
        lock(&self.jobs).last().cloned()
    }

    /// Write already-composed rasters, exactly as [`Spooler::submit`] would.
    ///
    /// This is the seam the tests use: it exercises the whole backend —
    /// naming, PNG encoding, recording — without needing a `RenderSession`
    /// or the composition stage.
    pub fn submit_rasters(
        &self,
        printer: &PrinterId,
        title: &str,
        options: &PrintOptions,
        compose: ComposeOptions,
        rasters: &[SheetRaster],
    ) -> Result<JobId, PrintError> {
        self.submit_raster_stream(printer, title, options, compose, rasters.iter().map(Ok))
    }

    /// Write sheets as `rasters` yields them: each is written to disk and
    /// dropped before the next one is pulled, so a lazy iterator that
    /// composes on demand keeps at most one composed sheet alive. This is how
    /// [`Spooler::submit`] spools a `Sheets` payload; composing every sheet
    /// first held the whole job in memory (26 MB per A4 sheet at 300 DPI).
    ///
    /// When an item is an error, or a sheet cannot be written, the files
    /// already written for this job are removed (best effort), nothing is
    /// recorded, and the sheet counter is rolled back when no other job has
    /// advanced it meanwhile, so the next job's names are unaffected.
    pub fn submit_raster_stream<R, I>(
        &self,
        printer: &PrinterId,
        title: &str,
        options: &PrintOptions,
        compose: ComposeOptions,
        rasters: I,
    ) -> Result<JobId, PrintError>
    where
        R: std::borrow::Borrow<SheetRaster>,
        I: IntoIterator<Item = Result<R, PrintError>>,
    {
        std::fs::create_dir_all(&self.dir)?;
        let mut files = Vec::new();
        let mut numbers = Vec::new();
        let write_all = || -> Result<(), PrintError> {
            for raster in rasters {
                let raster = raster?;
                let number = self.next_sheet_number();
                let path = self.dir.join(sheet_name(number));
                // Recorded before writing, so a partly written file is
                // removed on failure too.
                numbers.push(number);
                files.push(path.clone());
                write_sheet_png(&path, raster.borrow())?;
            }
            Ok(())
        };
        if let Err(error) = write_all() {
            self.discard_sheets(&files, &numbers);
            return Err(error);
        }
        Ok(self.record(printer, title, options, files, false, Some(compose)))
    }

    /// Consume one number from the sheet counter.
    fn next_sheet_number(&self) -> u32 {
        let mut count = lock(&self.sheets_written);
        *count += 1;
        *count
    }

    /// Undo a failed job's sheets: remove its files and, when its numbers
    /// are still the counter's latest (no other job interleaved), give them
    /// back.
    fn discard_sheets(&self, files: &[PathBuf], numbers: &[u32]) {
        for path in files {
            let _ = std::fs::remove_file(path);
        }
        let (Some(&first), Some(&last)) = (numbers.first(), numbers.last()) else {
            return;
        };
        let mut count = lock(&self.sheets_written);
        let contiguous = (last - first) as usize + 1 == numbers.len();
        if contiguous && *count == last {
            *count = first - 1;
        }
    }

    fn record(
        &self,
        printer: &PrinterId,
        title: &str,
        options: &PrintOptions,
        files: Vec<PathBuf>,
        pass_through: bool,
        compose: Option<ComposeOptions>,
    ) -> JobId {
        let mut jobs = lock(&self.jobs);
        let id = JobId(format!("{FILE_PRINTER}-{}", jobs.len() + 1));
        jobs.push(RecordedJob {
            id: id.clone(),
            printer: printer.clone(),
            title: title.to_string(),
            files,
            pass_through,
            options: options.clone(),
            compose,
        });
        id
    }

    fn job_count(&self) -> usize {
        lock(&self.jobs).len()
    }
}

impl Spooler for FileSpooler {
    fn printers(&self) -> Result<Vec<PrinterInfo>, PrintError> {
        Ok(vec![PrinterInfo {
            id: PrinterId::new(FILE_PRINTER),
            description: Some(format!("File spooler ({})", self.dir.display())),
            location: Some(self.dir.display().to_string()),
            is_default: true,
            accepting_jobs: true,
        }])
    }

    /// A directory has no unprintable border and no driver to disagree with,
    /// so the capabilities are the permissive ones: zero hardware margins,
    /// duplex and colour available, and PDF accepted (pass-through here just
    /// copies the bytes).
    fn capabilities(&self, _printer: &PrinterId) -> Result<DeviceCapabilities, PrintError> {
        Ok(DeviceCapabilities {
            hardware_margins: Margins::ZERO,
            supports_duplex: true,
            supports_color: true,
            resolution_dpi: Some(ComposeOptions::default().dpi),
            accepts_pdf: true,
        })
    }

    fn submit(&self, job: SpoolJob<'_>) -> Result<JobId, PrintError> {
        match job.payload {
            SpoolPayload::PassThroughPdf(bytes) => {
                std::fs::create_dir_all(&self.dir)?;
                let path = self
                    .dir
                    .join(format!("document-{:04}.pdf", self.job_count() + 1));
                std::fs::write(&path, bytes)?;
                Ok(self.record(
                    &job.printer,
                    &job.title,
                    job.options,
                    vec![path],
                    true,
                    None,
                ))
            }
            SpoolPayload::Sheets {
                session,
                sheets,
                compose,
            } => {
                // Composed lazily: each sheet is written and dropped before
                // the next is composed (B12).
                let rasters = sheets
                    .iter()
                    .map(|sheet| crate::compose::compose_sheet(session, sheet, &compose));
                self.submit_raster_stream(&job.printer, &job.title, job.options, compose, rasters)
            }
        }
    }

    /// A file is written by the time `submit` returns, so there is nothing
    /// left to wait for.
    fn status(&self, _job: &JobId) -> Result<JobStatus, PrintError> {
        Ok(JobStatus::Completed)
    }

    /// Nothing to cancel: the job completed synchronously.
    fn cancel(&self, _job: &JobId) -> Result<(), PrintError> {
        Ok(())
    }
}

/// The file name of sheet `number`.
fn sheet_name(number: u32) -> String {
    format!("sheet-{number:04}.png")
}

/// Encode `raster` as an 8-bit PNG at `path`.
///
/// Shared with the CUPS backend, which stages composed sheets as PNG for the
/// `lp` filter chain.
pub fn write_sheet_png(path: &Path, raster: &SheetRaster) -> Result<(), PrintError> {
    let color = match raster.channels {
        1 => png::ColorType::Grayscale,
        3 => png::ColorType::Rgb,
        4 => png::ColorType::Rgba,
        other => {
            return Err(PrintError::Spool(format!(
                "cannot encode a {other}-channel sheet raster as PNG"
            )));
        }
    };
    let expected = u64::from(raster.width) * u64::from(raster.height) * u64::from(raster.channels);
    if expected != raster.pixels.len() as u64 {
        return Err(PrintError::Spool(format!(
            "sheet raster is {} bytes, expected {expected} for {}x{}x{}",
            raster.pixels.len(),
            raster.width,
            raster.height,
            raster.channels
        )));
    }

    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), raster.width, raster.height);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(png_error)?;
    writer.write_image_data(&raster.pixels).map_err(png_error)?;
    writer.finish().map_err(png_error)?;
    Ok(())
}

fn png_error(error: png::EncodingError) -> PrintError {
    match error {
        png::EncodingError::IoError(io) => PrintError::Io(io),
        other => PrintError::Spool(format!("png encoding failed: {other}")),
    }
}

/// Take a lock, ignoring poisoning: the spooler's state is a plain record and
/// a panic elsewhere does not make it invalid.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
