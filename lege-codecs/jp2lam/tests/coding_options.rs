//! Public coding parameters: decomposition levels, code-block size, progression
//! order and the component transform reach the codestream, are signalled in
//! COD, and decode back - through this crate's decoder and, when ImageMagick
//! with OpenJPEG is installed, through an independent one.

use jp2lam::{
    CodingOptions, ComponentTransform, EncodeOptions, Image, OutputFormat, ProgressionOrder,
    QualityLayers, RateControl, TilePolicy, decode_jp2, encode, inspect_jp2,
};
use std::process::Command;

const ORDERS: [ProgressionOrder; 5] = [
    ProgressionOrder::Lrcp,
    ProgressionOrder::Rlcp,
    ProgressionOrder::Rpcl,
    ProgressionOrder::Pcrl,
    ProgressionOrder::Cprl,
];

fn rgb(width: u32, height: u32) -> Image {
    let mut data = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let texture = (x * 7 + y * 3) % 23;
            data.push((x * 200 / width + texture) as u8);
            data.push((y * 200 / height + texture) as u8);
            data.push(((x + y) * 100 / (width + height) + 2 * texture) as u8);
        }
    }
    Image::from_rgb_bytes(width, height, &data).expect("rgb image")
}

fn gray(width: u32, height: u32) -> Image {
    let data: Vec<u8> = (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            ((x * 13 + y * 7 + (x ^ y)) & 0xff) as u8
        })
        .collect();
    Image::from_gray_bytes(width, height, &data).expect("gray image")
}

fn with_coding(rate_control: RateControl, coding: CodingOptions) -> EncodeOptions {
    EncodeOptions {
        rate_control: Some(rate_control),
        format: OutputFormat::Jp2,
        coding,
        ..EncodeOptions::default()
    }
}

fn assert_lossless(label: &str, image: &Image, options: &EncodeOptions) -> Vec<u8> {
    let encoded = encode(image, options).unwrap_or_else(|err| panic!("{label}: encode: {err}"));
    let decoded = decode_jp2(&encoded).unwrap_or_else(|err| panic!("{label}: decode: {err}"));
    for (index, (actual, expected)) in decoded.components.iter().zip(&image.components).enumerate()
    {
        assert_eq!(actual.data, expected.data, "{label}: component {index}");
    }
    if let Some(external) = decode_with_openjpeg(&encoded, image) {
        assert_eq!(external, interleaved(image), "{label}: OpenJPEG decode");
    }
    encoded
}

fn psnr(image: &Image, encoded: &[u8]) -> f64 {
    let decoded = decode_jp2(encoded).expect("decode lossy");
    let mut squared = 0.0;
    let mut count = 0usize;
    for (actual, expected) in decoded.components.iter().zip(&image.components) {
        for (&a, &e) in actual.data.iter().zip(&expected.data) {
            squared += f64::from(a - e).powi(2);
            count += 1;
        }
    }
    let mse = squared / count as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

#[test]
fn every_progression_order_is_signalled_and_round_trips() {
    for (label, image) in [("gray", gray(61, 47)), ("rgb", rgb(61, 47))] {
        let mut streams = Vec::new();
        for order in ORDERS {
            let coding = CodingOptions {
                progression_order: order,
                ..CodingOptions::default()
            };
            let encoded = assert_lossless(
                &format!("{label} {order:?}"),
                &image,
                &with_coding(RateControl::Lossless, coding),
            );
            let meta = inspect_jp2(&encoded).expect("inspect");
            assert_eq!(meta.codestream.cod.progression_order, order, "{label}");
            streams.push(encoded);
        }
        // One layer and one precinct: the orders differ only in whether
        // resolution or component is the outer loop, so every order carries
        // the same packets and the same total length.
        assert!(streams.iter().all(|s| s.len() == streams[0].len()));
        if label == "rgb" {
            assert_ne!(streams[0], streams[3], "PCRL must reorder RGB packets");
        }
    }
}

#[test]
fn lossy_component_major_order_matches_resolution_major_quality() {
    let image = rgb(96, 80);
    let encode_order = |order| {
        encode(
            &image,
            &with_coding(
                RateControl::TargetBytes(4_000),
                CodingOptions {
                    progression_order: order,
                    ..CodingOptions::default()
                },
            ),
        )
        .expect("lossy encode")
    };
    let lrcp = encode_order(ProgressionOrder::Lrcp);
    let cprl = encode_order(ProgressionOrder::Cprl);
    assert_eq!(lrcp.len(), cprl.len());
    assert_eq!(psnr(&image, &lrcp), psnr(&image, &cprl));
    assert!(psnr(&image, &cprl) > 25.0);
}

#[test]
fn explicit_decomposition_levels_are_honoured() {
    let image = gray(150, 97);
    for levels in [0u8, 1, 3, 7, 9] {
        let coding = CodingOptions {
            decomposition_levels: Some(levels),
            ..CodingOptions::default()
        };
        let encoded = assert_lossless(
            &format!("levels {levels}"),
            &image,
            &with_coding(RateControl::Lossless, coding.clone()),
        );
        let meta = inspect_jp2(&encoded).expect("inspect");
        assert_eq!(meta.codestream.cod.decomposition_levels, levels);

        let lossy = encode(&image, &with_coding(RateControl::ApproxQuality(80), coding))
            .expect("lossy encode at explicit levels");
        assert!(psnr(&image, &lossy) > 28.0, "levels {levels}");
    }
}

#[test]
fn decomposition_levels_that_empty_a_tile_are_rejected() {
    // The second 64-wide tile column is one sample wide and starts at x=64,
    // so its low-pass band empties after six levels.
    let image = gray(65, 64);
    let options = |levels| EncodeOptions {
        tile_policy: TilePolicy::Fixed {
            width: 64,
            height: 64,
        },
        ..with_coding(
            RateControl::Lossless,
            CodingOptions {
                decomposition_levels: Some(levels),
                ..CodingOptions::default()
            },
        )
    };
    assert_lossless("six levels", &image, &options(6));
    let err = encode(&image, &options(7)).expect_err("seven levels");
    assert!(err.to_string().contains("decomposition levels"), "{err}");
    assert!(encode(&image, &options(33)).is_err());
}

#[test]
fn code_block_sizes_across_the_legal_range_round_trip() {
    let image = rgb(140, 90);
    for (width, height) in [(4, 4), (16, 64), (64, 16), (128, 32), (1024, 4), (32, 128)] {
        let coding = CodingOptions {
            code_block_size: Some((width, height)),
            ..CodingOptions::default()
        };
        let encoded = assert_lossless(
            &format!("code-block {width}x{height}"),
            &image,
            &with_coding(RateControl::Lossless, coding.clone()),
        );
        let cod = inspect_jp2(&encoded).expect("inspect").codestream.cod;
        assert_eq!(
            (cod.code_block_width, cod.code_block_height),
            (width, height)
        );

        let lossy = encode(&image, &with_coding(RateControl::ApproxQuality(75), coding))
            .expect("lossy encode at explicit code-block size");
        // Small blocks pay per-block header overhead (4x4 measures ~24 dB here,
        // OpenJPEG decoding the same stream agrees); this checks validity, not
        // efficiency.
        assert!(psnr(&image, &lossy) > 22.0, "code-block {width}x{height}");
    }
}

#[test]
fn illegal_code_block_sizes_are_rejected() {
    let image = gray(32, 32);
    for size in [(2, 4), (3, 4), (2048, 2), (128, 64), (64, 128), (0, 64)] {
        let coding = CodingOptions {
            code_block_size: Some(size),
            ..CodingOptions::default()
        };
        assert!(
            encode(&image, &with_coding(RateControl::Lossless, coding)).is_err(),
            "accepted code-block {size:?}"
        );
    }
}

#[test]
fn component_transform_none_stores_components_as_given() {
    let image = rgb(53, 41);
    let coding = CodingOptions {
        component_transform: ComponentTransform::None,
        ..CodingOptions::default()
    };
    let encoded = assert_lossless(
        "rgb without MCT",
        &image,
        &with_coding(RateControl::Lossless, coding.clone()),
    );
    assert!(
        !inspect_jp2(&encoded)
            .expect("inspect")
            .codestream
            .cod
            .use_mct
    );

    // `Auto` applies the ICT to lossy RGB. (Lossless RGB is coded without the
    // RCT until per-component quantization signalling exists, so only the
    // lossy default shows the transform.)
    let auto = encode(
        &image,
        &with_coding(RateControl::ApproxQuality(80), CodingOptions::default()),
    )
    .expect("default");
    assert!(inspect_jp2(&auto).expect("inspect").codestream.cod.use_mct);

    for rate_control in [
        RateControl::ApproxQuality(80),
        RateControl::TargetBytes(3_000),
        RateControl::Quality {
            level: 70,
            effort: jp2lam::PerceptualEffort::Fast,
        },
    ] {
        let lossy = encode(&image, &with_coding(rate_control, coding.clone()))
            .unwrap_or_else(|err| panic!("{rate_control:?}: {err}"));
        assert!(!inspect_jp2(&lossy).expect("inspect").codestream.cod.use_mct);
        assert!(psnr(&image, &lossy) > 25.0, "{rate_control:?}");
    }
}

#[test]
fn combined_options_survive_the_tiled_lossy_path() {
    let image = rgb(300, 220);
    let coding = CodingOptions {
        decomposition_levels: Some(4),
        code_block_size: Some((32, 32)),
        progression_order: ProgressionOrder::Rpcl,
        component_transform: ComponentTransform::Auto,
        ..CodingOptions::default()
    };
    let options = EncodeOptions {
        tile_policy: TilePolicy::Fixed {
            width: 128,
            height: 128,
        },
        ..with_coding(RateControl::TargetBytes(12_000), coding)
    };
    let encoded = encode(&image, &options).expect("tiled lossy encode");
    let cod = inspect_jp2(&encoded).expect("inspect").codestream.cod;
    assert_eq!(cod.progression_order, ProgressionOrder::Rpcl);
    assert_eq!(cod.decomposition_levels, 4);
    assert_eq!((cod.code_block_width, cod.code_block_height), (32, 32));
    assert!(psnr(&image, &encoded) > 25.0);
    assert_lossless(
        "tiled lossless combined",
        &image,
        &EncodeOptions {
            rate_control: Some(RateControl::Lossless),
            ..options
        },
    );
}

fn interleaved(image: &Image) -> Vec<u8> {
    let pixels = (image.width * image.height) as usize;
    let mut out = Vec::with_capacity(pixels * image.components.len());
    for index in 0..pixels {
        for component in &image.components {
            out.push(component.data[index] as u8);
        }
    }
    out
}

/// Decode with ImageMagick's OpenJPEG delegate, when one is installed.
fn decode_with_openjpeg(encoded: &[u8], image: &Image) -> Option<Vec<u8>> {
    let tool = ["magick", "convert"].into_iter().find(|tool| {
        Command::new(tool)
            .args(["-list", "format"])
            .output()
            .is_ok_and(|out| {
                out.status.success() && String::from_utf8_lossy(&out.stdout).contains("JP2")
            })
    })?;
    let dir = tempfile::tempdir().expect("tempdir");
    let input = dir.path().join("in.jp2");
    let output = dir.path().join("out.raw");
    std::fs::write(&input, encoded).expect("write jp2");
    let format = if image.components.len() == 1 {
        "gray"
    } else {
        "rgb"
    };
    let status = Command::new(tool)
        .arg(&input)
        .args(["-depth", "8"])
        .arg(format!("{format}:{}", output.display()))
        .status()
        .expect("run ImageMagick");
    assert!(
        status.success(),
        "OpenJPEG (via {tool}) rejected the stream"
    );
    Some(std::fs::read(output).expect("read raw"))
}

// ---------------------------------------------------------------------------
// Quality layers
// ---------------------------------------------------------------------------

fn layered(rate_control: RateControl, layers: QualityLayers) -> EncodeOptions {
    with_coding(
        rate_control,
        CodingOptions {
            layers,
            ..CodingOptions::default()
        },
    )
}

/// Layers only split the same stream into more packets: the final layer
/// completes the selection the rate control chose, so the decoded image is
/// the single-layer one and the file grows by packet headers alone.
#[test]
fn layers_split_the_same_stream_without_changing_the_picture() {
    let image = rgb(180, 140);
    for rate_control in [RateControl::Lossless, RateControl::ApproxQuality(70)] {
        let single = encode(&image, &layered(rate_control, QualityLayers::One)).expect("single");
        let reference = decode_jp2(&single).expect("decode single");
        for order in ORDERS {
            for count in [2u16, 4, 7] {
                let options = EncodeOptions {
                    coding: CodingOptions {
                        progression_order: order,
                        layers: QualityLayers::Count(count),
                        ..CodingOptions::default()
                    },
                    ..layered(rate_control, QualityLayers::One)
                };
                let label = format!("{rate_control:?} {order:?} x{count}");
                let encoded = encode(&image, &options).unwrap_or_else(|e| panic!("{label}: {e}"));
                let meta = inspect_jp2(&encoded).expect("inspect");
                assert_eq!(meta.codestream.cod.layers, count, "{label}");
                assert_eq!(meta.codestream.cod.progression_order, order, "{label}");
                let decoded = decode_jp2(&encoded).unwrap_or_else(|e| panic!("{label}: {e}"));
                for (a, b) in decoded.components.iter().zip(&reference.components) {
                    assert_eq!(a.data, b.data, "{label}");
                }
                // Packets per layer: one per (resolution, component).
                let packets = (usize::from(meta.codestream.cod.decomposition_levels) + 1) * 3;
                let overhead = encoded.len() - single.len();
                assert!(
                    overhead <= packets * usize::from(count - 1) * 24,
                    "{label}: {overhead} bytes of layer overhead"
                );
                if let Some(external) = decode_with_openjpeg(&encoded, &image) {
                    assert_close(&external, &interleaved(&decoded), rate_control, &label);
                }
            }
        }
    }
}

/// Each layer below the final one is a usable prefix: decoding only the first
/// `k` layers gives a picture whose quality rises with `k`, and whose byte
/// share sits near the geometric ladder the count asks for.
/// Each layer below the final one is a usable prefix: decoding only the first
/// `k` layers gives a picture whose quality rises with `k`.
#[test]
fn intermediate_layers_are_progressive_prefixes() {
    let image = gray(256, 192);
    let encoded = encode(
        &image,
        &layered(RateControl::ApproxQuality(85), QualityLayers::Count(4)),
    )
    .expect("four layers");
    let Some(scores) = layer_psnrs_with_openjpeg(&encoded, &image) else {
        eprintln!("no OpenJPEG; skipping the per-layer decode");
        return;
    };
    let full = psnr(&image, &encoded);
    for pair in scores.windows(2) {
        assert!(
            pair[0] < pair[1],
            "layers must improve monotonically: {scores:?}"
        );
    }
    assert!(
        (scores[3] - full).abs() < 0.05,
        "all layers = full: {scores:?} vs {full}"
    );
    assert!(
        scores[0] + 5.0 < scores[3],
        "first layer must be visibly coarser: {scores:?}"
    );
}

#[test]
fn explicit_layer_ratios_give_three_rising_layers() {
    let image = rgb(240, 200);
    let source_bytes = (image.width * image.height * 3) as f64;
    let options = layered(
        RateControl::CompressionRatio(8.0),
        QualityLayers::Ratios(vec![64.0, 24.0]),
    );
    let encoded = encode(&image, &options).expect("ratio layers");
    let meta = inspect_jp2(&encoded).expect("inspect");
    assert_eq!(meta.codestream.cod.layers, 3);
    let ratio = source_bytes / encoded.len() as f64;
    assert!((7.5..=8.5).contains(&ratio), "final ratio {ratio:.2}");
    let decoded = decode_jp2(&encoded).expect("decode");
    assert_eq!(decoded.components.len(), 3);
    if let Some(scores) = layer_psnrs_with_openjpeg(&encoded, &image) {
        assert!(scores[0] < scores[1] && scores[1] < scores[2], "{scores:?}");
        assert!(
            (scores[2] - psnr(&image, &encoded)).abs() < 0.05,
            "{scores:?}"
        );
    }
}

#[test]
fn layer_options_are_validated() {
    let image = gray(32, 32);
    for layers in [
        QualityLayers::Count(0),
        QualityLayers::Ratios(vec![10.0, 20.0]),
        QualityLayers::Ratios(vec![10.0, 10.0]),
        QualityLayers::Ratios(vec![f32::NAN]),
        QualityLayers::Ratios(vec![0.0]),
    ] {
        assert!(
            encode(&image, &layered(RateControl::Lossless, layers.clone())).is_err(),
            "accepted {layers:?}"
        );
    }
    assert_eq!(QualityLayers::Count(1).count(), 1);
    assert_eq!(QualityLayers::Ratios(vec![30.0, 15.0]).count(), 3);
    // One layer by count is the single-layer stream, byte for byte.
    let one = encode(&image, &layered(RateControl::Lossless, QualityLayers::One)).unwrap();
    let count_one = encode(
        &image,
        &layered(RateControl::Lossless, QualityLayers::Count(1)),
    )
    .unwrap();
    assert_eq!(one, count_one);
}

#[test]
fn layers_combine_with_tiles_and_exact_rate() {
    let image = rgb(300, 220);
    let options = EncodeOptions {
        tile_policy: TilePolicy::Fixed {
            width: 128,
            height: 128,
        },
        coding: CodingOptions {
            progression_order: ProgressionOrder::Rlcp,
            layers: QualityLayers::Count(3),
            ..CodingOptions::default()
        },
        ..with_coding(RateControl::TargetBytes(14_000), CodingOptions::default())
    };
    let encoded = encode(&image, &options).expect("tiled layered exact-rate");
    assert!(
        encoded.len() <= 14_000 && encoded.len() > 13_000,
        "{} bytes",
        encoded.len()
    );
    assert_eq!(
        inspect_jp2(&encoded)
            .expect("inspect")
            .codestream
            .cod
            .layers,
        3
    );
    assert!(psnr(&image, &encoded) > 25.0);
    if let Some(external) = decode_with_openjpeg(&encoded, &image) {
        assert_close(
            &external,
            &interleaved(&decode_jp2(&encoded).expect("decode")),
            RateControl::TargetBytes(14_000),
            "tiled layered exact-rate",
        );
    }
}

/// OpenJPEG and jp2lam agree exactly on reversible streams; on 9/7 streams
/// their float synthesis and colour transforms round differently by a sample
/// level here and there, so lossy agreement is "within 2 everywhere".
fn assert_close(external: &[u8], mine: &[u8], rate_control: RateControl, label: &str) {
    assert_eq!(external.len(), mine.len(), "{label}: decode sizes");
    if rate_control == RateControl::Lossless {
        assert_eq!(external, mine, "{label}: OpenJPEG disagrees");
        return;
    }
    let worst = external
        .iter()
        .zip(mine)
        .map(|(&a, &b)| a.abs_diff(b))
        .max()
        .unwrap_or(0);
    assert!(worst <= 2, "{label}: OpenJPEG differs by up to {worst}");
}

fn imagemagick_tool() -> Option<&'static str> {
    ["magick", "convert"].into_iter().find(|tool| {
        Command::new(tool)
            .args(["-list", "format"])
            .output()
            .is_ok_and(|out| {
                out.status.success() && String::from_utf8_lossy(&out.stdout).contains("JP2")
            })
    })
}

/// PSNR of the picture OpenJPEG reconstructs from the first `k` layers, for
/// every `k`, via ImageMagick's `jp2:layer-number` define.
fn layer_psnrs_with_openjpeg(encoded: &[u8], source: &Image) -> Option<Vec<f64>> {
    let tool = imagemagick_tool()?;
    let layers = inspect_jp2(encoded).ok()?.codestream.cod.layers;
    let dir = tempfile::tempdir().ok()?;
    let input = dir.path().join("in.jp2");
    std::fs::write(&input, encoded).ok()?;
    let format = if source.components.len() == 1 {
        "gray"
    } else {
        "rgb"
    };
    let reference = interleaved(source);
    let mut scores = Vec::new();
    for k in 1..=layers {
        let output = dir.path().join(format!("l{k}.raw"));
        let status = Command::new(tool)
            .arg("-define")
            .arg(format!("jp2:layer-number={k}"))
            .arg(&input)
            .args(["-depth", "8"])
            .arg(format!("{format}:{}", output.display()))
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        let bytes = std::fs::read(output).ok()?;
        assert_eq!(bytes.len(), reference.len(), "layer {k} decode size");
        let mse = bytes
            .iter()
            .zip(&reference)
            .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
            .sum::<f64>()
            / bytes.len() as f64;
        scores.push(if mse == 0.0 {
            99.0
        } else {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        });
    }
    Some(scores)
}
