//! An alpha channel: one component past the colour ones, coded on its own,
//! declared in the JP2 `cdef` box, and read back by this crate's decoder and
//! by OpenJPEG.

use jp2lam::{
    CodeBlockStyle, CodingOptions, EncodeOptions, Image, ImageView, OutputFormat, PerceptualEffort,
    QualityLayers, RateControl, TilePolicy, decode_jp2, encode, encode_view, inspect_jp2,
};
use std::process::Command;

fn rgba(width: u32, height: u32) -> Vec<u8> {
    (0..width * height)
        .flat_map(|i| {
            let (x, y) = (i % width, i / width);
            let t = ((x * 7 + y * 3) % 23) as u8;
            [
                (x * 255 / width) as u8 ^ t,
                (y * 255 / height) as u8 ^ t,
                ((x + y) * 255 / (width + height)) as u8 ^ (t << 1),
                // A soft disc of opacity with a hard edge of transparency.
                if (x * 2 + 1).abs_diff(width) < width / 2
                    && (y * 2 + 1).abs_diff(height) < height / 2
                {
                    (200 + (x ^ y) % 56) as u8
                } else {
                    (x % 3) as u8
                },
            ]
        })
        .collect()
}

fn gray_alpha(width: u32, height: u32) -> Vec<u8> {
    (0..width * height)
        .flat_map(|i| {
            let (x, y) = (i % width, i / width);
            [
                ((x * 29 + y * 17) % 256) as u8,
                if x > width / 4 {
                    255 - (y % 40) as u8
                } else {
                    0
                },
            ]
        })
        .collect()
}

fn options(rate_control: RateControl) -> EncodeOptions {
    EncodeOptions {
        rate_control: Some(rate_control),
        format: OutputFormat::Jp2,
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

fn psnr(expected: &[u8], decoded: &Image) -> f64 {
    let mine = interleaved(decoded);
    let mse = expected
        .iter()
        .zip(&mine)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
        .sum::<f64>()
        / expected.len() as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

#[test]
fn rgba_and_gray_alpha_round_trip_losslessly_with_a_cdef_box() {
    let (w, h) = (73u32, 58u32);
    let cases = [
        (
            "rgba",
            Image::from_rgba_bytes(w, h, &rgba(w, h)).expect("rgba"),
            4usize,
        ),
        (
            "gray+alpha",
            Image::from_gray_alpha_bytes(w, h, &gray_alpha(w, h)).expect("gray alpha"),
            2,
        ),
    ];
    for (label, image, channels) in &cases {
        assert!(image.has_alpha());
        let encoded = encode(image, &options(RateControl::Lossless)).expect(label);
        let meta = inspect_jp2(&encoded).expect("inspect");
        assert_eq!(meta.codestream.siz.components.len(), *channels, "{label}");
        let alpha = meta
            .in_data_alpha
            .unwrap_or_else(|| panic!("{label}: no cdef alpha"));
        assert_eq!(usize::from(alpha.component), channels - 1, "{label}");
        assert!(!alpha.premultiplied);
        // RGB keeps its colour transform; the alpha plane is outside it.
        assert!(
            !meta.codestream.cod.use_mct,
            "{label}: lossless RGB has no RCT yet"
        );

        let decoded = decode_jp2(&encoded).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(decoded.components.len(), *channels, "{label}");
        for (index, (a, b)) in decoded.components.iter().zip(&image.components).enumerate() {
            assert_eq!(a.data, b.data, "{label} component {index}");
        }
        decode_with_openjpeg(&encoded, &interleaved(image), w, h);
    }
}

#[test]
fn lossy_rgba_keeps_the_colour_transform_and_codes_alpha_separately() {
    let (w, h) = (120u32, 90u32);
    let source = rgba(w, h);
    let image = Image::from_rgba_bytes(w, h, &source).expect("rgba");
    for rate_control in [
        RateControl::ApproxQuality(70),
        RateControl::TargetBytes(9_000),
        RateControl::CompressionRatio(6.0),
    ] {
        let encoded = encode(&image, &options(rate_control))
            .unwrap_or_else(|e| panic!("{rate_control:?}: {e}"));
        let meta = inspect_jp2(&encoded).expect("inspect");
        assert!(
            meta.codestream.cod.use_mct,
            "{rate_control:?}: ICT on the colour planes"
        );
        assert_eq!(meta.in_data_alpha.map(|a| a.component), Some(3));
        let decoded = decode_jp2(&encoded).unwrap_or_else(|e| panic!("{rate_control:?}: {e}"));
        assert_eq!(decoded.components.len(), 4);
        let quality = psnr(&source, &decoded);
        assert!(quality > 27.0, "{rate_control:?}: {quality:.1} dB");
        if let RateControl::TargetBytes(target) = rate_control {
            assert!(encoded.len() as u64 <= target);
        }
        // Lossy: the alpha plane is only checked when the budget leaves it
        // intact, so the external check is reserved for the lossless tests.
    }
}

#[test]
fn alpha_combines_with_views_layers_tiles_and_styles() {
    let (w, h) = (160u32, 130u32);
    let source = rgba(w, h);
    let view = ImageView::from_rgba8_interleaved(w, h, &source).expect("view");
    assert!(view.has_alpha());
    let options = EncodeOptions {
        tile_policy: TilePolicy::Fixed {
            width: 64,
            height: 64,
        },
        coding: CodingOptions {
            layers: QualityLayers::Count(3),
            code_block_style: CodeBlockStyle {
                bypass: true,
                ..CodeBlockStyle::default()
            },
            ..CodingOptions::default()
        },
        ..options(RateControl::Lossless)
    };
    let encoded = encode_view(view, &options).expect("tiled layered bypass rgba");
    let decoded = decode_jp2(&encoded).expect("decode");
    assert_eq!(interleaved(&decoded), source);
    decode_with_openjpeg(&encoded, &source, w, h);

    let gray = gray_alpha(w, h);
    let view = ImageView::from_gray_alpha8_interleaved(w, h, &gray).expect("gray view");
    let encoded = encode_view(
        view,
        &EncodeOptions {
            rate_control: Some(RateControl::ApproxQuality(80)),
            ..options
        },
    )
    .expect("lossy gray alpha");
    let decoded = decode_jp2(&encoded).expect("decode gray alpha");
    assert_eq!(decoded.components.len(), 2);
    assert!(psnr(&gray, &decoded) > 30.0);
}

#[test]
fn alpha_rejects_what_it_cannot_do() {
    let (w, h) = (40u32, 30u32);
    let image = Image::from_rgba_bytes(w, h, &rgba(w, h)).expect("rgba");
    // Verified quality scores colour only.
    let err = encode(
        &image,
        &options(RateControl::Quality {
            level: 70,
            effort: PerceptualEffort::Fast,
        }),
    )
    .expect_err("perceptual with alpha");
    assert!(err.to_string().contains("alpha"), "{err}");

    // Two extra components are not "alpha".
    let mut five = image.clone();
    five.components.push(five.components[3].clone());
    assert!(!five.has_alpha());
    assert!(encode(&five, &options(RateControl::Lossless)).is_err());

    // Wrong buffer sizes are caught at construction.
    assert!(Image::from_rgba_bytes(w, h, &rgba(w, h)[1..]).is_err());
    assert!(ImageView::from_gray_alpha8_interleaved(w, h, &[0u8; 5]).is_err());
}

/// ImageMagick's OpenJPEG delegate as an oracle for images with alpha.
///
/// ImageMagick does not export the colour of translucent pixels unchanged
/// (its own lossless RGBA JP2 does not round-trip through `rgba:` either), so
/// this compares two things it does reproduce: the alpha plane exactly via
/// `-alpha extract`, and the full `rgba:` export of our file against the same
/// export of a file ImageMagick itself encoded losslessly from the same
/// pixels. Equal exports mean OpenJPEG reads our codestream and `cdef` as it
/// reads its own.
fn decode_with_openjpeg(encoded: &[u8], source: &[u8], width: u32, height: u32) -> Option<()> {
    let tool = ["magick", "convert"].into_iter().find(|tool| {
        Command::new(tool)
            .args(["-list", "format"])
            .output()
            .is_ok_and(|out| {
                out.status.success() && String::from_utf8_lossy(&out.stdout).contains("JP2")
            })
    })?;
    let channels = source.len() / (width as usize * height as usize);
    let format = match channels {
        2 => "graya",
        4 => "rgba",
        _ => unreachable!(),
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let ours = dir.path().join("ours.jp2");
    let theirs = dir.path().join("theirs.jp2");
    let raw = dir.path().join("src.raw");
    std::fs::write(&ours, encoded).expect("write jp2");
    std::fs::write(&raw, source).expect("write raw");
    let run = |args: &[&str]| {
        let status = Command::new(tool)
            .args(args)
            .status()
            .expect("run ImageMagick");
        assert!(status.success(), "ImageMagick failed: {args:?}");
    };
    run(&[
        "-size",
        &format!("{width}x{height}"),
        "-depth",
        "8",
        &format!("{format}:{}", raw.display()),
        "-quality",
        "100",
        theirs.to_str().unwrap(),
    ]);
    let export = |input: &std::path::Path, args: &[&str], name: &str| {
        let out = dir.path().join(name);
        let mut full: Vec<&str> = vec![input.to_str().unwrap()];
        full.extend_from_slice(args);
        let spec = format!(
            "{}:{}",
            if args.contains(&"-alpha") {
                "gray"
            } else {
                format
            },
            out.display()
        );
        full.push(&spec);
        run(&full);
        std::fs::read(out).expect("read export")
    };
    let alpha_ours = export(&ours, &["-alpha", "extract", "-depth", "8"], "a_ours.raw");
    let alpha_source: Vec<u8> = source
        .chunks_exact(channels)
        .map(|px| px[channels - 1])
        .collect();
    assert_eq!(alpha_ours, alpha_source, "OpenJPEG: alpha plane");
    let full_ours = export(&ours, &["-depth", "8"], "ours.raw");
    let full_theirs = export(&theirs, &["-depth", "8"], "theirs.raw");
    assert_eq!(
        full_ours, full_theirs,
        "OpenJPEG reads our file unlike its own"
    );
    Some(())
}
