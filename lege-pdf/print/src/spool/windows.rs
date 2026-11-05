//! Windows backend: `winspool.drv` for the queue, GDI for the raster path.
//!
//! Windows has no native PDF spool format, so every job here is composed:
//! [`SpoolPayload::PassThroughPdf`] is rejected rather than silently
//! mishandled, and the caller falls back to the sheet path.
//!
//! Windows is also the one platform that will tell us the *real* unprintable
//! border, through `GetDeviceCaps(PHYSICALOFFSETX/Y)`. Everywhere else the
//! crate has to assume a conservative quarter inch.
//!
//! Experimental: compile-tested, but not yet verified against a real queue.

// Every FFI call below is a documented `unsafe` block. The workspace lint is
// a warning rather than a hard denial precisely so a platform backend like
// this one can exist; nothing outside this module is unsafe.
#![allow(unsafe_code)]

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDCW, DEVMODEW, DIB_RGB_COLORS, DM_COLLATE,
    DM_COLOR, DM_COPIES, DM_DUPLEX, DM_IN_BUFFER, DM_ORIENTATION, DM_OUT_BUFFER, DM_PAPERLENGTH,
    DM_PAPERSIZE, DM_PAPERWIDTH, DMCOLLATE_FALSE, DMCOLLATE_TRUE, DMCOLOR_COLOR,
    DMCOLOR_MONOCHROME, DMDUP_HORIZONTAL, DMDUP_SIMPLEX, DMDUP_VERTICAL, DMORIENT_LANDSCAPE,
    DMORIENT_PORTRAIT, DMPAPER_A3, DMPAPER_A4, DMPAPER_A5, DMPAPER_B5, DMPAPER_EXECUTIVE,
    DMPAPER_LEGAL, DMPAPER_LETTER, DMPAPER_TABLOID, DMPAPER_USER, DeleteDC, GetDeviceCaps, HDC,
    HORZRES, LOGPIXELSX, LOGPIXELSY, PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY,
    PHYSICALWIDTH, SRCCOPY, StretchDIBits, VERTRES,
};
use windows::Win32::Graphics::Printing::{
    ClosePrinter, DocumentPropertiesW, EnumPrintersW, GetDefaultPrinterW, JOB_INFO_1W,
    OpenPrinterW, PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_4W,
    SetJobW,
};
use windows::Win32::Storage::Xps::{
    AbortDoc, DC_COLORDEVICE, DC_DUPLEX, DOCINFOW, DeviceCapabilitiesW, EndDoc, EndPage, StartDocW,
    StartPage,
};
use windows::core::{PCWSTR, PWSTR};

use super::{
    DeviceCapabilities, JobId, JobStatus, PrinterId, PrinterInfo, SpoolJob, SpoolPayload, Spooler,
};
use crate::paper::{Margins, POINTS_PER_INCH, PaperSize};
use crate::{Duplex, Orientation, PrintError, PrintJob, PrintOptions, Sheet};

/// `SetJob`'s `JOB_CONTROL_CANCEL`.
const JOB_CONTROL_CANCEL: u32 = 3;
/// `JOB_STATUS_*` bits we act on.
const JOB_STATUS_PAUSED: u32 = 0x0000_0001;
const JOB_STATUS_ERROR: u32 = 0x0000_0002;
const JOB_STATUS_DELETING: u32 = 0x0000_0004;
const JOB_STATUS_PRINTING: u32 = 0x0000_0010;
const JOB_STATUS_PRINTED: u32 = 0x0000_0080;
const JOB_STATUS_DELETED: u32 = 0x0000_0100;

#[derive(Debug, Default)]
pub struct WindowsSpooler {
    _private: (),
}

impl WindowsSpooler {
    #[must_use]
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Spooler for WindowsSpooler {
    fn printers(&self) -> Result<Vec<PrinterInfo>, PrintError> {
        let names = enum_printer_names()?;
        let default = default_printer_name();
        Ok(names
            .into_iter()
            .map(|name| PrinterInfo {
                is_default: default.as_deref() == Some(name.as_str()),
                id: PrinterId::new(name),
                // `PRINTER_INFO_4W` is the cheap level and carries neither.
                // `PRINTER_INFO_2W` would, but it needs the `Win32_Security`
                // feature for its security descriptor field, which this crate
                // has no other use for.
                description: None,
                location: None,
                // Level 4 does not report the queue status either. A paused
                // queue still accepts jobs; they simply wait.
                accepting_jobs: true,
            })
            .collect())
    }

    fn capabilities(&self, printer: &PrinterId) -> Result<DeviceCapabilities, PrintError> {
        let name = wide(printer.as_str());
        let dc = InfoDc::open(&name)?;
        let metrics = dc.metrics();

        // SAFETY: `name` is a NUL-terminated wide string that outlives the
        // call; a null port and null DEVMODE ask about the queue's defaults.
        let duplex = unsafe {
            DeviceCapabilitiesW(PCWSTR(name.as_ptr()), PCWSTR::null(), DC_DUPLEX, None, None)
        };
        // SAFETY: as above.
        let color = unsafe {
            DeviceCapabilitiesW(
                PCWSTR(name.as_ptr()),
                PCWSTR::null(),
                DC_COLORDEVICE,
                None,
                None,
            )
        };

        Ok(DeviceCapabilities {
            hardware_margins: metrics.hardware_margins(),
            supports_duplex: duplex > 0,
            supports_color: color > 0,
            resolution_dpi: metrics.dpi_x(),
            // No Windows print processor takes PDF. Pass-through is a CUPS
            // affordance and nothing else.
            accepts_pdf: false,
        })
    }

    fn plan_sheets(
        &self,
        printer: &PrinterId,
        job: &PrintJob,
        _capabilities: &DeviceCapabilities,
    ) -> Result<Vec<Sheet>, PrintError> {
        job.options.validate()?;
        // Square bounds cannot carry an auto-selected orientation to submit().
        // Use portrait deterministically, while preserving explicit choices.
        let mut square_job;
        let job = if job.options.orientation == Orientation::Auto
            && job.options.paper.width() == job.options.paper.height()
        {
            square_job = job.clone();
            square_job.options.orientation = Orientation::Portrait;
            &square_job
        } else {
            job
        };
        let name = wide(printer.as_str());
        let margins = |landscape| -> Result<Margins, PrintError> {
            let devmode = DevMode::for_job(&name, &job.options, landscape)?;
            let dc = InfoDc::open_with_devmode(&name, devmode.as_ptr())?;
            let metrics = dc.metrics();
            metrics.validate_paper(job.options.paper, landscape)?;
            Ok(metrics.hardware_margins())
        };
        // Ask the driver about the requested media in its actual orientation;
        // queue-default portrait margins cannot describe a landscape A3 job.
        let portrait = if job.options.orientation == Orientation::Landscape {
            Margins::ZERO
        } else {
            margins(false)?
        };
        let landscape = if job.options.orientation == Orientation::Portrait {
            Margins::ZERO
        } else {
            margins(true)?
        };
        crate::impose_uniform(job, portrait, landscape)
    }

    fn submit(&self, job: SpoolJob<'_>) -> Result<JobId, PrintError> {
        job.options.validate()?;
        match job.payload {
            SpoolPayload::PassThroughPdf(_) => Err(PrintError::Unsupported(
                "Windows has no PDF spool format; compose the job to sheets instead",
            )),
            SpoolPayload::Sheets {
                session,
                sheets,
                compose,
            } => {
                // The DEVMODE orientation has to match the sheets we composed;
                // reject inconsistent direct submissions before opening a job.
                let landscape = sheet_orientation(sheets, job.options)?;
                let name = wide(job.printer.as_str());
                let devmode = DevMode::for_job(&name, job.options, landscape)?;
                let dc = PrintDc::open(&name, devmode.as_ptr())?;
                let geometry = dc.metrics();
                geometry.validate_paper(job.options.paper, landscape)?;
                let job_id = dc.start_doc(&job.title)?;
                for sheet in sheets {
                    let raster = crate::compose::compose_sheet(session, sheet, &compose)?;
                    dc.print_page(&raster, geometry)?;
                }
                dc.end_doc()?;
                Ok(JobId(job_id.to_string()))
            }
        }
    }

    fn status(&self, job: &JobId) -> Result<JobStatus, PrintError> {
        let Ok(target) = job.0.parse::<u32>() else {
            return Ok(JobStatus::Unknown);
        };
        // A Windows job id is only unique per printer, so every queue has to
        // be asked. In practice the loop stops at the first hit.
        for name in enum_printer_names()? {
            let handle = PrinterHandle::open(&wide(&name))?;
            if let Some(status) = handle.job_status(target)? {
                return Ok(status);
            }
        }
        // Gone from every queue: printed and reaped.
        Ok(JobStatus::Completed)
    }

    fn cancel(&self, job: &JobId) -> Result<(), PrintError> {
        let target = job
            .0
            .parse::<u32>()
            .map_err(|_| PrintError::Spool(format!("{job} is not a Windows job id")))?;
        for name in enum_printer_names()? {
            let handle = PrinterHandle::open(&wide(&name))?;
            if handle.job_status(target)?.is_some() {
                return handle.cancel(target);
            }
        }
        Err(PrintError::Spool(format!("job {job} is not in any queue")))
    }
}

fn sheet_orientation(sheets: &[Sheet], options: &PrintOptions) -> Result<bool, PrintError> {
    let first = sheets.first().ok_or(PrintError::EmptyRange)?;
    let paper = options.paper;
    let landscape = match options.orientation {
        Orientation::Portrait => false,
        Orientation::Landscape => true,
        Orientation::Auto => {
            first.bounds == paper.rect(Orientation::Landscape)
                && first.bounds != paper.rect(Orientation::Portrait)
        }
    };
    let orientation = if landscape {
        Orientation::Landscape
    } else {
        Orientation::Portrait
    };
    let expected = paper.rect(orientation);
    if sheets.iter().any(|sheet| sheet.bounds != expected) {
        return Err(PrintError::InvalidOptions(
            "Windows requires one paper size and orientation per job; use Spooler::plan_sheets"
                .into(),
        ));
    }
    Ok(landscape)
}

// ---------------------------------------------------------------------------
// Queue enumeration
// ---------------------------------------------------------------------------

fn enum_printer_names() -> Result<Vec<String>, PrintError> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let mut needed = 0u32;
    let mut returned = 0u32;
    // SAFETY: the sizing call writes only through the two out-pointers, both
    // of which are valid locals. It is expected to fail with
    // ERROR_INSUFFICIENT_BUFFER, which is why the result is discarded.
    let _ = unsafe { EnumPrintersW(flags, PCWSTR::null(), 4, None, &mut needed, &mut returned) };
    if needed == 0 {
        return Ok(Vec::new());
    }

    let mut buffer = AlignedBuffer::new(needed as usize);
    // SAFETY: the buffer is at least `needed` bytes and pointer-aligned, as
    // PRINTER_INFO_4W requires; the out-pointers are valid locals.
    unsafe {
        EnumPrintersW(
            flags,
            PCWSTR::null(),
            4,
            Some(buffer.as_bytes_mut()),
            &mut needed,
            &mut returned,
        )
    }
    .map_err(|e| PrintError::Spool(format!("EnumPrinters failed: {e}")))?;

    let mut names = Vec::with_capacity(returned as usize);
    for index in 0..returned as usize {
        // SAFETY: the spooler wrote `returned` PRINTER_INFO_4W records into
        // the front of the buffer, and the buffer outlives this loop. The
        // name pointers point into the same buffer.
        let name = unsafe {
            let entry = buffer.as_ptr().cast::<PRINTER_INFO_4W>().add(index).read();
            pwstr_to_string(entry.pPrinterName)
        };
        if !name.is_empty() {
            names.push(name);
        }
    }
    Ok(names)
}

fn default_printer_name() -> Option<String> {
    let mut len = 0u32;
    // SAFETY: the sizing call writes only `len`; it is expected to fail.
    let _ = unsafe { GetDefaultPrinterW(None, &mut len) };
    if len == 0 {
        return None;
    }
    let mut buffer = vec![0u16; len as usize];
    // SAFETY: `buffer` holds `len` UTF-16 units, which is what the sizing
    // call asked for, and `len` is a valid local.
    let ok = unsafe { GetDefaultPrinterW(Some(PWSTR(buffer.as_mut_ptr())), &mut len) };
    if !ok.as_bool() {
        return None;
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

// ---------------------------------------------------------------------------
// Device context
// ---------------------------------------------------------------------------

/// The device metrics a printer DC reports, in device units.
#[derive(Debug, Clone, Copy)]
struct DcMetrics {
    physical_width: i32,
    physical_height: i32,
    offset_x: i32,
    offset_y: i32,
    printable_width: i32,
    printable_height: i32,
    dpi_x: i32,
    dpi_y: i32,
}

impl DcMetrics {
    fn read(hdc: HDC) -> Self {
        // SAFETY: `hdc` is a live DC for the duration of the call; every
        // index is a documented GetDeviceCaps constant.
        let get = |index| unsafe { GetDeviceCaps(Some(hdc), index) };
        Self {
            physical_width: get(PHYSICALWIDTH),
            physical_height: get(PHYSICALHEIGHT),
            offset_x: get(PHYSICALOFFSETX),
            offset_y: get(PHYSICALOFFSETY),
            printable_width: get(HORZRES),
            printable_height: get(VERTRES),
            dpi_x: get(LOGPIXELSX),
            dpi_y: get(LOGPIXELSY),
        }
    }

    fn dpi_x(self) -> Option<f64> {
        (self.dpi_x > 0).then(|| f64::from(self.dpi_x))
    }

    fn validate_paper(self, paper: PaperSize, landscape: bool) -> Result<(), PrintError> {
        if self.dpi_x <= 0
            || self.dpi_y <= 0
            || self.physical_width <= 0
            || self.physical_height <= 0
        {
            return Err(PrintError::Spool(
                "printer returned invalid physical page metrics".into(),
            ));
        }
        let (mut width, mut height) = paper.size();
        if landscape {
            std::mem::swap(&mut width, &mut height);
        }
        let actual_width = f64::from(self.physical_width) * POINTS_PER_INCH / f64::from(self.dpi_x);
        let actual_height =
            f64::from(self.physical_height) * POINTS_PER_INCH / f64::from(self.dpi_y);
        // Driver dimensions are rounded to device pixels (and custom media
        // to tenths of a millimetre). Two points allow those roundings while
        // still rejecting a driver that kept its default paper/orientation.
        if (actual_width - width).abs() > 2.0 || (actual_height - height).abs() > 2.0 {
            return Err(PrintError::Spool(format!(
                "printer configured {actual_width:.1}x{actual_height:.1}pt instead of requested {width:.1}x{height:.1}pt"
            )));
        }
        Ok(())
    }

    /// The unprintable border in points — the real one, straight from the
    /// driver.
    fn hardware_margins(self) -> Margins {
        if self.dpi_x <= 0 || self.dpi_y <= 0 {
            return Margins::uniform(crate::paper::DEFAULT_HARDWARE_MARGIN_PT);
        }
        let to_pt_x = |px: i32| f64::from(px) / f64::from(self.dpi_x) * POINTS_PER_INCH;
        let to_pt_y = |px: i32| f64::from(px) / f64::from(self.dpi_y) * POINTS_PER_INCH;
        let right = self.physical_width - self.offset_x - self.printable_width;
        let bottom = self.physical_height - self.offset_y - self.printable_height;
        Margins {
            left: to_pt_x(self.offset_x.max(0)),
            right: to_pt_x(right.max(0)),
            top: to_pt_y(self.offset_y.max(0)),
            bottom: to_pt_y(bottom.max(0)),
        }
    }
}

/// An information-only DC, used for capability queries.
struct InfoDc {
    hdc: HDC,
}

impl InfoDc {
    fn open(name: &[u16]) -> Result<Self, PrintError> {
        Self::open_with_devmode(name, std::ptr::null())
    }

    fn open_with_devmode(name: &[u16], devmode: *const DEVMODEW) -> Result<Self, PrintError> {
        // SAFETY: `name` is a NUL-terminated wide string that outlives the
        // call; `devmode` is null or points to a live driver-validated buffer.
        let hdc = unsafe {
            CreateDCW(
                PCWSTR::null(),
                PCWSTR(name.as_ptr()),
                PCWSTR::null(),
                if devmode.is_null() {
                    None
                } else {
                    Some(devmode)
                },
            )
        };
        if hdc.is_invalid() {
            return Err(PrintError::NoSuchPrinter(String::from_utf16_lossy(
                &name[..name.len().saturating_sub(1)],
            )));
        }
        Ok(Self { hdc })
    }

    fn metrics(&self) -> DcMetrics {
        DcMetrics::read(self.hdc)
    }
}

impl Drop for InfoDc {
    fn drop(&mut self) {
        // SAFETY: the DC was created by `CreateDCW` and is deleted exactly
        // once, here.
        let _ = unsafe { DeleteDC(self.hdc) };
    }
}

/// A printer DC with a document open on it.
struct PrintDc {
    hdc: HDC,
    doc_open: std::cell::Cell<bool>,
}

impl PrintDc {
    fn open(name: &[u16], devmode: *const DEVMODEW) -> Result<Self, PrintError> {
        // SAFETY: `name` outlives the call; `devmode` is either null or a
        // driver-validated DEVMODE owned by the caller for the duration.
        let hdc = unsafe {
            CreateDCW(
                PCWSTR::null(),
                PCWSTR(name.as_ptr()),
                PCWSTR::null(),
                if devmode.is_null() {
                    None
                } else {
                    Some(devmode)
                },
            )
        };
        if hdc.is_invalid() {
            return Err(PrintError::NoSuchPrinter(String::from_utf16_lossy(
                &name[..name.len().saturating_sub(1)],
            )));
        }
        Ok(Self {
            hdc,
            doc_open: std::cell::Cell::new(false),
        })
    }

    fn metrics(&self) -> DcMetrics {
        DcMetrics::read(self.hdc)
    }

    fn start_doc(&self, title: &str) -> Result<i32, PrintError> {
        let name = wide(title);
        let info = DOCINFOW {
            cbSize: i32::try_from(std::mem::size_of::<DOCINFOW>()).unwrap_or(0),
            lpszDocName: PCWSTR(name.as_ptr()),
            lpszOutput: PCWSTR::null(),
            lpszDatatype: PCWSTR::null(),
            fwType: 0,
        };
        // SAFETY: `info` and the title buffer it points at both outlive the
        // call, and `hdc` is a live printer DC.
        let job = unsafe { StartDocW(self.hdc, &info) };
        if job <= 0 {
            return Err(PrintError::Spool("StartDoc failed".to_string()));
        }
        self.doc_open.set(true);
        Ok(job)
    }

    fn print_page(
        &self,
        raster: &crate::compose::SheetRaster,
        metrics: DcMetrics,
    ) -> Result<(), PrintError> {
        let dib = Dib::from_raster(raster)?;
        // SAFETY: `hdc` is a live printer DC with a document open.
        if unsafe { StartPage(self.hdc) } <= 0 {
            return Err(PrintError::Spool("StartPage failed".to_string()));
        }
        // The composed sheet spans the whole physical page, margins
        // included, and a printer DC's origin sits at the top-left of the
        // *printable* area — hence the negative destination corner.
        // SAFETY: `dib` owns both the header and the pixel bytes it points
        // at for the duration of the call, and its header describes exactly
        // that buffer.
        let copied = unsafe {
            StretchDIBits(
                self.hdc,
                -metrics.offset_x,
                -metrics.offset_y,
                metrics.physical_width,
                metrics.physical_height,
                0,
                0,
                dib.width,
                dib.height,
                Some(dib.bits.as_ptr().cast()),
                &dib.info,
                DIB_RGB_COLORS,
                SRCCOPY,
            )
        };
        // SAFETY: as above; the page is ended whether or not the blit
        // reported rows, so the document stays well-formed.
        let ended = unsafe { EndPage(self.hdc) };
        if copied == 0 {
            return Err(PrintError::Spool(
                "StretchDIBits copied no rows".to_string(),
            ));
        }
        if ended <= 0 {
            return Err(PrintError::Spool("EndPage failed".to_string()));
        }
        Ok(())
    }

    fn end_doc(&self) -> Result<(), PrintError> {
        // SAFETY: `hdc` is live and a document is open on it.
        let result = unsafe { EndDoc(self.hdc) };
        if result <= 0 {
            return Err(PrintError::Spool("EndDoc failed".to_string()));
        }
        self.doc_open.set(false);
        Ok(())
    }
}

impl Drop for PrintDc {
    fn drop(&mut self) {
        if self.doc_open.get() {
            // An error left the document open. Abort rather than committing
            // a partial job, including when EndDoc itself failed.
            // SAFETY: `hdc` is live and a document is open on it.
            let _ = unsafe { AbortDoc(self.hdc) };
        }
        // SAFETY: created by `CreateDCW`, deleted exactly once.
        let _ = unsafe { DeleteDC(self.hdc) };
    }
}

// ---------------------------------------------------------------------------
// DEVMODE
// ---------------------------------------------------------------------------

/// A driver-validated `DEVMODEW` carrying the job's duplex, copies, colour,
/// paper and orientation.
struct DevMode {
    buffer: AlignedBuffer,
}

impl DevMode {
    /// Ask the driver for its defaults, apply the job's options, and let the
    /// driver validate the result.
    ///
    /// Fail if the driver cannot validate the configuration. Falling back to
    /// defaults would invalidate the geometry and margins used for composition.
    fn for_job(name: &[u16], options: &PrintOptions, landscape: bool) -> Result<Self, PrintError> {
        let printer = PrinterHandle::open(name)?;
        // SAFETY: `printer` is a live handle and `name` outlives the call;
        // null in/out DEVMODE pointers with fMode 0 request the size only.
        let needed = unsafe {
            DocumentPropertiesW(
                None::<HWND>,
                printer.handle,
                PCWSTR(name.as_ptr()),
                None,
                None,
                0,
            )
        };
        if needed < std::mem::size_of::<DEVMODEW>() as i32 {
            return Err(PrintError::Spool(
                "DocumentProperties returned an invalid DEVMODE size".into(),
            ));
        }

        let size = needed as usize;
        let mut defaults = AlignedBuffer::new(size);
        // SAFETY: `defaults` is at least `needed` bytes and aligned for
        // DEVMODEW; DM_OUT_BUFFER asks the driver to fill it.
        let ok = unsafe {
            DocumentPropertiesW(
                None::<HWND>,
                printer.handle,
                PCWSTR(name.as_ptr()),
                Some(defaults.as_mut_ptr().cast::<DEVMODEW>()),
                None,
                DM_OUT_BUFFER.0,
            )
        };
        if ok != 1 {
            // IDOK
            return Err(PrintError::Spool(
                "DocumentProperties could not read printer defaults".into(),
            ));
        }

        // SAFETY: the driver just wrote a DEVMODEW at the front of the
        // buffer, and the buffer is aligned for it.
        let devmode = unsafe { &mut *defaults.as_mut_ptr().cast::<DEVMODEW>() };
        apply_options(devmode, options, landscape);

        let mut merged = AlignedBuffer::new(size);
        // SAFETY: both buffers are at least `needed` bytes and aligned;
        // DM_IN_BUFFER|DM_OUT_BUFFER merges the edited DEVMODE into a
        // driver-validated one.
        let ok = unsafe {
            DocumentPropertiesW(
                None::<HWND>,
                printer.handle,
                PCWSTR(name.as_ptr()),
                Some(merged.as_mut_ptr().cast::<DEVMODEW>()),
                Some(defaults.as_ptr().cast::<DEVMODEW>()),
                DM_IN_BUFFER.0 | DM_OUT_BUFFER.0,
            )
        };
        if ok != 1 {
            // IDOK
            return Err(PrintError::Spool(
                "DocumentProperties could not validate print settings".into(),
            ));
        }
        Ok(Self { buffer: merged })
    }

    fn as_ptr(&self) -> *const DEVMODEW {
        self.buffer.as_ptr().cast::<DEVMODEW>()
    }
}

fn apply_options(devmode: &mut DEVMODEW, options: &PrintOptions, landscape: bool) {
    let mut fields = devmode.dmFields;

    devmode.dmDuplex = match options.duplex {
        Duplex::None => DMDUP_SIMPLEX,
        // "Vertical" is the flip axis running down the sheet, which is the
        // long edge of a portrait page: the usual book binding.
        Duplex::LongEdge => DMDUP_VERTICAL,
        Duplex::ShortEdge => DMDUP_HORIZONTAL,
    };
    fields = fields | DM_DUPLEX;

    devmode.dmColor = if options.grayscale {
        DMCOLOR_MONOCHROME
    } else {
        DMCOLOR_COLOR
    };
    fields = fields | DM_COLOR;

    devmode.dmCollate = if options.collate {
        DMCOLLATE_TRUE
    } else {
        DMCOLLATE_FALSE
    };
    fields = fields | DM_COLLATE;

    // SAFETY: `Anonymous1.Anonymous1` is the printer arm of the DEVMODE
    // union — the only meaningful one for a printer DEVMODE, which is what
    // `DocumentProperties` on a print queue returns.
    let printer_fields = unsafe { &mut devmode.Anonymous1.Anonymous1 };
    printer_fields.dmCopies = i16::try_from(options.copies).unwrap_or(i16::MAX);
    fields = fields | DM_COPIES;

    printer_fields.dmOrientation = if landscape {
        i16::try_from(DMORIENT_LANDSCAPE).unwrap_or(2)
    } else {
        i16::try_from(DMORIENT_PORTRAIT).unwrap_or(1)
    };
    fields = fields | DM_ORIENTATION;

    // DEVMODE paper is always the *portrait* form; orientation does the
    // turning, so the named size is used as-is.
    let (paper_code, custom) = paper_code(options.paper);
    printer_fields.dmPaperSize = paper_code;
    if let Some((width_tenths_mm, height_tenths_mm)) = custom {
        printer_fields.dmPaperWidth = width_tenths_mm;
        printer_fields.dmPaperLength = height_tenths_mm;
        fields |= DM_PAPERWIDTH | DM_PAPERLENGTH;
    } else {
        fields &= !(DM_PAPERWIDTH | DM_PAPERLENGTH);
    }
    fields = fields | DM_PAPERSIZE;

    devmode.dmFields = fields;
}

/// The `DMPAPER_*` code for a paper size, plus custom dimensions in tenths of
/// a millimetre when there is no named code.
fn paper_code(paper: PaperSize) -> (i16, Option<(i16, i16)>) {
    let named = match paper {
        PaperSize::A3 => Some(DMPAPER_A3),
        PaperSize::A4 => Some(DMPAPER_A4),
        PaperSize::A5 => Some(DMPAPER_A5),
        PaperSize::B5 => Some(DMPAPER_B5),
        PaperSize::Letter => Some(DMPAPER_LETTER),
        PaperSize::Legal => Some(DMPAPER_LEGAL),
        PaperSize::Tabloid => Some(DMPAPER_TABLOID),
        PaperSize::Executive => Some(DMPAPER_EXECUTIVE),
        // A6 has no DMPAPER code in the base set, so it goes through the
        // custom path alongside `PaperSize::Custom`.
        PaperSize::A6 | PaperSize::Custom { .. } => None,
    };
    if let Some(code) = named {
        return (i16::try_from(code).unwrap_or(0), None);
    }
    let (w_pt, h_pt) = paper.size();
    let tenths = |pt: f64| {
        let mm10 = pt / crate::paper::POINTS_PER_MM * 10.0;
        i16::try_from(mm10.round() as i64).unwrap_or(i16::MAX)
    };
    (
        i16::try_from(DMPAPER_USER).unwrap_or(256),
        Some((tenths(w_pt), tenths(h_pt))),
    )
}

// ---------------------------------------------------------------------------
// Printer handle, job status
// ---------------------------------------------------------------------------

struct PrinterHandle {
    handle: PRINTER_HANDLE,
}

impl PrinterHandle {
    fn open(name: &[u16]) -> Result<Self, PrintError> {
        let mut handle = PRINTER_HANDLE::default();
        // SAFETY: `name` is NUL-terminated and outlives the call; `handle`
        // is a valid out-parameter and a null default is "read access".
        unsafe { OpenPrinterW(PCWSTR(name.as_ptr()), &mut handle, None) }.map_err(|e| {
            PrintError::NoSuchPrinter(format!(
                "{}: {e}",
                String::from_utf16_lossy(&name[..name.len().saturating_sub(1)])
            ))
        })?;
        Ok(Self { handle })
    }

    /// The status of job `id` in this queue, or `None` if it is not here.
    fn job_status(&self, id: u32) -> Result<Option<JobStatus>, PrintError> {
        use windows::Win32::Graphics::Printing::EnumJobsW;

        let mut needed = 0u32;
        let mut returned = 0u32;
        // SAFETY: the sizing call writes only the two out-pointers and is
        // expected to fail with ERROR_INSUFFICIENT_BUFFER.
        let _ = unsafe {
            EnumJobsW(
                self.handle,
                0,
                u32::MAX,
                1,
                None,
                &mut needed,
                &mut returned,
            )
        };
        if needed == 0 {
            return Ok(None);
        }
        let mut buffer = AlignedBuffer::new(needed as usize);
        // SAFETY: the buffer is at least `needed` bytes and aligned for
        // JOB_INFO_1W; the out-pointers are valid locals.
        if unsafe {
            EnumJobsW(
                self.handle,
                0,
                u32::MAX,
                1,
                Some(buffer.as_bytes_mut()),
                &mut needed,
                &mut returned,
            )
        }
        .is_err()
        {
            return Ok(None);
        }
        for index in 0..returned as usize {
            // SAFETY: the spooler wrote `returned` JOB_INFO_1W records into
            // the front of the buffer, which outlives this loop.
            let job = unsafe { buffer.as_ptr().cast::<JOB_INFO_1W>().add(index).read() };
            if job.JobId == id {
                return Ok(Some(job_status_from_bits(job.Status)));
            }
        }
        Ok(None)
    }

    fn cancel(&self, id: u32) -> Result<(), PrintError> {
        // SAFETY: `self.handle` is live; level 0 with a null job info and a
        // command is the documented way to control a job.
        let ok = unsafe { SetJobW(self.handle, id, 0, None, JOB_CONTROL_CANCEL) };
        if ok.as_bool() {
            Ok(())
        } else {
            Err(PrintError::Spool(format!("could not cancel job {id}")))
        }
    }
}

impl Drop for PrinterHandle {
    fn drop(&mut self) {
        // SAFETY: opened by `OpenPrinterW`, closed exactly once.
        let _ = unsafe { ClosePrinter(self.handle) };
    }
}

fn job_status_from_bits(bits: u32) -> JobStatus {
    if bits & (JOB_STATUS_DELETED | JOB_STATUS_DELETING) != 0 {
        JobStatus::Cancelled
    } else if bits & JOB_STATUS_ERROR != 0 {
        JobStatus::Aborted
    } else if bits & JOB_STATUS_PRINTED != 0 {
        JobStatus::Completed
    } else if bits & JOB_STATUS_PRINTING != 0 {
        JobStatus::Processing
    } else if bits & JOB_STATUS_PAUSED != 0 {
        JobStatus::Pending
    } else {
        JobStatus::Pending
    }
}

// ---------------------------------------------------------------------------
// DIB
// ---------------------------------------------------------------------------

/// A top-down 24-bit BGR DIB, ready for `StretchDIBits`.
struct Dib {
    info: BITMAPINFO,
    bits: Vec<u8>,
    width: i32,
    height: i32,
}

impl Dib {
    fn from_raster(raster: &crate::compose::SheetRaster) -> Result<Self, PrintError> {
        let width = i32::try_from(raster.width)
            .map_err(|_| PrintError::Spool("sheet is wider than a DIB allows".to_string()))?;
        let height = i32::try_from(raster.height)
            .map_err(|_| PrintError::Spool("sheet is taller than a DIB allows".to_string()))?;
        if width == 0 || height == 0 {
            return Err(PrintError::Spool("sheet raster is empty".to_string()));
        }

        // DIB rows are DWORD-aligned, and GDI wants BGR rather than RGB.
        let stride = (raster.width as usize * 3).div_ceil(4) * 4;
        let mut bits = vec![0u8; stride * raster.height as usize];
        let channels = raster.channels as usize;
        for y in 0..raster.height as usize {
            let src = &raster.pixels[y * raster.width as usize * channels..];
            let dst = &mut bits[y * stride..];
            for x in 0..raster.width as usize {
                let (r, g, b) = match channels {
                    1 => {
                        let v = src[x];
                        (v, v, v)
                    }
                    3 | 4 => (
                        src[x * channels],
                        src[x * channels + 1],
                        src[x * channels + 2],
                    ),
                    other => {
                        return Err(PrintError::Spool(format!(
                            "cannot blit a {other}-channel sheet raster"
                        )));
                    }
                };
                dst[x * 3] = b;
                dst[x * 3 + 1] = g;
                dst[x * 3 + 2] = r;
            }
        }

        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: u32::try_from(std::mem::size_of::<BITMAPINFOHEADER>()).unwrap_or(40),
            biWidth: width,
            // Negative height means top-down, which is how the composer
            // hands sheets over.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 24,
            biCompression: BI_RGB.0,
            biSizeImage: u32::try_from(bits.len()).unwrap_or(0),
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };

        Ok(Self {
            info,
            bits,
            width,
            height,
        })
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// A pointer-aligned byte buffer, which the `Enum*`/`DocumentProperties`
/// families require: they write structs full of pointers into it, and a plain
/// `Vec<u8>` is only byte-aligned.
struct AlignedBuffer {
    words: Vec<usize>,
    len: usize,
}

impl AlignedBuffer {
    fn new(bytes: usize) -> Self {
        let words = bytes.div_ceil(std::mem::size_of::<usize>()).max(1);
        Self {
            words: vec![0usize; words],
            len: words * std::mem::size_of::<usize>(),
        }
    }

    fn as_bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: `usize` has no padding or invalid bit patterns, so its
        // backing store is a valid `[u8]` of the same extent.
        unsafe { std::slice::from_raw_parts_mut(self.words.as_mut_ptr().cast::<u8>(), self.len) }
    }

    fn as_ptr(&self) -> *const u8 {
        self.words.as_ptr().cast::<u8>()
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.words.as_mut_ptr().cast::<u8>()
    }
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copy a spooler-owned `PWSTR` into an owned `String`.
///
/// # Safety
///
/// `ptr` must be null or point at a NUL-terminated UTF-16 string that stays
/// alive for the duration of the call.
unsafe fn pwstr_to_string(ptr: PWSTR) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees a live NUL-terminated string.
    unsafe { ptr.to_string() }.unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn letter_metrics() -> DcMetrics {
        DcMetrics {
            physical_width: 2550,
            physical_height: 6600,
            offset_x: 75,
            offset_y: 50,
            printable_width: 2350,
            printable_height: 6450,
            dpi_x: 300,
            dpi_y: 600,
        }
    }

    #[test]
    fn hardware_margins_use_the_configured_dcs_axes_and_resolution() {
        let portrait = letter_metrics();
        portrait.validate_paper(PaperSize::Letter, false).unwrap();
        assert_eq!(
            portrait.hardware_margins(),
            Margins {
                left: 18.0,
                right: 30.0,
                top: 6.0,
                bottom: 12.0,
            }
        );
        let landscape = DcMetrics {
            physical_width: 3300,
            physical_height: 5100,
            offset_x: 125,
            offset_y: 150,
            printable_width: 3150,
            printable_height: 4800,
            ..portrait
        };
        landscape.validate_paper(PaperSize::Letter, true).unwrap();
        assert_eq!(
            landscape.hardware_margins(),
            Margins {
                left: 30.0,
                right: 6.0,
                top: 18.0,
                bottom: 18.0,
            }
        );
    }

    #[test]
    fn driver_geometry_must_match_requested_paper_and_orientation() {
        let metrics = letter_metrics();
        assert!(metrics.validate_paper(PaperSize::A4, false).is_err());
        assert!(metrics.validate_paper(PaperSize::Letter, true).is_err());
        assert!(
            DcMetrics {
                dpi_x: 0,
                ..metrics
            }
            .validate_paper(PaperSize::Letter, false)
            .is_err()
        );
        // Whole-device-pixel rounding is tolerated.
        DcMetrics {
            physical_width: 2551,
            ..metrics
        }
        .validate_paper(PaperSize::Letter, false)
        .unwrap();
    }

    #[test]
    fn devmode_sets_orientation_and_custom_media_dimension_flags() {
        let mut devmode = DEVMODEW::default();
        let options = PrintOptions {
            paper: PaperSize::Custom {
                width: 360.0,
                height: 720.0,
            },
            ..PrintOptions::default()
        };
        apply_options(&mut devmode, &options, true);
        assert!(
            devmode
                .dmFields
                .contains(DM_ORIENTATION | DM_PAPERWIDTH | DM_PAPERLENGTH)
        );
        // SAFETY: apply_options populated the printer arm of this DEVMODE.
        let fields = unsafe { &devmode.Anonymous1.Anonymous1 };
        assert_eq!(i32::from(fields.dmOrientation), DMORIENT_LANDSCAPE as i32);
        assert_eq!((fields.dmPaperWidth, fields.dmPaperLength), (1270, 2540));
        apply_options(&mut devmode, &PrintOptions::default(), false);
        assert_eq!(devmode.dmFields.0 & (DM_PAPERWIDTH | DM_PAPERLENGTH).0, 0);
    }

    #[test]
    fn direct_submission_requires_one_sheet_geometry() {
        let sheet = |orientation| {
            let bounds = PaperSize::A4.rect(orientation);
            Sheet {
                index: 0,
                bounds,
                imageable: bounds,
                side: crate::Side::Front,
                placements: vec![],
            }
        };
        let portrait = sheet(Orientation::Portrait);
        let landscape = sheet(Orientation::Landscape);
        let options = PrintOptions::default();
        assert!(!sheet_orientation(&[portrait.clone(), portrait.clone()], &options).unwrap());
        assert!(sheet_orientation(std::slice::from_ref(&landscape), &options).unwrap());
        assert!(matches!(
            sheet_orientation(&[portrait, landscape], &options),
            Err(PrintError::InvalidOptions(_))
        ));
        assert!(matches!(
            sheet_orientation(&[], &options),
            Err(PrintError::EmptyRange)
        ));
        // The DEVMODE choice follows declared paper axes, including custom
        // media that is square or whose nominal width exceeds its height.
        for paper in [
            PaperSize::Custom {
                width: 720.0,
                height: 360.0,
            },
            PaperSize::Custom {
                width: 360.0,
                height: 360.0,
            },
        ] {
            for orientation in [Orientation::Portrait, Orientation::Landscape] {
                let options = PrintOptions {
                    paper,
                    orientation,
                    ..PrintOptions::default()
                };
                let bounds = paper.rect(orientation);
                let sheet = Sheet {
                    index: 0,
                    bounds,
                    imageable: bounds,
                    side: crate::Side::Front,
                    placements: vec![],
                };
                assert_eq!(
                    sheet_orientation(&[sheet], &options).unwrap(),
                    orientation == Orientation::Landscape
                );
            }
        }
    }
}
