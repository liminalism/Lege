# jp2lam

The canonical source is the [Lege monorepo](https://github.com/liminalism/lege/tree/main/lege-codecs/jp2lam).

JPEG 2000 Part 1 encoding in Rust, with focused JP2 decoding for document image workflows.

`jp2lam` writes unsigned 8–16-bit grayscale or sRGB images as JP2 files or raw J2K codestreams. It also includes a focused native decoder used for encoder conformance and Internet Archive-style JP2 page images.

The current goal is practical document-image interoperability first: a small Rust API, explicit unsupported-feature errors, and code organized around the JPEG 2000 standard rather than OpenJPEG internals.

## Highlights

- Encode unsigned 8–16-bit grayscale or sRGB input, with an optional alpha channel, from owned images or borrowed planar/interleaved views.
- Write JP2 wrapper files or raw J2K codestreams.
- Use explicit lossless, quality, target-byte, total-bpp, or compression-ratio rate control.
- Choose decomposition levels, code-block size, progression order, quality layers, the six code-block coding switches, and whether RGB gets the component transform (`CodingOptions`).
- Bound high-resolution work with fixed/automatic tiling and RAM-to-temporary-file code-block spill.
- Label enumerated grayscale/sRGB explicitly, or embed a restricted (matrix-shaper) ICC profile exactly; anything else is refused rather than mislabelled.
- Decode Internet Archive-style JP2 page images into the crate's native `Image` model.
- Batch encode or decode folders whose images share dimensions, color model, and precision; encoded batches also share quality and format.
- Retain the legacy `quality` field while providing explicit `RateControl` and photographic presets.
- Optional PSNR/SSIM helper through `encode_with_psnr`.
- Optional `image::ImageEncoder` implementation (`Jp2Encoder`) behind the `image` feature.
- Optional CLI behind the `cli` feature.

## Install

```toml
[dependencies]
jp2lam = "0.3"
```

The production SSIMULACRA2 metric comes from the published
[`jpxl-perceptual`](https://crates.io/crates/jpxl-perceptual) crate, shared with
JPXL so both codecs score with the same `METRIC_VERSION`.

Minimum Rust version from the crate manifest:

```text
Rust 1.95+
```

## Quick Start

### Encode RGB To JP2

```rust
use jp2lam::{EncodeOptions, Image, OutputFormat, Preset};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let width = 800;
    let height = 600;
    let rgb = vec![128u8; width * height * 3];

    let image = Image::from_rgb_bytes(width as u32, height as u32, &rgb)?;
    let options = EncodeOptions::from_preset(Preset::PhotoHigh, OutputFormat::Jp2);

    let bytes = jp2lam::encode(&image, &options)?;
    std::fs::write("output.jp2", bytes)?;

    Ok(())
}
```

### Decode JP2

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("page.jp2")?;
    let image = jp2lam::decode_jp2(&bytes)?;

    println!(
        "{}x{} {:?} components={}",
        image.width,
        image.height,
        image.colorspace,
        image.components.len()
    );

    Ok(())
}
```

### Decode Directly For A Renderer

Use a reusable `Jp2Decoder` when the destination will display a minified image.
The decoder omits unneeded high-resolution code-block reconstruction and writes
the requested packed 8-bit pixels directly:

```rust
use jp2lam::{
    DecodeConcurrency, DecodeOutputFormat, DecodeRequest, DecodeResolution, DecodeResult,
    Jp2Decoder,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("page.jp2")?;
    let mut decoder = Jp2Decoder::new();
    let request = DecodeRequest {
        resolution: DecodeResolution::AtLeast {
            width: 800,
            height: 1100,
            quality_margin: 1.5,
        },
        output: DecodeOutputFormat::Rgb8,
        concurrency: DecodeConcurrency::Budgeted(2),
        ..Default::default()
    };

    let DecodeResult::Raster(raster) = decoder.decode(&bytes, &request)? else {
        unreachable!("a packed output format returns a raster");
    };
    render_rgb8(raster.width, raster.height, raster.stride, &raster.data);
    Ok(())
}

fn render_rgb8(_width: u32, _height: u32, _stride: usize, _pixels: &[u8]) {}
```

Reduced-resolution and region (`DecodeRegion`) requests work on single- and
multi-tile codestreams.

### Batch Encode Matching Pages

Use `BatchEncoder` when a folder or page stream comes from the same source and should keep one consistent image profile and encode configuration.

```rust
use jp2lam::{BatchEncoder, EncodeOptions, Image, OutputFormat};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let width = 2550;
    let height = 3300;
    let pages: Vec<Vec<u8>> = load_grayscale_pages();

    let options = EncodeOptions {
        quality: 85,
        format: OutputFormat::Jp2,
        ..Default::default()
    };
    let mut encoder = BatchEncoder::new(options);

    for (idx, page) in pages.iter().enumerate() {
        let image = Image::from_gray_bytes(width, height, page)?;
        let bytes = encoder.encode_one(&image)?;
        std::fs::write(format!("page_{idx:04}.jp2"), bytes)?;
    }

    Ok(())
}

fn load_grayscale_pages() -> Vec<Vec<u8>> {
    todo!("load pages from your own folder or pipeline")
}
```

`BatchDecoder` provides the matching decode path and rejects later items whose decoded image profile does not match the first page.

## Decoder Scope

The decoder is intentionally focused. It currently targets JP2 wrapper files and bare JPEG 2000 Part 1 codestreams that match the Internet Archive and PDF `/JPXDecode` page-image outputs tested during development:

- Unsigned 8–16-bit grayscale, RGB, CMYK, and palette-mapped images.
- Single or multi-tile codestreams, including multiple packet-boundary tile-parts per tile and Annex B.11 interleaving between tiles. Per-tile coding-style and quantization overrides remain unsupported.
- All five Part 1 progression orders, explicit precinct partitions, and SOP/EPH packet delimiters.
- All six code-block coding switches: selective arithmetic bypass, context reset, per-pass termination, vertical-causal contexts, predictable termination, and segmentation symbols.
- Common Archive.org `_jp2.zip` and extracted `*_jp2/` page-image layouts through the CLI.

This is not a universal JPEG 2000 or JPX decoder yet. Arbitrary ICC profiles, multiple codestream boxes, and other out-of-scope features fail explicitly.

## Quality Guide

Prefer `RateControl` for new code. `Lossless` selects reversible 5/3. `Quality { level, effort }` is the canonical quality scale: `level` is a verified SSIMULACRA2 floor of the same value (quality 80 means score 80 on every image and resolution), and the encoder emits the smallest stream that meets it, spending up to `effort` metric evaluations to find it; `EncodeOptions::photo_quality(80)` is the one-call form. `ApproxQuality(0..=99)` is the legacy open-loop photographic preset: one encode at a rate derived from the number, cheap and unmeasured, whose perceptual quality varies with content and resolution (preset 80 measured SSIMULACRA2 52 at 0.8 MP and 14 at 50 MP). `Perceptual(PerceptualTarget)` is the expert form of `Quality` with a raw fractional score, and its optional `display` profile (`PerceptualTarget::for_display`, `EncodeOptions::display_photo`, CLI `--display eink:WxH` / `--display tablet:WxH`) moves the floor to where the reader sees it: source and candidate are both box-filtered in linear light into the display box (fit inside, aspect preserved, never upscaled) and, for `Eink`, folded to display luminance, so the score means "SSIMULACRA2 at display size". That metric is cheaper and more permissive than the source-resolution one, so a display target spends at most three evaluations - one encode plus two corrections - and still refuses to emit an under-floor stream; 72-75 with `Eink` and `Fast` effort suits small page images. A profile that conditions nothing for the source - a color panel the image already fits, or an e-ink panel for a gray source - is planned as the plain `Quality` target with the effort's own search (`DisplayProfile::conditions`): the display policy at box == source shipped first probes up to 1.5x the bytes of that search. `DisplayProfile::resampling_source()` (CLI `--resample`) goes one step further and encodes the display-sized image instead of the source, so the pixels the reader will never see are never coded: on the 2026-09-05 display corpus (11 photos, 0.8-6 MP, floor 70 into e-ink 600x450) that is 0.44x the bytes, 0.26x the encode time and 0.30x the decode time of the same target at source resolution, with every emitted stream still clearing the floor when re-measured against a linear-light downscale of the original. **It changes the emitted image dimensions** - `DisplayProfile::encoded_size(width, height)` says what they will be - so whatever places the image must read them back; it is off by default, and skipped when the source is less than `DisplayProfile::RESAMPLE_MIN_FACTOR` (1.25x) larger than the box. Two inputs resolve a `Quality` or `Perceptual` target to reversible 5/3, which meets any floor exactly, instead of failing: a floor of 100 (only an identical image scores 100), and a source below the metric's 8x8 minimum, which has no score. Verified quality needs 8-bit unsigned gray or enumerated sRGB; 10-16-bit input takes `ApproxQuality` or an exact-rate target. The exact-rate variants (`TargetBytes`, `TargetBitsPerPixel`, `CompressionRatio`) target the complete J2K codestream or JP2 file and allocate bytes by measured mean-squared error, not by the perceptual metric. The legacy `quality` field retains its compatibility meaning when `rate_control` is absent.

The table below is the legacy `quality` / `ApproxQuality` preset scale, not the verified `Quality` scale:

| Quality  | Use case                                                                    |
| -------: | --------------------------------------------------------------------------- |
|  `0..25` | Very small output, previews, stress testing                                 |
| `30..50` | Compact lossy output                                                        |
| `60..85` | Practical range for documents, screenshots, illustrations, and mixed images |
| `90..99` | High-fidelity lossy output                                                  |
|    `100` | Lossless output                                                             |

## Supported Input

Encoder input:

- Unsigned 8–16-bit grayscale.
- Unsigned 8–16-bit sRGB/RGB with explicit color description.
- Either of those plus one straight-alpha component.
- Full-size, non-subsampled components.
- Any non-zero size. A tile may not straddle a multiple of 32768 samples (the encoder writes one precinct per resolution), so images larger than that need `TilePolicy::Auto` or a fixed tile size that divides 32768.
- Borrowed `u8`/`u16` planar or interleaved input through `ImageView`.
- Interleaved RGB input through `Image::from_rgb_bytes`.
- Grayscale input through `Image::from_gray_bytes`.

Decoder input:

- JP2 files accepted by the focused decoder scope above.
- Complete in-memory byte slices through `decode_jp2`.
- Reader-backed input through `decode_from_reader`.

## CLI

The CLI is optional:

```bash
cargo run --release --features cli --bin jp2lam -- input.png
cargo run --release --features cli --bin jp2lam -- encode input.png q80                    # verified: SSIMULACRA2 >= 80, balanced effort
cargo run --release --features cli --bin jp2lam -- encode input.png q80 --effort fast      # same floor, fewer metric evaluations
cargo run --release --features cli --bin jp2lam -- encode input.png --approx q75           # legacy open-loop preset, unmeasured
cargo run --release --features cli --bin jp2lam -- encode input.png --ssimulacra2 82.5     # expert: raw score floor
cargo run --release --features cli --bin jp2lam -- encode input.png q75 --display eink:400x300  # floor measured at the reader's display size, in grayscale
cargo run --release --features cli --bin jp2lam -- encode input.png q75 --display tablet:1024x768
cargo run --release --features cli --bin jp2lam -- encode input.png q70 --display eink:600x450 --resample  # encode the panel, not the source: output is 600x450
cargo run --release --features cli --bin jp2lam -- encode-dir pages_png/ pages_jp2/ q85
cargo run --release --features cli --bin jp2lam -- decode page.jp2 page.png
cargo run --release --features cli --bin jp2lam -- decode-dir book_jp2/ book_png/
cargo run --release --features cli --bin jp2lam -- decode-zip book_jp2.zip book_png/
```

The CLI reads normal image files through the optional `image` dependency, writes encoded JP2 output, and writes decoded pages as PNG files. `decode-zip` also walks nested ZIP entries and preserves the archive-relative output layout.

## Coding Parameters

`EncodeOptions::coding` holds the codestream parameters a caller can set. Every field defaults to letting the encoder choose.

```rust
use jp2lam::{
    CodeBlockStyle, CodingOptions, ComponentTransform, EncodeOptions, ProgressionOrder,
    QualityLayers, RateControl,
};

let options = EncodeOptions {
    rate_control: Some(RateControl::CompressionRatio(20.0)),
    coding: CodingOptions {
        decomposition_levels: Some(5),     // NL; resolutions = NL + 1
        code_block_size: Some((32, 32)),   // powers of two, 4..=1024, area <= 4096
        progression_order: ProgressionOrder::Rpcl,
        component_transform: ComponentTransform::Auto, // `None`: store RGB/YCbCr as given
        layers: QualityLayers::Count(4),  // or Ratios(vec![40.0, 20.0]) below the final rate
        code_block_style: CodeBlockStyle {
            bypass: true,                  // raw-coded lower passes: faster, slightly larger
            ..CodeBlockStyle::default()    // reset, termall, causal, pterm, segsym likewise
        },
    },
    ..EncodeOptions::default()
};
```

Quality layers split each tile's packets so a reader can stop early: the final layer completes the stream the rate control chose, and each layer below it is the rate-distortion-optimal prefix of that stream for a smaller budget (PCRD run again on the final selection), so the decoded picture is the single-layer one and only packet headers are added. `Count(n)` places boundaries at geometric fractions of the final size; `Ratios` names compression ratios for the layers below the final one, the way OpenJPEG's `-r` list reads. On a 256x192 test image at `ApproxQuality(85)`, `Count(4)` decoded layer by layer with OpenJPEG at 17.9, 20.9, 28.0 and 38.5 dB for 83 extra bytes.

`code_block_style` sets the six Annex D switches, written to COD and honoured per pass: with `bypass`, the significance and refinement passes of the fifth and later bit-planes are raw bits in their own codeword segments (Table D.9); `terminate_each_pass` makes every pass a segment; `reset_contexts`, `vertical_causal`, `predictable_termination` and `segmentation_symbols` do what their names say. The all-off style is the tuned single-segment encoder; any switch takes the general pass loop. Every combination round-trips through this crate's decoder and OpenJPEG, lossless and lossy, with quality layers and tiles.

The rest of the recipe is fixed, and every fixed choice is a legal Part 1 baseline: one precinct per subband, two guard bits, zero image and tile origins, one tile-part per tile, no SOP/EPH, ROI, POC, TLM or PLT markers, and Rsiz 0. Lossless RGB is coded without the reversible colour transform for now.

An alpha channel is one more component after the colour ones (`Image::from_rgba_bytes`, `Image::from_gray_alpha_bytes`, `ImageView::from_rgba8_interleaved`, `ImageView::from_gray_alpha8_interleaved`, or any view whose component count is the colour count plus one). It is coded as its own codestream component outside the colour transform and declared as straight opacity in the JP2 `cdef` box, which this crate's decoder reports as `in_data_alpha`. The verified quality floor scores colour only, so images with alpha take `ApproxQuality`, `Lossless` or an exact-rate target.

Not encoded: signed samples, subsampled components, precision outside 8-16 bits, premultiplied alpha, palettes, CMYK and YCbCr colour spaces (all of which the decoder reads), resolution boxes, and XML/UUID metadata. A JP2 file whose codestream exceeds 4 GiB uses the extended (`XLBox`) box length.

## `image` Crate Integration

With the `image` feature, `Jp2Encoder` implements `image::ImageEncoder` for `L8`, `L16`, `Rgb8`, `Rgb16`, `La8` and `Rgba8` buffers, and `set_icc_profile` embeds a restricted (matrix-shaper) ICC profile:

```rust,ignore
let picture = image::open("page.png")?;
let mut out = std::fs::File::create("page.jp2")?;
picture.write_with_encoder(jp2lam::Jp2Encoder::new(&mut out))?; // lossless
```

`Jp2Encoder::with_options` takes any `EncodeOptions`.

## Batch API

The batch API is for callers handling a sequence of images from one source. It does not require a folder abstraction in the library itself; external programs can walk their own directories and feed each page to `BatchEncoder` or `BatchDecoder`.

`BatchEncoder` checks dimensions, color model, component precision, component sampling, quality, and output format against the first image. `BatchDecoder` checks the decoded image profile against the first decoded page. The current implementation mainly centralizes validation and call shape; it is also the place to add future buffer reuse or shared setup without changing external callers.

Convenience helpers are available when all inputs are already in memory:

```rust
let encoded_pages = jp2lam::encode_batch(images.iter(), &options)?;
let decoded_pages = jp2lam::decode_batch(jp2_streams.iter().map(Vec::as_slice))?;
```

## Metrics

Use `encode_with_psnr` when you want encoded bytes plus internal PSNR/SSIM estimates:

```rust
use jp2lam::{EncodeOptions, Image, OutputFormat};

fn main() -> jp2lam::Result<()> {
    let width = 800;
    let height = 600;
    let gray = vec![240u8; width * height];

    let image = Image::from_gray_bytes(width as u32, height as u32, &gray)?;
    let options = EncodeOptions {
        quality: 75,
        format: OutputFormat::Jp2,
        ..Default::default()
    };

    let (bytes, metrics) = jp2lam::encode_with_psnr(&image, &options)?;

    println!("bytes: {}", bytes.len());
    println!("psnr: {:?}", metrics.psnr_db);
    println!("ssim: {:?}", metrics.ssim);

    Ok(())
}
```

## What It Does Internally

The encoder pipeline is organized around the standard JPEG 2000 stages:

1. Validate image and component metadata.
2. Prepare image samples.
3. Apply color transform where needed.
4. Run reversible 5/3 or irreversible 9/7 DWT.
5. Quantize lossy coefficients.
6. Encode code-block bit-planes with Tier-1 coding.
7. Select truncation points with PCRD.
8. Build packets and packet headers.
9. Write codestream markers.
10. Wrap the codestream in JP2 boxes when requested.

The crate also has an explicit Annex B geometry layer for tiles, tile-components, subbands, precincts, and code-blocks. Fixed and automatic tiling use reference-grid-aware DWT phase, and the production writer streams selected code-block payloads from bounded RAM/spill storage.

## Features

Default library build:

```toml
jp2lam = "0.3"
```

Optional CLI:

```toml
jp2lam = { version = "0.3", features = ["cli"] }
```

Available feature flags:

| Feature    | Purpose                                             |
| ---------- | --------------------------------------------------- |
| `simd`     | Default. Portable SIMD (`wide`) for DWT and colour transforms |
| `image`    | `Jp2Encoder`, an `image::ImageEncoder` implementation |
| `cli`      | Enables the command-line tools and image-file input (implies `image`) |
| `profile`  | Enables profiling hooks                             |
| `counters` | Exposes Tier-1 and stage-level memory high-water counters |

## Testing

```bash
cargo test
cargo test --features cli
```

## License

Dual-licensed under either:

- MIT
- Apache-2.0
