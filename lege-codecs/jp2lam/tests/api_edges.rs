//! Library-facing edges: thread limits, wide images, and lossy coding above
//! 8 bits per sample.

use jp2lam::{
    EncodeOptions, Image, ImageView, OutputFormat, RateControl, ResourceLimits, TilePolicy,
    decode_jp2, encode, encode_view, encode_view_to_writer, inspect_jp2,
};

fn textured_gray(width: u32, height: u32) -> Image {
    let data: Vec<u8> = (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            ((x * 3 + y * 5) % 200 + (x ^ y) % 37) as u8
        })
        .collect();
    Image::from_gray_bytes(width, height, &data).expect("gray")
}

/// `max_threads` now sizes a pool for the whole encode instead of only gating
/// component parallelism. The output must not depend on it.
#[test]
fn thread_limit_does_not_change_output() {
    let image = textured_gray(300, 200);
    for rate_control in [
        RateControl::Lossless,
        RateControl::ApproxQuality(70),
        RateControl::TargetBytes(9_000),
    ] {
        let options = |max_threads| EncodeOptions {
            rate_control: Some(rate_control),
            tile_policy: TilePolicy::Fixed {
                width: 128,
                height: 128,
            },
            resource_limits: ResourceLimits {
                max_threads,
                ..ResourceLimits::default()
            },
            ..EncodeOptions::default()
        };
        let unlimited = encode(&image, &options(None)).expect("unlimited");
        assert_eq!(
            encode(&image, &options(Some(1))).expect("one thread"),
            unlimited
        );
        assert_eq!(
            encode(&image, &options(Some(3))).expect("three threads"),
            unlimited
        );

        let mut streamed = Vec::new();
        encode_view_to_writer(
            image.as_view().expect("view"),
            &options(Some(2)),
            &mut streamed,
        )
        .expect("writer path under a thread limit");
        assert_eq!(streamed, unlimited, "{rate_control:?} writer path");
    }
}

/// A 40000-sample-wide image cannot be one tile with one precinct per
/// resolution; `Auto` splits it at 32768 and the result decodes exactly.
#[test]
fn images_wider_than_a_precinct_encode_with_auto_tiles() {
    let image = textured_gray(40_000, 6);
    let single = EncodeOptions {
        rate_control: Some(RateControl::Lossless),
        ..EncodeOptions::default()
    };
    assert!(encode(&image, &single).is_err());

    let auto = EncodeOptions {
        tile_policy: TilePolicy::Auto,
        ..single
    };
    let encoded = encode(&image, &auto).expect("auto tiles");
    let meta = inspect_jp2(&encoded).expect("inspect");
    assert_eq!(meta.codestream.siz.tile_width, 32_768);
    let decoded = decode_jp2(&encoded).expect("decode");
    assert_eq!(decoded.components[0].data, image.components[0].data);
}

fn psnr_at_depth(original: &[u16], decoded: &[i32], bits: u32) -> f64 {
    let peak = f64::from((1u32 << bits) - 1);
    let mse = original
        .iter()
        .zip(decoded)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
        .sum::<f64>()
        / original.len() as f64;
    10.0 * (peak * peak / mse).log10()
}

/// Lossy 9/7 coding was accepted at 10-16 bits but only lossless had
/// round-trip coverage there.
#[test]
fn lossy_high_bit_depth_round_trips_at_declared_precision() {
    let (width, height) = (96u32, 72u32);
    for bits in [10u32, 12, 14, 16] {
        let max = (1u32 << bits) - 1;
        let gray: Vec<u16> = (0..width * height)
            .map(|i| {
                let (x, y) = (i % width, i / width);
                ((x * max / width + y * max / height) / 2 + (x * 7 % 13) * (max / 400)).min(max)
                    as u16
            })
            .collect();
        let rgb: Vec<u16> = gray
            .iter()
            .flat_map(|&v| [v, (max as u16).saturating_sub(v), v / 2 + (max / 4) as u16])
            .collect();

        let views = [
            (
                "gray",
                ImageView::from_gray16(width, height, &gray, bits).expect("gray view"),
            ),
            (
                "rgb",
                ImageView::from_rgb16_interleaved(width, height, &rgb, bits).expect("rgb view"),
            ),
        ];
        for (label, view) in views {
            for rate_control in [
                RateControl::ApproxQuality(85),
                RateControl::CompressionRatio(8.0),
            ] {
                let options = EncodeOptions {
                    rate_control: Some(rate_control),
                    format: OutputFormat::Jp2,
                    ..EncodeOptions::default()
                };
                let encoded = encode_view(view.clone(), &options)
                    .unwrap_or_else(|err| panic!("{label} {bits}-bit {rate_control:?}: {err}"));
                let decoded = decode_jp2(&encoded).expect("decode");
                assert_eq!(decoded.components.len(), view.components.len());
                for (index, component) in decoded.components.iter().enumerate() {
                    assert_eq!(component.precision, bits, "{label} {bits}-bit");
                    let original: Vec<u16> = if label == "gray" {
                        gray.clone()
                    } else {
                        rgb.iter().skip(index).step_by(3).copied().collect()
                    };
                    let psnr = psnr_at_depth(&original, &component.data, bits);
                    assert!(
                        psnr > 35.0,
                        "{label} {bits}-bit {rate_control:?} component {index}: {psnr:.1} dB"
                    );
                }
                if let RateControl::CompressionRatio(ratio) = rate_control {
                    let source_bytes =
                        (width * height) as f64 * view.components.len() as f64 * f64::from(bits)
                            / 8.0;
                    let achieved = source_bytes / encoded.len() as f64;
                    assert!(
                        achieved >= f64::from(ratio) * 0.95,
                        "{label} {bits}-bit ratio {achieved:.2}"
                    );
                }
            }
        }
    }
}
