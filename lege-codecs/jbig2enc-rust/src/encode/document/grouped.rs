use super::resources::bitmap_bytes;
use super::{EncoderMetrics, Jbig2Encoder, PdfSplitOutput};
use crate::{Jbig2Config, jbig2sym::BitImage};
use anyhow::{Result, bail};

/// Bounds one independently encoded PDF dictionary group. These bounds cover
/// source-page retention, not total process RSS; symbols and trial streams also
/// consume memory. No device-specific default is implied.
#[derive(Debug, Clone, Copy)]
pub struct PdfGroupOptions {
    pub max_pages: usize,
    pub max_source_bitmap_bytes: Option<usize>,
}

/// Page streams in this value all refer exclusively to this value's globals.
/// `first_page` is zero-based in the caller's input sequence. Segment numbers
/// and page association 1 may repeat across independently embedded PDF images.
#[derive(Debug)]
pub struct EncodedPdfGroup {
    pub group_index: usize,
    pub first_page: usize,
    pub source_bitmap_bytes: usize,
    pub output: PdfSplitOutput,
    pub metrics: EncoderMetrics,
}

/// Incremental bounded-group encoder. The sink can write each group's globals
/// and page streams directly to a PDF, retaining the appropriate globals object
/// for those pages. No prior groups or source pages are kept by this encoder.
///
/// Sink or encoding errors are terminal: a sink may already have written data,
/// so retrying it implicitly could duplicate PDF objects. Errors propagate.
pub struct PdfGroupEncoder<'a, F: FnMut(EncodedPdfGroup) -> Result<()>> {
    config: &'a Jbig2Config,
    options: PdfGroupOptions,
    sink: F,
    encoder: Option<Jbig2Encoder<'a>>,
    pages: usize,
    bytes: usize,
    first_page: usize,
    group_index: usize,
    failed: bool,
}

impl<'a, F: FnMut(EncodedPdfGroup) -> Result<()>> PdfGroupEncoder<'a, F> {
    pub fn new(config: &'a Jbig2Config, options: PdfGroupOptions, sink: F) -> Result<Self> {
        if options.max_pages == 0 || options.max_source_bitmap_bytes == Some(0) {
            bail!("PDF group limits must be positive");
        }
        config
            .validate_exact_symbol_mode()
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            config,
            options,
            sink,
            encoder: None,
            pages: 0,
            bytes: 0,
            first_page: 0,
            group_index: 0,
            failed: false,
        })
    }

    pub fn push_page(&mut self, image: BitImage) -> Result<()> {
        if self.failed {
            bail!("PDF group encoder is in a failed state");
        }
        let bytes = bitmap_bytes(&image);
        if self
            .options
            .max_source_bitmap_bytes
            .is_some_and(|limit| bytes > limit)
        {
            // Reject before flushing or otherwise mutating the accepted prefix.
            bail!("source page alone exceeds the group bitmap limit");
        }
        let combined = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| anyhow::anyhow!("source bitmap byte count overflow"))?;
        if self.pages >= self.options.max_pages
            || self
                .options
                .max_source_bitmap_bytes
                .is_some_and(|limit| combined > limit)
        {
            self.emit()?;
        }
        let encoder = self
            .encoder
            .get_or_insert_with(|| Jbig2Encoder::new(self.config));
        if let Err(error) = encoder.add_page_bitimage(image) {
            self.failed = true;
            return Err(error);
        }
        self.pages += 1;
        self.bytes += bytes;
        // Emit immediately at the page cap, so a caller can release a completed
        // group without providing an additional page or retaining a lookahead.
        if self.pages == self.options.max_pages {
            self.emit()?;
        }
        Ok(())
    }

    fn emit(&mut self) -> Result<()> {
        if self.failed {
            bail!("PDF group encoder is in a failed state");
        }
        let Some(mut encoder) = self.encoder.take() else {
            return Ok(());
        };
        self.failed = true;
        let output = encoder.flush_pdf_split()?;
        let metrics = encoder.metrics_snapshot();
        drop(encoder); // Release source pages, symbols and instance bitmaps before the sink.
        (self.sink)(EncodedPdfGroup {
            group_index: self.group_index,
            first_page: self.first_page,
            source_bitmap_bytes: self.bytes,
            output,
            metrics,
        })?;
        self.first_page += self.pages;
        self.pages = 0;
        self.bytes = 0;
        self.group_index += 1;
        self.failed = false;
        Ok(())
    }

    /// Flush the final partial group. Empty input emits nothing. Dropping this
    /// value does not flush; callers must handle this explicit result.
    pub fn finish(mut self) -> Result<()> {
        self.emit()
    }
}
