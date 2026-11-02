//! Scalar coordinate-based template oracle for the packed-word fast path.
use jbig2enc_rust::jbig2arith::Jbig2ArithCoder;

fn sample(words: &[u32], width: usize, height: usize, x: isize, y: isize) -> bool {
    x >= 0
        && y >= 0
        && (x as usize) < width
        && (y as usize) < height
        && words[y as usize * width.div_ceil(32) + x as usize / 32] & (1 << (31 - x as usize % 32))
            != 0
}

fn scalar(words: &[u32], width: usize, height: usize) -> Vec<u8> {
    let mut coder = Jbig2ArithCoder::new();
    for y in 0..height as isize {
        for x in 0..width as isize {
            let mut context = 0usize;
            for (dy, offsets) in [(-2, -2..=2), (-1, -3..=3), (0, -4..=-1)] {
                for dx in offsets {
                    context =
                        (context << 1) | usize::from(sample(words, width, height, x + dx, y + dy));
                }
            }
            coder.encode_bit(context, sample(words, width, height, x, y));
        }
    }
    coder.flush(true);
    coder.as_bytes().to_vec()
}

#[test]
fn packed_word_windows_match_scalar_template_zero_even_with_dirty_tail_padding() {
    for width in [0usize, 1, 2, 7, 8, 15, 31, 32, 33, 63, 64, 65, 97] {
        for height in [0usize, 1, 2, 3, 9] {
            for pattern in 0..4 {
                let stride = width.div_ceil(32);
                let mut words = vec![0u32; stride * height];
                let mut random = 0x192a_73ef_u32;
                for y in 0..height {
                    for x in 0..width {
                        random = random.wrapping_mul(1664525).wrapping_add(1013904223);
                        if pattern == 1
                            || (pattern == 2 && (x + y) % 2 == 0)
                            || (pattern == 3 && random >> 29 < 3)
                        {
                            words[y * stride + x / 32] |= 1 << (31 - x % 32);
                        }
                    }
                }
                let expected = scalar(&words, width, height);
                for dirty in [false, true] {
                    if dirty && width % 32 != 0 {
                        for row in words.chunks_exact_mut(stride) {
                            *row.last_mut().unwrap() |= u32::MAX >> (width % 32);
                        }
                    }
                    let mut actual = Jbig2ArithCoder::new();
                    actual
                        .encode_generic_region_inner(
                            &words,
                            width,
                            height,
                            0,
                            &[(3, -1), (-3, -1), (2, -2), (-2, -2)],
                        )
                        .unwrap();
                    actual.flush(true);
                    assert_eq!(
                        actual.as_bytes(),
                        expected,
                        "{width}x{height}, pattern={pattern}, dirty={dirty}"
                    );
                }
            }
        }
    }
}

#[test]
fn long_blank_runs_and_sparse_islands_match_per_pixel_arithmetic() {
    // Long enough to exercise the low-Qe states and repeated renormalization,
    // then interrupt runs at/near packed-word and context-row boundaries.
    let width = 4097usize;
    let height = 257usize;
    let stride = width.div_ceil(32);
    let mut words = vec![0u32; stride * height];
    for sparse in [false, true] {
        if sparse {
            for (x, y) in [
                (0, 0),
                (31, 3),
                (32, 3),
                (63, 4),
                (64, 5),
                (4096, 100),
                (2050, 256),
            ] {
                words[y * stride + x / 32] |= 1 << (31 - x % 32);
            }
        }
        let expected = scalar(&words, width, height);
        let mut actual = Jbig2ArithCoder::new();
        actual
            .encode_generic_region_inner(
                &words,
                width,
                height,
                0,
                &[(3, -1), (-3, -1), (2, -2), (-2, -2)],
            )
            .unwrap();
        actual.flush(true);
        assert_eq!(actual.as_bytes(), expected, "sparse={sparse}");
    }
}
