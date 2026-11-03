use crate::j2k::decode_markers::{CodeBlockStyle, ProgressionOrder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    Gray,
    #[doc(hidden)]
    Rgb,
    Srgb,
    /// 4-component subtractive CMYK (JP2 EnumCS 12). Decoded as four
    /// independent planes (no multiple-component transform); the consumer maps
    /// C/M/Y/K to output.
    Cmyk,
    #[doc(hidden)]
    Yuv,
    #[doc(hidden)]
    YCbCr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IccComponentModel {
    Gray,
    Rgb,
}

/// Explicit interpretation of decoded component samples in a JP2 container.
///
/// ICC profiles are embedded unchanged; the encoder does not color-convert the
/// supplied samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorEncoding {
    Gray,
    Srgb,
    /// Enumerated CMYK (JP2 EnumCS 12).
    Cmyk,
    IccProfile {
        bytes: Vec<u8>,
        component_model: IccComponentModel,
    },
}

impl ColorEncoding {
    /// Describe the samples with an ICC profile embedded by the JP2 Restricted
    /// ICC method. The profile must be a monochrome or three-component
    /// matrix-shaper profile (grey TRC, or RGB colorant XYZ columns plus TRCs,
    /// input or display class, no `A2B0` lookup table); anything else needs
    /// the JPX any-ICC method, which this crate does not write.
    ///
    /// # Errors
    ///
    /// [`crate::error::Jp2LamError::InvalidInput`] for a profile that is not a
    /// well-formed restricted profile of `component_model`.
    pub fn restricted_icc(
        bytes: Vec<u8>,
        component_model: IccComponentModel,
    ) -> crate::error::Result<Self> {
        validate_restricted_icc(&bytes, component_model)?;
        Ok(Self::IccProfile {
            bytes,
            component_model,
        })
    }

    pub(crate) fn validate_for(self: &Self, colorspace: ColorSpace) -> crate::error::Result<()> {
        match (colorspace, self) {
            (ColorSpace::Gray, Self::Gray)
            | (ColorSpace::Cmyk, Self::Cmyk)
            | (ColorSpace::Srgb | ColorSpace::Rgb, Self::Srgb)
            | (
                ColorSpace::Gray,
                Self::IccProfile {
                    component_model: IccComponentModel::Gray,
                    ..
                },
            )
            | (
                ColorSpace::Srgb | ColorSpace::Rgb,
                Self::IccProfile {
                    component_model: IccComponentModel::Rgb,
                    ..
                },
            ) => {}
            _ => {
                return Err(invalid_input(
                    "color encoding is incompatible with the image component model",
                ));
            }
        }
        if let Self::IccProfile {
            bytes,
            component_model,
        } = self
        {
            validate_restricted_icc(bytes, *component_model)?;
        }
        Ok(())
    }
}

const MAX_ICC_PROFILE_BYTES: usize = 16 * 1024 * 1024;

/// Encoder-side check for the JP2 Restricted ICC method (ISO/IEC 15444-1
/// I.5.3.3): the header checks of [`validate_icc_header`], plus the profile
/// must be a Monochrome or Three-Component Matrix-Based profile - an input
/// (`scnr`) or display (`mntr`) class profile whose transform is the grey TRC,
/// or the three colorant XYZ columns and three TRCs, with no LUT-based
/// `A2B0` transform that a full ICC reader would apply instead of the matrix.
///
/// The encoder embeds the profile unchanged and never converts samples, so a
/// profile that a restricted JP2 reader cannot interpret would silently
/// describe the pixels wrongly; it is refused here instead.
pub(crate) fn validate_restricted_icc(
    bytes: &[u8],
    model: IccComponentModel,
) -> crate::error::Result<()> {
    validate_icc_header(bytes, model)?;
    let class = &bytes[12..16];
    if class != b"scnr" && class != b"mntr" {
        return Err(invalid_input(format!(
            "restricted JP2 ICC profile class must be input ('scnr') or display ('mntr'), got '{}'",
            String::from_utf8_lossy(class)
        )));
    }
    let tags = icc_tag_table(bytes)?;
    let find = |signature: &[u8; 4]| tags.iter().find(|tag| &tag.signature == signature);
    if find(b"A2B0").is_some() {
        return Err(invalid_input(
            "ICC profile carries an A2B0 lookup table; restricted JP2 accepts matrix-shaper \
             profiles only",
        ));
    }
    let (curves, colorants): (&[&[u8; 4]], &[&[u8; 4]]) = match model {
        IccComponentModel::Gray => (&[b"kTRC"], &[]),
        IccComponentModel::Rgb => (&[b"rTRC", b"gTRC", b"bTRC"], &[b"rXYZ", b"gXYZ", b"bXYZ"]),
    };
    for signature in curves {
        let tag = find(signature).ok_or_else(|| {
            invalid_input(format!(
                "restricted JP2 ICC profile lacks the '{}' tone curve of a matrix-shaper profile",
                String::from_utf8_lossy(*signature)
            ))
        })?;
        let data = tag.data(bytes);
        let ok = match data.get(0..4) {
            Some(b"curv") => data
                .get(8..12)
                .map(|count| u32::from_be_bytes(count.try_into().expect("four bytes")))
                .is_some_and(|count| {
                    (count as usize)
                        .checked_mul(2)
                        .and_then(|len| len.checked_add(12))
                        .is_some_and(|len| len <= data.len())
                }),
            Some(b"para") => data
                .get(8..10)
                .map(|kind| u16::from_be_bytes(kind.try_into().expect("two bytes")))
                .and_then(|kind| [4usize, 12, 16, 20, 28].get(usize::from(kind)).copied())
                .is_some_and(|params| 12 + params <= data.len()),
            _ => false,
        };
        if !ok {
            return Err(invalid_input(format!(
                "ICC '{}' tag is not a well-formed 'curv' or 'para' curve",
                String::from_utf8_lossy(*signature)
            )));
        }
    }
    for signature in colorants {
        let tag = find(signature).ok_or_else(|| {
            invalid_input(format!(
                "restricted JP2 ICC profile lacks the '{}' colorant of a matrix-shaper profile",
                String::from_utf8_lossy(*signature)
            ))
        })?;
        let data = tag.data(bytes);
        if data.get(0..4) != Some(b"XYZ ".as_slice()) || data.len() < 20 {
            return Err(invalid_input(format!(
                "ICC '{}' tag is not a well-formed 'XYZ ' colorant",
                String::from_utf8_lossy(*signature)
            )));
        }
    }
    Ok(())
}

struct IccTag {
    signature: [u8; 4],
    offset: usize,
    size: usize,
}

impl IccTag {
    fn data<'a>(&self, profile: &'a [u8]) -> &'a [u8] {
        &profile[self.offset..self.offset + self.size]
    }
}

/// The ICC tag table (ICC.1 7.3): a count at byte 128, then 12-byte
/// signature/offset/size entries, every one inside the profile.
fn icc_tag_table(bytes: &[u8]) -> crate::error::Result<Vec<IccTag>> {
    let read_u32 = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|word| u32::from_be_bytes(word.try_into().expect("four bytes")) as usize)
            .ok_or_else(|| invalid_input("ICC tag table is truncated"))
    };
    let count = read_u32(128)?;
    let table_end = count
        .checked_mul(12)
        .and_then(|len| len.checked_add(132))
        .filter(|&end| end <= bytes.len())
        .ok_or_else(|| invalid_input("ICC tag table is truncated"))?;
    (132..table_end)
        .step_by(12)
        .map(|entry| {
            let offset = read_u32(entry + 4)?;
            let size = read_u32(entry + 8)?;
            if offset.checked_add(size).is_none_or(|end| end > bytes.len()) {
                return Err(invalid_input("ICC tag data lies outside the profile"));
            }
            Ok(IccTag {
                signature: bytes[entry..entry + 4].try_into().expect("four bytes"),
                offset,
                size,
            })
        })
        .collect()
}

/// The ICC header fields a JP2 reader relies on: size, `acsp` signature, an
/// XYZ connection space and a data colour space matching the component model.
/// Decoding accepts any profile that passes this; encoding additionally
/// requires [`validate_restricted_icc`].
pub(crate) fn validate_icc_header(
    bytes: &[u8],
    model: IccComponentModel,
) -> crate::error::Result<()> {
    if !(128..=MAX_ICC_PROFILE_BYTES).contains(&bytes.len()) {
        return Err(invalid_input(format!(
            "restricted ICC profile length {} is outside 128..={MAX_ICC_PROFILE_BYTES}",
            bytes.len()
        )));
    }
    let declared_len = u32::from_be_bytes(bytes[0..4].try_into().expect("four-byte ICC length"));
    if declared_len as usize != bytes.len() {
        return Err(invalid_input(
            "ICC header size does not match profile bytes",
        ));
    }
    if &bytes[36..40] != b"acsp" {
        return Err(invalid_input("ICC profile signature is not 'acsp'"));
    }
    if &bytes[20..24] != b"XYZ " {
        return Err(invalid_input(
            "restricted JP2 ICC profile PCS must be 'XYZ '",
        ));
    }
    let expected_space = match model {
        IccComponentModel::Gray => b"GRAY".as_slice(),
        IccComponentModel::Rgb => b"RGB ".as_slice(),
    };
    if &bytes[16..20] != expected_space {
        return Err(invalid_input(
            "ICC data color space does not match the declared component model",
        ));
    }
    Ok(())
}

impl ColorSpace {
    pub fn encoding_domain(self) -> Self {
        match self {
            Self::Gray => Self::Gray,
            Self::Cmyk => Self::Cmyk,
            Self::Rgb | Self::Srgb | Self::Yuv | Self::YCbCr => Self::Srgb,
        }
    }

    pub fn component_count(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::Cmyk => 4,
            Self::Rgb | Self::Srgb | Self::Yuv | Self::YCbCr => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Jp2,
    J2k,
}

/// Validated meaningful sample precision supported by the native encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SamplePrecision(u8);

impl SamplePrecision {
    pub const MIN: u8 = 8;
    pub const MAX: u8 = 16;

    pub fn new(bits: u32) -> crate::error::Result<Self> {
        let bits = u8::try_from(bits).map_err(|_| {
            invalid_input(format!(
                "component precision {bits} is outside the supported 8..=16 range"
            ))
        })?;
        if !(Self::MIN..=Self::MAX).contains(&bits) {
            return Err(invalid_input(format!(
                "component precision {bits} is outside the supported 8..=16 range"
            )));
        }
        Ok(Self(bits))
    }

    pub const fn bits(self) -> u32 {
        self.0 as u32
    }

    pub const fn bits_u8(self) -> u8 {
        self.0
    }

    pub const fn unsigned_level_shift(self) -> i32 {
        1i32 << (self.0 - 1)
    }

    pub const fn unsigned_max(self) -> i32 {
        (1i32 << self.0) - 1
    }
}

impl TryFrom<u32> for SamplePrecision {
    type Error = crate::error::Jp2LamError;

    fn try_from(bits: u32) -> Result<Self, Self::Error> {
        Self::new(bits)
    }
}

/// High-resolution tile selection policy.
///
/// `Single` encodes the image as one tile. `Fixed` selects a regular tile grid.
/// `Auto` keeps one tile unless something requires more: a
/// [`ResourceLimits::max_working_memory`] the whole image does not fit under
/// (then the largest power-of-two edge from 4096 down that does), or an image
/// wider or taller than 32768 samples, which one precinct per resolution
/// cannot cover. The wavelet does not cross tile borders, so every extra tile
/// edge is a possible seam at low rates.
///
/// Every policy is rejected when a tile would straddle a multiple of 32768
/// samples, for the same precinct reason; `Single` therefore refuses images
/// larger than that, and `Fixed` needs a tile size that divides 32768 on such
/// images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TilePolicy {
    #[default]
    Single,
    Fixed {
        width: u32,
        height: u32,
    },
    Auto,
}

/// Multiple-component transform (ISO/IEC 15444-1 Annex G) for three-component
/// input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComponentTransform {
    /// The irreversible colour transform for lossy RGB. Lossless RGB is coded
    /// without the reversible colour transform for now (COD signals none): the
    /// encoder writes one QCD for all components, and RCT chroma needs its own
    /// bit-plane range. Gray input has nothing to transform.
    #[default]
    Auto,
    /// Code the components exactly as given and signal no transform in COD.
    /// Use this for samples that are already decorrelated (YCbCr, XYZ) so they
    /// are not transformed a second time. Lossy coding of correlated RGB this
    /// way costs bytes; it is a fidelity knob, not a speed one.
    None,
}

/// Codestream coding parameters. Every field defaults to "let the encoder
/// choose", which is what the encoder did before these were exposed.
///
/// Fields that are not here are fixed by the encoder and are legal Part 1
/// choices: one precinct per subband (Scod precinct bit clear, so 2^15
/// precincts), 2 guard bits, a zero image and tile origin, one tile-part per
/// tile, no SOP/EPH, ROI, POC, TLM or PLT markers, and Rsiz 0.
/// How many quality layers a tile's packets are split into (Annex B.6).
///
/// The final layer always completes the stream the rate control chose; the
/// layers below it are rate-distortion-optimal prefixes of that same stream,
/// so a reader that stops after layer *k* sees the best picture those bytes
/// can give. Every layer costs a packet header per (resolution, component),
/// a few bytes each.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum QualityLayers {
    /// One layer: the whole stream in one packet per (resolution, component).
    #[default]
    One,
    /// `n` layers (1..=65535). Each intermediate layer holds about half the
    /// bytes of the next, so `Count(4)` places layer boundaries near 1/8, 1/4
    /// and 1/2 of the final size.
    Count(u16),
    /// Explicit compression ratios (source bits / output bits) for the layers
    /// below the final one, most compressed first and strictly descending,
    /// as OpenJPEG's `-r` list reads them. A ratio the final stream already
    /// beats gives an empty layer.
    Ratios(Vec<f32>),
}

impl QualityLayers {
    /// Total layers written to COD.
    pub fn count(&self) -> u16 {
        match self {
            Self::One => 1,
            Self::Count(n) => (*n).max(1),
            Self::Ratios(ratios) => {
                ratios.len().saturating_add(1).min(usize::from(u16::MAX)) as u16
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CodingOptions {
    /// Number of wavelet decomposition levels (`NL`, 0..=32); one fewer than
    /// the number of resolutions. `None` derives it from the image: enough
    /// levels to leave a low-pass band of roughly 16-32 pixels, at most 5 for
    /// gray and 6 for colour. An explicit value must leave every tile's
    /// low-pass band non-empty or the encode is rejected.
    pub decomposition_levels: Option<u8>,
    /// Nominal code-block width and height. Each must be a power of two in
    /// 4..=1024 and their product at most 4096 (Annex A.6.1). `None` uses
    /// 64x64, or 32x32 for legacy approximate qualities below 30.
    pub code_block_size: Option<(u32, u32)>,
    /// Packet progression order written to COD and used to emit packets.
    /// With one precinct per subband the position loop is trivial, so the
    /// orders differ in how layer, resolution and component nest: LRCP puts
    /// every layer's low resolutions first (quality-progressive), RLCP and
    /// RPCL complete each resolution before the next, PCRL and CPRL complete
    /// each component.
    pub progression_order: ProgressionOrder,
    /// Whether three-component input gets the multiple-component transform.
    pub component_transform: ComponentTransform,
    /// Quality layers per tile; one by default.
    pub layers: QualityLayers,
    /// Code-block coding switches (bypass, reset, termall, causal,
    /// predictable termination, segmentation symbols); all off by default.
    pub code_block_style: CodeBlockStyle,
}

impl CodingOptions {
    pub(crate) fn validate(&self) -> crate::error::Result<()> {
        match &self.layers {
            QualityLayers::One => {}
            QualityLayers::Count(0) => {
                return Err(invalid_input("quality layer count must be at least 1"));
            }
            QualityLayers::Count(_) => {}
            QualityLayers::Ratios(ratios) => {
                if ratios.len() >= usize::from(u16::MAX) {
                    return Err(invalid_input("at most 65535 quality layers fit in COD"));
                }
                for pair in ratios.windows(2) {
                    if pair[1] >= pair[0] {
                        return Err(invalid_input(format!(
                            "quality layer ratios must be strictly descending, got {} then {}",
                            pair[0], pair[1]
                        )));
                    }
                }
                if let Some(bad) = ratios.iter().find(|r| !r.is_finite() || **r <= 0.0) {
                    return Err(invalid_input(format!(
                        "quality layer ratio must be finite and positive, got {bad}"
                    )));
                }
            }
        }
        if let Some(levels) = self.decomposition_levels
            && levels > 32
        {
            return Err(invalid_input(format!(
                "decomposition levels must be in 0..=32, got {levels}"
            )));
        }
        if let Some((width, height)) = self.code_block_size {
            for (label, value) in [("width", width), ("height", height)] {
                if !value.is_power_of_two() || !(4..=1024).contains(&value) {
                    return Err(invalid_input(format!(
                        "code-block {label} must be a power of two in 4..=1024, got {value}"
                    )));
                }
            }
            if u64::from(width) * u64::from(height) > 4096 {
                return Err(invalid_input(format!(
                    "code-block area {width}x{height} exceeds 4096 samples"
                )));
            }
        }
        Ok(())
    }
}

/// Optional resource bounds for an encode. Every field defaults to unbounded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResourceLimits {
    /// Encoder-owned transient memory per active tile, in bytes (borrowed
    /// source samples are excluded). Drives [`TilePolicy::Auto`] and decides
    /// whether components may be transformed concurrently and whether the
    /// perceptual search may cache unquantized wavelet planes.
    pub max_working_memory: Option<usize>,
    /// Worker threads for the whole encode. When set, the encode runs on its
    /// own Rayon pool of this size - DWT, Tier-1, rate search and metric work
    /// all included - instead of the caller's pool. Output does not depend on
    /// it. Component-level concurrency additionally needs a
    /// `max_working_memory` large enough for every component at once.
    pub max_threads: Option<usize>,
    /// RAM held for encoded code-block bytes before they spill to disk.
    pub encoded_store_memory_limit: Option<usize>,
    /// Where spilled code-block bytes go; the system temporary directory when
    /// `None`.
    pub spill_directory: Option<std::path::PathBuf>,
}

/// Named preset for convenience construction of [`EncodeOptions`].
///
/// Each preset maps to a quality value tuned for that scenario.
/// Use [`Preset::quality`] to get the underlying `u8` value, or pass a
/// `quality` directly in [`EncodeOptions`] for full 0–100 control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Scanned book pages destined for a PDF — compressed but fully readable.
    DocumentLow,
    /// Scanned book pages destined for a PDF — high fidelity, near-archival.
    DocumentHigh,
    /// Web-derived images (screenshots, web-rips) destined for a PDF — compact.
    WebLow,
    /// Web-derived images (screenshots, web-rips) destined for a PDF — crisp.
    WebHigh,
    /// Compact continuous-tone photographic output.
    PhotoCompact,
    /// High-fidelity continuous-tone photographic output.
    PhotoHigh,
    /// Near-lossless photographic output while retaining irreversible 9/7.
    PhotoNearLossless,
}

impl Preset {
    /// Quality value (0–100) associated with this preset.
    pub fn quality(self) -> u8 {
        match self {
            Self::DocumentLow => 30,
            Self::DocumentHigh => 85,
            Self::WebLow => 42,
            Self::WebHigh => 62,
            Self::PhotoCompact => 45,
            Self::PhotoHigh => 75,
            Self::PhotoNearLossless => 95,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Component {
    pub data: Vec<i32>,
    pub width: u32,
    pub height: u32,
    pub precision: u32,
    pub signed: bool,
    pub dx: u32,
    pub dy: u32,
}

/// An owned image: one [`Component`] per colour channel of `colorspace`,
/// optionally followed by one more component carrying opacity (straight,
/// not premultiplied). A trailing alpha component is coded as its own
/// codestream component, left out of any colour transform, and declared in
/// the JP2 file's `cdef` box; see [`Image::from_rgba_bytes`] and
/// [`Image::has_alpha`].
#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub components: Vec<Component>,
    pub colorspace: ColorSpace,
}

impl Image {
    /// Whether the last component is an opacity channel: one more component
    /// than `colorspace` has colour channels.
    pub fn has_alpha(&self) -> bool {
        self.components.len() == self.colorspace.component_count() + 1
    }
}

/// Borrowed component sample storage for high-resolution encoding views.
///
/// The public zero-copy photographic path starts with `u8` and `u16` storage.
/// The `I32` variant lets the existing owned [`Image`] API adapt into the same
/// view model without cloning its current compatibility representation.
#[derive(Debug, Clone, Copy)]
pub enum ComponentSampleData<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
    I32(&'a [i32]),
}

impl<'a> ComponentSampleData<'a> {
    fn len(self) -> usize {
        match self {
            Self::U8(samples) => samples.len(),
            Self::U16(samples) => samples.len(),
            Self::I32(samples) => samples.len(),
        }
    }

    fn sample_i32(self, index: usize) -> Option<i32> {
        match self {
            Self::U8(samples) => samples.get(index).map(|&sample| i32::from(sample)),
            Self::U16(samples) => samples.get(index).map(|&sample| i32::from(sample)),
            Self::I32(samples) => samples.get(index).copied(),
        }
    }

    fn storage_name(self) -> &'static str {
        match self {
            Self::U8(_) => "u8",
            Self::U16(_) => "u16",
            Self::I32(_) => "i32",
        }
    }
}

/// Borrowed view of one image component.
///
/// Strides and offsets are measured in samples of the backing storage, not in
/// bytes. This makes planar, interleaved, cropped, and padded inputs all use the
/// same representation without deinterleaving.
#[derive(Debug, Clone, Copy)]
pub struct ComponentView<'a> {
    pub samples: ComponentSampleData<'a>,
    pub offset: usize,
    pub row_stride: usize,
    pub sample_stride: usize,
    pub width: u32,
    pub height: u32,
    pub precision: u32,
    pub signed: bool,
    pub dx: u32,
    pub dy: u32,
}

impl<'a> ComponentView<'a> {
    pub fn new(
        samples: ComponentSampleData<'a>,
        offset: usize,
        row_stride: usize,
        sample_stride: usize,
        width: u32,
        height: u32,
        precision: u32,
        signed: bool,
        dx: u32,
        dy: u32,
    ) -> crate::error::Result<Self> {
        let view = Self {
            samples,
            offset,
            row_stride,
            sample_stride,
            width,
            height,
            precision,
            signed,
            dx,
            dy,
        };
        view.validate_bounds_and_metadata()?;
        Ok(view)
    }

    pub fn planar_u8(width: u32, height: u32, data: &'a [u8]) -> crate::error::Result<Self> {
        Self::new(
            ComponentSampleData::U8(data),
            0,
            width as usize,
            1,
            width,
            height,
            8,
            false,
            1,
            1,
        )
    }

    pub fn planar_u16(
        width: u32,
        height: u32,
        data: &'a [u16],
        precision: u32,
    ) -> crate::error::Result<Self> {
        Self::new(
            ComponentSampleData::U16(data),
            0,
            width as usize,
            1,
            width,
            height,
            precision,
            false,
            1,
            1,
        )
    }

    pub fn from_component(component: &'a Component) -> crate::error::Result<Self> {
        Self::new(
            ComponentSampleData::I32(&component.data),
            0,
            component.width as usize,
            1,
            component.width,
            component.height,
            component.precision,
            component.signed,
            component.dx,
            component.dy,
        )
    }

    pub fn sample_at(&self, x: u32, y: u32) -> Option<i32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = self
            .offset
            .checked_add((y as usize).checked_mul(self.row_stride)?)?
            .checked_add((x as usize).checked_mul(self.sample_stride)?)?;
        self.samples.sample_i32(index)
    }

    fn validate_bounds_and_metadata(&self) -> crate::error::Result<()> {
        validate_nonzero_dimensions(self.width, self.height)?;
        validate_precision(self.precision)?;
        if self.dx == 0 || self.dy == 0 {
            return Err(invalid_input(
                "component sampling factors dx/dy must be non-zero",
            ));
        }
        if self.row_stride == 0 || self.sample_stride == 0 {
            return Err(invalid_input(
                "component row/sample strides must be non-zero",
            ));
        }
        if matches!(self.samples, ComponentSampleData::U8(_)) && self.precision > 8 {
            return Err(invalid_input(format!(
                "u8 component storage cannot represent {} meaningful bits",
                self.precision
            )));
        }

        let last_index = self
            .offset
            .checked_add(
                ((self.height - 1) as usize)
                    .checked_mul(self.row_stride)
                    .ok_or_else(|| invalid_input("component row stride overflows usize"))?,
            )
            .and_then(|base| {
                base.checked_add(((self.width - 1) as usize).checked_mul(self.sample_stride)?)
            })
            .ok_or_else(|| invalid_input("component view bounds overflow usize"))?;

        if last_index >= self.samples.len() {
            return Err(invalid_input(format!(
                "{} component view last index {} exceeds backing length {}",
                self.samples.storage_name(),
                last_index,
                self.samples.len()
            )));
        }
        Ok(())
    }
}

/// Borrowed image view accepted by the high-resolution encoding pipeline.
#[derive(Debug, Clone)]
pub struct ImageView<'a> {
    pub width: u32,
    pub height: u32,
    pub components: Vec<ComponentView<'a>>,
    pub colorspace: ColorSpace,
}

impl<'a> ImageView<'a> {
    /// Whether the last component is an opacity channel (see
    /// [`Image::has_alpha`]).
    pub fn has_alpha(&self) -> bool {
        self.components.len() == self.colorspace.component_count() + 1
    }

    /// Interleaved 8-bit gray plus alpha (2 bytes per pixel).
    pub fn from_gray_alpha8_interleaved(
        width: u32,
        height: u32,
        data: &'a [u8],
    ) -> crate::error::Result<Self> {
        validate_exact_len("gray-alpha8", data.len(), width, height, 2)?;
        Self::interleaved_u8(width, height, data, 2, ColorSpace::Gray)
    }

    /// Interleaved 8-bit sRGB plus alpha (4 bytes per pixel, R-G-B-A order).
    pub fn from_rgba8_interleaved(
        width: u32,
        height: u32,
        data: &'a [u8],
    ) -> crate::error::Result<Self> {
        validate_exact_len("rgba8", data.len(), width, height, 4)?;
        Self::interleaved_u8(width, height, data, 4, ColorSpace::Srgb)
    }

    fn interleaved_u8(
        width: u32,
        height: u32,
        data: &'a [u8],
        channels: usize,
        colorspace: ColorSpace,
    ) -> crate::error::Result<Self> {
        let row_stride = (width as usize)
            .checked_mul(channels)
            .ok_or_else(|| invalid_input("interleaved row stride overflows usize"))?;
        let components = (0..channels)
            .map(|offset| {
                ComponentView::new(
                    ComponentSampleData::U8(data),
                    offset,
                    row_stride,
                    channels,
                    width,
                    height,
                    8,
                    false,
                    1,
                    1,
                )
            })
            .collect::<crate::error::Result<Vec<_>>>()?;
        Self::from_planar_components(width, height, colorspace, components)
    }

    pub fn from_gray8(width: u32, height: u32, data: &'a [u8]) -> crate::error::Result<Self> {
        validate_exact_len("gray8", data.len(), width, height, 1)?;
        Self::from_planar_components(
            width,
            height,
            ColorSpace::Gray,
            vec![ComponentView::planar_u8(width, height, data)?],
        )
    }

    pub fn from_gray16(
        width: u32,
        height: u32,
        data: &'a [u16],
        precision: u32,
    ) -> crate::error::Result<Self> {
        validate_exact_len("gray16", data.len(), width, height, 1)?;
        Self::from_planar_components(
            width,
            height,
            ColorSpace::Gray,
            vec![ComponentView::planar_u16(width, height, data, precision)?],
        )
    }

    pub fn from_rgb8_interleaved(
        width: u32,
        height: u32,
        data: &'a [u8],
    ) -> crate::error::Result<Self> {
        validate_exact_len("rgb8", data.len(), width, height, 3)?;
        let row_stride = (width as usize)
            .checked_mul(3)
            .ok_or_else(|| invalid_input("RGB row stride overflows usize"))?;
        let components = (0..3)
            .map(|offset| {
                ComponentView::new(
                    ComponentSampleData::U8(data),
                    offset,
                    row_stride,
                    3,
                    width,
                    height,
                    8,
                    false,
                    1,
                    1,
                )
            })
            .collect::<crate::error::Result<Vec<_>>>()?;
        Self::from_planar_components(width, height, ColorSpace::Srgb, components)
    }

    pub fn from_rgb16_interleaved(
        width: u32,
        height: u32,
        data: &'a [u16],
        precision: u32,
    ) -> crate::error::Result<Self> {
        validate_exact_len("rgb16", data.len(), width, height, 3)?;
        let row_stride = (width as usize)
            .checked_mul(3)
            .ok_or_else(|| invalid_input("RGB row stride overflows usize"))?;
        let components = (0..3)
            .map(|offset| {
                ComponentView::new(
                    ComponentSampleData::U16(data),
                    offset,
                    row_stride,
                    3,
                    width,
                    height,
                    precision,
                    false,
                    1,
                    1,
                )
            })
            .collect::<crate::error::Result<Vec<_>>>()?;
        Self::from_planar_components(width, height, ColorSpace::Srgb, components)
    }

    pub fn from_planar_components(
        width: u32,
        height: u32,
        colorspace: ColorSpace,
        components: Vec<ComponentView<'a>>,
    ) -> crate::error::Result<Self> {
        validate_nonzero_dimensions(width, height)?;
        let expected_components = colorspace.component_count();
        if components.len() != expected_components && components.len() != expected_components + 1 {
            return Err(invalid_input(format!(
                "{colorspace:?} image view expects {expected_components} components, or {} with \
                 an alpha channel, got {}",
                expected_components + 1,
                components.len()
            )));
        }
        for (index, component) in components.iter().enumerate() {
            if component.width != width || component.height != height {
                return Err(invalid_input(format!(
                    "component {index} dimensions {}×{} do not match image view {}×{}",
                    component.width, component.height, width, height
                )));
            }
        }
        Ok(Self {
            width,
            height,
            components,
            colorspace,
        })
    }

    pub fn from_image(image: &'a Image) -> crate::error::Result<Self> {
        let components = image
            .components
            .iter()
            .map(ComponentView::from_component)
            .collect::<crate::error::Result<Vec<_>>>()?;
        Self::from_planar_components(image.width, image.height, image.colorspace, components)
    }
}

impl Image {
    /// Construct from interleaved sRGB bytes (3 bytes per pixel, R-G-B order).
    ///
    /// `data.len()` must equal `width * height * 3`.
    pub fn from_rgb_bytes(width: u32, height: u32, data: &[u8]) -> crate::error::Result<Self> {
        validate_exact_len("RGB", data.len(), width, height, 3)?;
        let pixel_count = data.len() / 3;
        let mut r = Vec::with_capacity(pixel_count);
        let mut g = Vec::with_capacity(pixel_count);
        let mut b = Vec::with_capacity(pixel_count);
        for px in data.chunks_exact(3) {
            r.push(i32::from(px[0]));
            g.push(i32::from(px[1]));
            b.push(i32::from(px[2]));
        }
        Ok(Self {
            width,
            height,
            components: vec![
                make_component(r, width, height),
                make_component(g, width, height),
                make_component(b, width, height),
            ],
            colorspace: ColorSpace::Srgb,
        })
    }

    /// Construct from interleaved sRGB plus alpha bytes (4 bytes per pixel,
    /// R-G-B-A order); the alpha is straight (unassociated) opacity.
    ///
    /// `data.len()` must equal `width * height * 4`.
    pub fn from_rgba_bytes(width: u32, height: u32, data: &[u8]) -> crate::error::Result<Self> {
        validate_exact_len("RGBA", data.len(), width, height, 4)?;
        Ok(Self {
            width,
            height,
            components: deinterleave(data, 4)
                .into_iter()
                .map(|plane| make_component(plane, width, height))
                .collect(),
            colorspace: ColorSpace::Srgb,
        })
    }

    /// Construct from interleaved gray plus alpha bytes (2 bytes per pixel).
    ///
    /// `data.len()` must equal `width * height * 2`.
    pub fn from_gray_alpha_bytes(
        width: u32,
        height: u32,
        data: &[u8],
    ) -> crate::error::Result<Self> {
        validate_exact_len("gray-alpha", data.len(), width, height, 2)?;
        Ok(Self {
            width,
            height,
            components: deinterleave(data, 2)
                .into_iter()
                .map(|plane| make_component(plane, width, height))
                .collect(),
            colorspace: ColorSpace::Gray,
        })
    }

    /// Construct from grayscale bytes (1 byte per pixel).
    ///
    /// `data.len()` must equal `width * height`.
    pub fn from_gray_bytes(width: u32, height: u32, data: &[u8]) -> crate::error::Result<Self> {
        validate_exact_len("Gray", data.len(), width, height, 1)?;
        let samples: Vec<i32> = data.iter().map(|&v| i32::from(v)).collect();
        Ok(Self {
            width,
            height,
            components: vec![make_component(samples, width, height)],
            colorspace: ColorSpace::Gray,
        })
    }

    pub fn as_view(&self) -> crate::error::Result<ImageView<'_>> {
        ImageView::from_image(self)
    }
}

fn deinterleave(data: &[u8], channels: usize) -> Vec<Vec<i32>> {
    let pixels = data.len() / channels;
    let mut planes = vec![Vec::with_capacity(pixels); channels];
    for px in data.chunks_exact(channels) {
        for (plane, &sample) in planes.iter_mut().zip(px) {
            plane.push(i32::from(sample));
        }
    }
    planes
}

fn make_component(data: Vec<i32>, width: u32, height: u32) -> Component {
    Component {
        data,
        width,
        height,
        precision: 8,
        signed: false,
        dx: 1,
        dy: 1,
    }
}

fn validate_nonzero_dimensions(width: u32, height: u32) -> crate::error::Result<()> {
    if width == 0 || height == 0 {
        return Err(invalid_input(format!(
            "image dimensions must be non-zero, got {width}×{height}"
        )));
    }
    Ok(())
}

fn validate_precision(precision: u32) -> crate::error::Result<()> {
    SamplePrecision::new(precision).map(|_| ())
}

fn validate_exact_len(
    label: &str,
    actual: usize,
    width: u32,
    height: u32,
    samples_per_pixel: usize,
) -> crate::error::Result<()> {
    validate_nonzero_dimensions(width, height)?;
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(samples_per_pixel))
        .ok_or_else(|| invalid_input(format!("{label} buffer length overflows usize")))?;
    if actual != expected {
        return Err(invalid_input(format!(
            "{label} buffer length {actual} does not match {width}×{height}×{samples_per_pixel}={expected}"
        )));
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> crate::error::Jp2LamError {
    crate::error::Jp2LamError::InvalidInput(message.into())
}

/// Content profile selecting the lossy rate-control strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentProfile {
    /// General-purpose behavior (photo-safe): quality-scaled quantization steps
    /// plus lambda-based PCRD pass selection with the heuristic distortion model.
    #[default]
    General,
    /// Continuous-tone photographic content. This is currently the explicit
    /// spelling of the photo-safe `General` behavior; `General` remains as a
    /// compatibility alias during API migration.
    Photo,
    /// Scanned document pages (text/line art on a flat background).
    /// Rate control is quantization-driven (much coarser steps per quality)
    /// with a light measured-ΔMSE PCRD trim. On cleaned text pages this gives
    /// ~35% smaller output than `General` at equal PSNR. Not suitable for
    /// continuous-tone (photo) content.
    Document,
}

/// How hard the perceptual controller may work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PerceptualEffort {
    /// Three pixel probes and two exact prices (plus one documented rescue).
    Fast,
    /// Five pixel probes and three exact prices (plus one documented rescue).
    #[default]
    Balanced,
    /// Ten pixel probes and four exact prices (plus one documented rescue).
    Quality,
}

/// Color the reader's display actually shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayColor {
    /// Color panel: the metric keeps RGB.
    #[default]
    Tablet,
    /// Grayscale e-ink panel: the metric scores display luminance (no
    /// dithering is modelled).
    Eink,
}

/// The size and color the reader will actually see an image at.
///
/// Both the source and every candidate are conditioned to this box (fit
/// inside, aspect preserved, never upscaled) before scoring, so the floor
/// means "SSIMULACRA2 at display size", not at source resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayProfile {
    /// Widest the image is ever rendered, in display pixels.
    pub max_width: u32,
    /// Tallest the image is ever rendered, in display pixels.
    pub max_height: u32,
    /// Color mode of the panel.
    pub color: DisplayColor,
    /// Encode the display-sized image instead of the source.
    ///
    /// **This changes the emitted image dimensions.** With it on, a source
    /// larger than the box is box-filtered down to [`Self::metric_size`] before
    /// planning, and the codestream, the JP2 `ihdr` box and every decode carry
    /// those dimensions - so whatever places the image (a PDF `/Width`, a page
    /// layout) must read them back rather than assume the source size.
    /// [`Self::encoded_size`] answers what they will be without encoding.
    ///
    /// Off by default. Pixels the reader never sees are the cheapest bytes to
    /// drop, so this is much smaller and much faster than encoding the source
    /// and letting the metric score it downscaled; the cost is that the file
    /// can no longer be shown larger than the panel it was encoded for.
    /// Resampling is skipped when it would upscale, or when the source is less
    /// than [`Self::RESAMPLE_MIN_FACTOR`] larger than the box.
    pub resample_source: bool,
}

impl DisplayProfile {
    /// Grayscale e-ink panel of at most `max_width` x `max_height` pixels.
    #[must_use]
    pub fn eink(max_width: u32, max_height: u32) -> Self {
        Self {
            max_width,
            max_height,
            color: DisplayColor::Eink,
            resample_source: false,
        }
    }

    /// Color panel of at most `max_width` x `max_height` pixels.
    #[must_use]
    pub fn tablet(max_width: u32, max_height: u32) -> Self {
        Self {
            max_width,
            max_height,
            color: DisplayColor::Tablet,
            resample_source: false,
        }
    }

    /// Smallest linear downscale worth resampling the source for. Below it the
    /// bytes saved do not pay for throwing resolution away.
    pub const RESAMPLE_MIN_FACTOR: f64 = 1.25;

    /// Turns on [`Self::resample_source`]: encode the display-sized image, not
    /// the source. Read [`Self::resample_source`] first - it changes the
    /// emitted image dimensions.
    #[must_use]
    pub fn resampling_source(self) -> Self {
        Self {
            resample_source: true,
            ..self
        }
    }

    /// Dimensions an encode with this profile will actually emit for a
    /// `width` x `height` source: the display box when [`Self::resample_source`]
    /// is on and the source is at least [`Self::RESAMPLE_MIN_FACTOR`] larger in
    /// both axes, and the source size otherwise.
    #[must_use]
    pub fn encoded_size(self, width: u32, height: u32) -> (u32, u32) {
        if !self.resample_source {
            return (width, height);
        }
        let (fit_width, fit_height) = self.metric_size(width, height);
        if f64::from(width) < f64::from(fit_width) * Self::RESAMPLE_MIN_FACTOR {
            return (width, height);
        }
        (fit_width, fit_height)
    }

    /// Whether this profile changes what the metric sees for a `width` x
    /// `height` source: it downscales into the box, or folds a color source to
    /// e-ink luminance. A profile that does neither is the source-resolution
    /// metric under another name, and the encoder plans such a target as the
    /// plain [`PerceptualTarget`] it is - the display search policy (one probe
    /// plus two corrections) is tuned for the downscaled metric and, at box ==
    /// source, ships first probes 1.5x the bytes of the effort's own search
    /// (measured 2026-09-05 on 21 device-resolution book-scan figure crops:
    /// floor 55 landed at 65-82).
    #[must_use]
    pub fn conditions(self, width: u32, height: u32, gray_source: bool) -> bool {
        self.metric_size(width, height) != (width, height)
            || (matches!(self.color, DisplayColor::Eink) && !gray_source)
    }

    /// Size the metric scores at: fit inside the box, aspect preserved, never
    /// upscaled. Returns the source size unchanged when it already fits.
    #[must_use]
    pub fn metric_size(self, width: u32, height: u32) -> (u32, u32) {
        if width <= self.max_width && height <= self.max_height {
            return (width, height);
        }
        let scale = (f64::from(self.max_width) / f64::from(width))
            .min(f64::from(self.max_height) / f64::from(height));
        (
            ((f64::from(width) * scale).round() as u32).max(1),
            ((f64::from(height) * scale).round() as u32).max(1),
        )
    }
}

/// A minimum SSIMULACRA2 score the encoder must meet.
///
/// Construct with [`PerceptualTarget::new`] so out-of-range scores are rejected
/// at the edge. Direct struct literals are still re-checked at plan time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerceptualTarget {
    /// Minimum acceptable score, finite and in `0.0..=100.0`.
    pub score: f64,
    /// Probe / exact-price budget.
    pub effort: PerceptualEffort,
    /// When set, the score is measured at the reader's display size and color
    /// mode instead of at source resolution. `None` keeps the source-resolution
    /// metric.
    pub display: Option<DisplayProfile>,
}

impl PerceptualTarget {
    /// Builds a target, rejecting a score that is not finite and in `0..=100`.
    ///
    /// # Errors
    ///
    /// [`crate::error::Jp2LamError::InvalidInput`] when `score` is NaN,
    /// infinite, below zero, or above 100.
    pub fn new(score: f64, effort: PerceptualEffort) -> crate::error::Result<Self> {
        if !score.is_finite() || !(0.0..=100.0).contains(&score) {
            return Err(crate::error::Jp2LamError::InvalidInput(format!(
                "perceptual minimum score must be finite and in 0..=100, got {score}"
            )));
        }
        Ok(Self {
            score,
            effort,
            display: None,
        })
    }

    /// The target behind [`RateControl::Quality`]: quality `q` is a
    /// SSIMULACRA2 floor of `q`; 100 is the lossless path, not a score.
    ///
    /// # Errors
    ///
    /// [`crate::error::Jp2LamError::InvalidInput`] for quality 100, which is
    /// the lossless path and has no score floor.
    pub fn for_quality(quality: u8, effort: PerceptualEffort) -> crate::error::Result<Self> {
        if quality >= 100 {
            return Err(crate::error::Jp2LamError::InvalidInput(
                "quality 100 is lossless; use RateControl::Lossless".into(),
            ));
        }
        Self::new(f64::from(quality), effort)
    }

    /// A floor measured at the size and color the reader will actually see,
    /// not at source resolution: both source and candidate are downscaled into
    /// `display` (and folded to luminance for e-ink) before scoring.
    ///
    /// Suggested default for page images on an e-reader: `score` 70,
    /// [`DisplayProfile::eink`], [`PerceptualEffort::Fast`]. 70 is where the
    /// byte cost per floor point turns over on the 2026-09-05 display corpus
    /// (11 photos, 0.8-6 MP, four panels): below it a point of floor costs
    /// under 2.7% of the file, above it about 4%. At 600x450 a floor of 70
    /// lands 0.49x the bytes of a plain source-resolution q75 and delivers 75.6
    /// at display size on average, because the correction aims above the floor.
    /// The same number suits a colour tablet - a bigger panel is already a
    /// stricter test at the same score (source-resolution equivalent 62 at
    /// 800x600 against 51 at 600x450), so the profile carries the difference
    /// and the floor does not have to.
    ///
    /// A display target is capped at two metric evaluations - one encode plus
    /// one correction - whatever `effort` says, because the point is a cheap
    /// verified encode. A third, rescue evaluation at the quantizer's all-pass
    /// body is spent only when neither of those met the floor: the metric is
    /// not monotone in body bytes at heavy downscale, so an aimed correction
    /// can miss, and a fat verified stream beats a failed encode.
    ///
    /// # Errors
    ///
    /// [`crate::error::Jp2LamError::InvalidInput`] for a score outside
    /// `0..=100` or a display box with a zero dimension.
    pub fn for_display(
        score: f64,
        display: DisplayProfile,
        effort: PerceptualEffort,
    ) -> crate::error::Result<Self> {
        if display.max_width == 0 || display.max_height == 0 {
            return Err(crate::error::Jp2LamError::InvalidInput(format!(
                "display profile must have non-zero pixel dimensions, got {}x{}",
                display.max_width, display.max_height
            )));
        }
        Ok(Self {
            display: Some(display),
            ..Self::new(score, effort)?
        })
    }
}

/// Why a completed score-targeted search stopped where it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityStatus {
    /// The selected stream meets the target.
    Met,
    /// The selected stream meets the target but the budget ran out before the
    /// overshoot could be tightened.
    MetWorkCap,
    /// Even the coarsest quantizer meets the target; the coarsest was chosen.
    SaturatedFloor,
    /// Even the finest quantizer misses the target.
    SaturatedTop,
    /// The probe budget (plus its one rescue probe) ran out before any
    /// candidate met the target.
    UnderTargetWorkCap,
}

/// What one scored candidate reported.
#[derive(Debug, Clone, PartialEq)]
pub struct PerceptualObservation {
    /// Canonical SSIMULACRA2 score against the source.
    pub score: f64,
    /// Wall time of candidate reconstruction, in milliseconds.
    pub reconstruct_millis: Option<u64>,
    /// Wall time of the metric pass, in milliseconds.
    pub metric_millis: Option<u64>,
}

/// One scored probe in a perceptual search.
#[derive(Debug, Clone, PartialEq)]
pub struct PerceptualProbe {
    /// Global quantizer scale used for this probe (`1.0` is the ISO-derived
    /// q50 step). Smaller is finer.
    pub quant_scale: f64,
    /// PCRD body-byte truncation applied at that quantizer.
    pub pcrd_bytes: u32,
    /// Canonical score of the reconstructed candidate.
    pub score: f64,
    /// Whether `score` met the requested floor.
    pub feasible: bool,
}

/// Ordered diagnostic of one perceptual encode.
#[derive(Debug, Clone, PartialEq)]
pub struct PerceptualTrace {
    /// Requested SSIMULACRA2 floor.
    pub target_score: f64,
    /// Pinned metric identity the scores came from.
    pub metric_version: &'static str,
    /// Probes in search order.
    pub probes: Vec<PerceptualProbe>,
    /// Winner score, when any candidate was scored.
    pub winner_score: Option<f64>,
    /// Winner output bytes, when a stream was priced.
    pub winner_bytes: Option<u64>,
    /// Why the search stopped.
    pub status: QualityStatus,
}

/// Caller-visible compression intent.
///
/// Exact-rate variants target the complete selected output: the raw codestream
/// for [`OutputFormat::J2k`], or the complete JP2 file for
/// [`OutputFormat::Jp2`]. `TargetBitsPerPixel` is the total compressed output
/// bits divided by full-image pixel count, across all components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RateControl {
    /// Reversible 5/3 coding with no truncation target.
    Lossless,
    /// Canonical quality: `level` in `0..=99` is a verified SSIMULACRA2 floor
    /// of the same value (`ssimulacra2-jpxl-1`), so quality 80 means score 80
    /// for every image and resolution. Emits the smallest exact stream that
    /// meets it; `effort` bounds the metric evaluations spent finding it.
    ///
    /// Requires 8-bit unsigned gray or enumerated sRGB; other inputs are
    /// rejected. Two cases resolve to [`Self::Lossless`], which meets every
    /// floor exactly: `level` 100 (an identical image is the only way to score
    /// 100), and a source below the metric's 8x8 minimum in either dimension,
    /// where no score exists.
    Quality { level: u8, effort: PerceptualEffort },
    /// Legacy open-loop photographic preset in `0..=99`: one encode at a rate
    /// derived from the number, no measurement. Cheap, but the resulting
    /// perceptual quality varies with content and resolution (the Session 8c
    /// calibration measured preset 80 at SSIMULACRA2 52 on 0.8 MP photos and
    /// 14 at 50 MP). Not on the [`Self::Quality`] scale.
    ApproxQuality(u8),
    /// Expert form of [`Self::Quality`]: a raw SSIMULACRA2 floor, fractional
    /// scores allowed, with the same input requirements. A score of exactly
    /// 100 resolves to [`Self::Lossless`], like `Quality { level: 100 }`.
    Perceptual(PerceptualTarget),
    /// Target complete output size in bytes. This and the two variants below
    /// allocate by measured mean-squared error, not by the perceptual metric:
    /// they hit a size, not a look.
    TargetBytes(u64),
    /// Target complete output bits per full-image pixel, across all components.
    TargetBitsPerPixel(f32),
    /// Meaningful source bits divided by complete compressed output bits.
    /// Source bits use declared component precision and sampling geometry, not
    /// the Rust storage width.
    CompressionRatio(f32),
}

#[derive(Debug, Clone)]
pub struct EncodeOptions {
    /// Approximate preset 0–100 used only when `rate_control` is absent:
    /// 100 = lossless (reversible 5/3), below 100 = [`RateControl::ApproxQuality`]
    /// (irreversible 9/7 at a rate derived from the number, unmeasured).
    /// Photographs wanting a guaranteed quality use [`Self::photo_quality`]
    /// or [`RateControl::Quality`] instead.
    pub quality: u8,
    /// Authoritative rate-control intent. When absent, `quality` applies as
    /// described above.
    pub rate_control: Option<RateControl>,
    pub format: OutputFormat,
    /// Lossy rate-control strategy. Ignored for lossless (quality 100).
    pub profile: ContentProfile,
    /// Tile policy for bounded-memory high-resolution encoding.
    pub tile_policy: TilePolicy,
    /// Optional resource limits for future bounded-memory planning.
    pub resource_limits: ResourceLimits,
    /// Optional explicit JP2 color description. Required for ambiguous legacy
    /// `ColorSpace::Rgb`; ordinary Gray/Srgb constructors infer their matching
    /// enumerated description when this is `None`.
    pub color_encoding: Option<ColorEncoding>,
    /// Codestream coding parameters (decomposition levels, code-block size,
    /// progression order, component transform). Defaults let the encoder
    /// choose.
    pub coding: CodingOptions,
}

impl EncodeOptions {
    /// Convenience constructor from a named preset.
    pub fn from_preset(preset: Preset, format: OutputFormat) -> Self {
        let is_photo = matches!(
            preset,
            Preset::PhotoCompact | Preset::PhotoHigh | Preset::PhotoNearLossless
        );
        Self {
            quality: preset.quality(),
            rate_control: is_photo.then(|| RateControl::Quality {
                level: preset.quality(),
                effort: PerceptualEffort::Balanced,
            }),
            format,
            profile: if is_photo {
                ContentProfile::Photo
            } else {
                ContentProfile::General
            },
            tile_policy: if is_photo {
                TilePolicy::Auto
            } else {
                TilePolicy::Single
            },
            resource_limits: ResourceLimits::default(),
            color_encoding: None,
            coding: CodingOptions::default(),
        }
    }

    /// JP2 photograph at verified quality `level` (SSIMULACRA2 floor of
    /// `level`, balanced effort): the one-call form of [`Self::photo`].
    pub fn photo_quality(level: u8) -> Self {
        Self::photo(level, OutputFormat::Jp2)
    }

    /// JP2 photograph whose verified floor is measured at the reader's display
    /// size and color mode (see [`PerceptualTarget::for_display`]).
    ///
    /// # Errors
    ///
    /// [`crate::error::Jp2LamError::InvalidInput`] for an out-of-range score or
    /// a degenerate display box.
    pub fn display_photo(score: f64, display: DisplayProfile) -> crate::error::Result<Self> {
        let target = PerceptualTarget::for_display(score, display, PerceptualEffort::Fast)?;
        Ok(Self {
            rate_control: Some(RateControl::Perceptual(target)),
            ..Self::photo(99, OutputFormat::Jp2)
        })
    }

    /// Options for scanned document pages (see [`ContentProfile::Document`]).
    pub fn document(quality: u8, format: OutputFormat) -> Self {
        Self {
            quality,
            rate_control: None,
            format,
            profile: ContentProfile::Document,
            tile_policy: TilePolicy::Single,
            resource_limits: ResourceLimits::default(),
            color_encoding: None,
            coding: CodingOptions::default(),
        }
    }

    /// Options for continuous-tone photographs on the canonical verified
    /// quality scale ([`RateControl::Quality`], balanced effort); `quality`
    /// 100 stays lossless.
    pub fn photo(quality: u8, format: OutputFormat) -> Self {
        Self {
            quality,
            rate_control: Some(if quality >= 100 {
                RateControl::Lossless
            } else {
                RateControl::Quality {
                    level: quality,
                    effort: PerceptualEffort::Balanced,
                }
            }),
            format,
            profile: ContentProfile::Photo,
            tile_policy: TilePolicy::Auto,
            resource_limits: ResourceLimits::default(),
            color_encoding: None,
            coding: CodingOptions::default(),
        }
    }
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            quality: Preset::DocumentHigh.quality(),
            rate_control: None,
            format: OutputFormat::Jp2,
            profile: ContentProfile::General,
            tile_policy: TilePolicy::Single,
            resource_limits: ResourceLimits::default(),
            color_encoding: None,
            coding: CodingOptions::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ColorSpace, ComponentView, ContentProfile, EncodeOptions, Image, ImageView, OutputFormat,
        PerceptualEffort, PerceptualTarget, Preset, RateControl, SamplePrecision, TilePolicy,
    };

    #[test]
    fn perceptual_target_rejects_scores_outside_zero_to_one_hundred() {
        for score in [f64::NAN, f64::INFINITY, -0.1, 100.1] {
            assert!(
                PerceptualTarget::new(score, PerceptualEffort::Balanced).is_err(),
                "accepted {score}"
            );
        }
        let ok = PerceptualTarget::new(90.0, PerceptualEffort::Fast).expect("90 is in range");
        assert_eq!(ok.score, 90.0);
        assert_eq!(ok.effort, PerceptualEffort::Fast);
        assert!(PerceptualTarget::new(0.0, PerceptualEffort::Quality).is_ok());
        assert!(PerceptualTarget::new(100.0, PerceptualEffort::Balanced).is_ok());
    }

    #[test]
    fn photographic_presets_and_constructor_use_explicit_photo_profile() {
        assert_eq!(Preset::PhotoCompact.quality(), 45);
        assert_eq!(Preset::PhotoHigh.quality(), 75);
        assert_eq!(Preset::PhotoNearLossless.quality(), 95);
        let options = EncodeOptions::photo(75, OutputFormat::Jp2);
        assert_eq!(options.profile, ContentProfile::Photo);
        let verified = Some(RateControl::Quality {
            level: 75,
            effort: PerceptualEffort::Balanced,
        });
        assert_eq!(options.rate_control, verified);
        assert_eq!(options.tile_policy, TilePolicy::Auto);
        let preset = EncodeOptions::from_preset(Preset::PhotoHigh, OutputFormat::Jp2);
        assert_eq!(preset.profile, ContentProfile::Photo);
        assert_eq!(preset.rate_control, verified);
        assert_eq!(
            EncodeOptions::photo_quality(80).rate_control,
            Some(RateControl::Quality {
                level: 80,
                effort: PerceptualEffort::Balanced
            })
        );
        assert_eq!(
            EncodeOptions::photo(100, OutputFormat::Jp2).rate_control,
            Some(RateControl::Lossless)
        );
    }

    #[test]
    fn sample_precision_derives_checked_limits() {
        let precision = SamplePrecision::new(16).expect("16-bit precision");
        assert_eq!(precision.bits(), 16);
        assert_eq!(precision.unsigned_level_shift(), 32_768);
        assert_eq!(precision.unsigned_max(), 65_535);
        assert!(SamplePrecision::new(7).is_err());
        assert!(SamplePrecision::new(17).is_err());
    }

    #[test]
    fn rgb8_interleaved_view_uses_shared_storage_and_strides() {
        let data = [
            10u8, 20, 30, //
            40, 50, 60, //
            70, 80, 90, //
            100, 110, 120,
        ];
        let view = ImageView::from_rgb8_interleaved(2, 2, &data).expect("rgb view");

        assert_eq!(view.width, 2);
        assert_eq!(view.height, 2);
        assert_eq!(view.colorspace, ColorSpace::Srgb);
        assert_eq!(view.components.len(), 3);
        assert_eq!(view.components[0].offset, 0);
        assert_eq!(view.components[1].offset, 1);
        assert_eq!(view.components[2].offset, 2);
        assert_eq!(view.components[0].row_stride, 6);
        assert_eq!(view.components[0].sample_stride, 3);
        assert_eq!(view.components[0].sample_at(1, 1), Some(100));
        assert_eq!(view.components[1].sample_at(1, 1), Some(110));
        assert_eq!(view.components[2].sample_at(1, 1), Some(120));
    }

    #[test]
    fn gray16_view_preserves_meaningful_precision() {
        let data = [0u16, 1023, 17, 33];
        let view = ImageView::from_gray16(2, 2, &data, 10).expect("gray16 view");

        assert_eq!(view.components[0].precision, 10);
        assert_eq!(view.components[0].sample_at(1, 0), Some(1023));
    }

    #[test]
    fn rgb16_interleaved_view_preserves_precision_and_strides() {
        let data = [
            0u16, 1023, 17, //
            33, 512, 999,
        ];
        let view = ImageView::from_rgb16_interleaved(2, 1, &data, 10).expect("rgb16 view");

        assert_eq!(view.colorspace, ColorSpace::Srgb);
        assert_eq!(view.components[0].precision, 10);
        assert_eq!(view.components[0].row_stride, 6);
        assert_eq!(view.components[0].sample_stride, 3);
        assert_eq!(view.components[0].sample_at(1, 0), Some(33));
        assert_eq!(view.components[1].sample_at(1, 0), Some(512));
        assert_eq!(view.components[2].sample_at(1, 0), Some(999));
    }

    #[test]
    fn image_view_rejects_invalid_precision_and_short_buffers() {
        let data = [0u16; 4];
        let err = ImageView::from_gray16(2, 2, &data, 17).expect_err("precision should fail");
        assert!(err.to_string().contains("8..=16"), "{err}");

        let short = [0u8; 5];
        let err = ImageView::from_rgb8_interleaved(2, 1, &short).expect_err("length should fail");
        assert!(err.to_string().contains("buffer length"), "{err}");
    }

    #[test]
    fn component_view_rejects_out_of_bounds_strides() {
        let data = [0u8; 8];
        let err = ComponentView::new(
            super::ComponentSampleData::U8(&data),
            2,
            10,
            1,
            2,
            2,
            8,
            false,
            1,
            1,
        )
        .expect_err("view should exceed backing storage");

        assert!(err.to_string().contains("exceeds backing length"), "{err}");
    }

    #[test]
    fn owned_image_adapts_to_view_without_component_clone() {
        let image = Image::from_gray_bytes(2, 2, &[1, 2, 3, 4]).expect("owned image");
        let view = image.as_view().expect("image view");

        assert_eq!(view.colorspace, ColorSpace::Gray);
        assert_eq!(view.components[0].sample_at(0, 1), Some(3));
        assert_eq!(view.components[0].sample_at(1, 1), Some(4));
    }

    #[test]
    fn owned_image_constructors_reject_zero_and_overflowing_dimensions() {
        for result in [
            Image::from_gray_bytes(0, 1, &[]),
            Image::from_rgb_bytes(1, 0, &[]),
            Image::from_gray_bytes(u32::MAX, u32::MAX, &[]),
            Image::from_rgb_bytes(u32::MAX, u32::MAX, &[]),
        ] {
            assert!(result.is_err());
        }
    }
}
