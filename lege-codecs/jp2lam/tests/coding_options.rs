//! Public coding parameters: decomposition levels, code-block size, progression
//! order and the component transform reach the codestream, are signalled in
//! COD, and decode back - through this crate's decoder and, when ImageMagick
//! with OpenJPEG is installed, through an independent one.

use jp2lam::{
    CodingOptions, ComponentTransform, EncodeOptions, Image, OutputFormat, ProgressionOrder,
    RateControl, TilePolicy, decode_jp2, encode, inspect_jp2,
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
            &with_coding(RateControl::Lossless, coding),
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
            &with_coding(RateControl::Lossless, coding),
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
        &with_coding(RateControl::Lossless, coding),
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
        let lossy = encode(&image, &with_coding(rate_control, coding))
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
