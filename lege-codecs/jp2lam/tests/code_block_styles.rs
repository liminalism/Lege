//! The six Annex D code-block coding switches: signalled in COD, honoured by
//! the encoder, decoded back by this crate and, when ImageMagick's OpenJPEG
//! is installed, by an independent decoder.

use jp2lam::{
    CodeBlockStyle, CodingOptions, EncodeOptions, Image, OutputFormat, QualityLayers, RateControl,
    TilePolicy, decode_jp2, encode, inspect_jp2,
};
use std::process::Command;

fn styles() -> Vec<(&'static str, CodeBlockStyle)> {
    let off = CodeBlockStyle::default();
    vec![
        ("default", off),
        (
            "bypass",
            CodeBlockStyle {
                bypass: true,
                ..off
            },
        ),
        (
            "reset",
            CodeBlockStyle {
                reset_contexts: true,
                ..off
            },
        ),
        (
            "termall",
            CodeBlockStyle {
                terminate_each_pass: true,
                ..off
            },
        ),
        (
            "causal",
            CodeBlockStyle {
                vertical_causal: true,
                ..off
            },
        ),
        (
            "pterm",
            CodeBlockStyle {
                predictable_termination: true,
                ..off
            },
        ),
        (
            "segsym",
            CodeBlockStyle {
                segmentation_symbols: true,
                ..off
            },
        ),
        (
            "bypass+termall",
            CodeBlockStyle {
                bypass: true,
                terminate_each_pass: true,
                ..off
            },
        ),
        (
            "bypass+reset",
            CodeBlockStyle {
                bypass: true,
                reset_contexts: true,
                ..off
            },
        ),
        (
            "bypass+pterm",
            CodeBlockStyle {
                bypass: true,
                predictable_termination: true,
                ..off
            },
        ),
        (
            "termall+pterm+segsym",
            CodeBlockStyle {
                terminate_each_pass: true,
                predictable_termination: true,
                segmentation_symbols: true,
                ..off
            },
        ),
        (
            "all",
            CodeBlockStyle {
                bypass: true,
                reset_contexts: true,
                terminate_each_pass: true,
                vertical_causal: true,
                predictable_termination: true,
                segmentation_symbols: true,
            },
        ),
    ]
}

/// Full 8-bit range with texture, so code-blocks reach the ten-plus passes
/// where bypass starts coding raw.
fn gray(width: u32, height: u32) -> Image {
    let data: Vec<u8> = (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            ((x * 29 + y * 17) % 256) as u8 ^ ((x ^ y) & 0x3f) as u8
        })
        .collect();
    Image::from_gray_bytes(width, height, &data).expect("gray")
}

fn rgb(width: u32, height: u32) -> Image {
    let data: Vec<u8> = (0..width * height)
        .flat_map(|i| {
            let (x, y) = (i % width, i / width);
            let t = ((x * 7 + y * 3) % 23) as u8;
            [
                (x * 255 / width) as u8 ^ t,
                (y * 255 / height) as u8 ^ t,
                ((x + y) * 255 / (width + height)) as u8 ^ (t << 1),
            ]
        })
        .collect();
    Image::from_rgb_bytes(width, height, &data).expect("rgb")
}

fn options(rate_control: RateControl, style: CodeBlockStyle) -> EncodeOptions {
    EncodeOptions {
        rate_control: Some(rate_control),
        format: OutputFormat::Jp2,
        coding: CodingOptions {
            code_block_style: style,
            ..CodingOptions::default()
        },
        ..EncodeOptions::default()
    }
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

fn psnr(image: &Image, decoded: &Image) -> f64 {
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
fn every_style_is_signalled_and_round_trips_losslessly() {
    let images = [("gray", gray(97, 61)), ("rgb", rgb(80, 70))];
    let mut default_sizes = Vec::new();
    for (name, style) in styles() {
        for (kind, image) in &images {
            let label = format!("{name} {kind}");
            let encoded = encode(image, &options(RateControl::Lossless, style))
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            let meta = inspect_jp2(&encoded).expect("inspect");
            assert_eq!(meta.codestream.cod.code_block_style, style, "{label}");
            let decoded = decode_jp2(&encoded).unwrap_or_else(|e| panic!("{label}: decode: {e}"));
            for (a, b) in decoded.components.iter().zip(&image.components) {
                assert_eq!(a.data, b.data, "{label}");
            }
            if let Some(external) = decode_with_openjpeg(&encoded, image) {
                assert_eq!(external, interleaved(image), "{label}: OpenJPEG");
            }
            if name == "default" {
                default_sizes.push(encoded.len());
            } else {
                // Every switch costs a little; none should cost much.
                let base = default_sizes[if *kind == "gray" { 0 } else { 1 }];
                assert!(
                    encoded.len() < base + base / 5,
                    "{label}: {} bytes against {base} for the default style",
                    encoded.len()
                );
            }
        }
    }
}

#[test]
fn every_style_round_trips_lossy_within_tolerance() {
    let image = rgb(120, 90);
    for (name, style) in styles() {
        for rate_control in [
            RateControl::ApproxQuality(65),
            RateControl::TargetBytes(5_000),
        ] {
            let label = format!("{name} {rate_control:?}");
            let encoded = encode(&image, &options(rate_control, style))
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            let decoded = decode_jp2(&encoded).unwrap_or_else(|e| panic!("{label}: decode: {e}"));
            let quality = psnr(&image, &decoded);
            assert!(quality > 24.0, "{label}: {quality:.1} dB");
            if let Some(external) = decode_with_openjpeg(&encoded, &image) {
                let mine = interleaved(&decoded);
                let worst = external
                    .iter()
                    .zip(&mine)
                    .map(|(&a, &b)| a.abs_diff(b))
                    .max()
                    .unwrap_or(0);
                assert!(worst <= 2, "{label}: OpenJPEG differs by up to {worst}");
            }
        }
    }
}

/// Codeword segments cross layer boundaries: a raw segment may start in one
/// layer and end in the next, and a termination inside a layer splits that
/// layer's contribution into several signalled lengths.
#[test]
fn styles_combine_with_quality_layers_and_tiles() {
    let image = gray(150, 110);
    for (name, style) in styles() {
        let base = options(RateControl::Lossless, style);
        let layered = EncodeOptions {
            tile_policy: TilePolicy::Fixed {
                width: 64,
                height: 64,
            },
            coding: CodingOptions {
                layers: QualityLayers::Count(4),
                ..base.coding.clone()
            },
            ..base
        };
        let encoded = encode(&image, &layered).unwrap_or_else(|e| panic!("{name}: {e}"));
        let meta = inspect_jp2(&encoded).expect("inspect");
        assert_eq!(meta.codestream.cod.layers, 4);
        assert_eq!(meta.codestream.cod.code_block_style, style);
        let decoded = decode_jp2(&encoded).unwrap_or_else(|e| panic!("{name}: decode: {e}"));
        assert_eq!(
            decoded.components[0].data, image.components[0].data,
            "{name}"
        );
        if let Some(external) = decode_with_openjpeg(&encoded, &image) {
            assert_eq!(external, interleaved(&image), "{name}: OpenJPEG");
        }

        let lossy = encode(
            &image,
            &EncodeOptions {
                rate_control: Some(RateControl::ApproxQuality(70)),
                ..layered
            },
        )
        .unwrap_or_else(|e| panic!("{name} lossy: {e}"));
        let decoded = decode_jp2(&lossy).unwrap_or_else(|e| panic!("{name} lossy decode: {e}"));
        assert!(psnr(&image, &decoded) > 26.0, "{name} lossy");
    }
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
