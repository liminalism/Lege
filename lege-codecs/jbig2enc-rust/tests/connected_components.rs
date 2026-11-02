//! Independent pixel-graph oracle for the span-graph provenance replacement.
use jbig2enc_rust::jbig2sym::BitImage;
use jbig2enc_rust::{CCImage, analyze_page};

fn pixel_components(image: &BitImage) -> Vec<Vec<(usize, usize)>> {
    let mut visited = vec![false; image.width * image.height];
    let mut groups = Vec::new();
    for y in 0..image.height {
        for x in 0..image.width {
            if visited[y * image.width + x] || !image.get_usize(x, y) {
                continue;
            }
            let mut pending = vec![(x, y)];
            visited[y * image.width + x] = true;
            let mut pixels = Vec::new();
            while let Some((x, y)) = pending.pop() {
                pixels.push((x, y));
                for ny in y.saturating_sub(1)..=(y + 1).min(image.height - 1) {
                    for nx in x.saturating_sub(1)..=(x + 1).min(image.width - 1) {
                        let offset = ny * image.width + nx;
                        if !visited[offset] && image.get_usize(nx, ny) {
                            visited[offset] = true;
                            pending.push((nx, ny));
                        }
                    }
                }
            }
            pixels.sort_unstable();
            groups.push(pixels);
        }
    }
    groups.sort_unstable();
    groups
}

fn extracted_pixels(components: &CCImage) -> Vec<Vec<(usize, usize)>> {
    let mut groups = Vec::new();
    for shape in components.extract_shape_refs() {
        let bitmap = components.get_bitmap_for_cc(shape.ccid).unwrap();
        let mut pixels = Vec::new();
        for y in 0..bitmap.height {
            for x in 0..bitmap.width {
                if bitmap.get_usize(x, y) {
                    pixels.push((x + shape.bbox.xmin as usize, y + shape.bbox.ymin as usize));
                }
            }
        }
        assert_eq!(pixels.len(), shape.black_pixels);
        pixels.sort_unstable();
        groups.push(pixels);
    }
    groups.sort_unstable();
    groups
}

#[test]
fn span_components_equal_pixel_graph_on_odd_widths_and_random_patterns() {
    for width in [1, 2, 7, 8, 31, 32, 33, 65, 129] {
        for seed in 0..24 {
            let mut image = BitImage::new(width, 29).unwrap();
            let mut state = 0x8514_2973_u32 ^ seed;
            for y in 0..image.height {
                for x in 0..image.width {
                    state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                    image.set_usize(x, y, state >> 28 < 4 + seed % 8);
                }
            }
            let expected = pixel_components(&image);
            let actual = analyze_page(&image, 300, 0);
            assert_eq!(
                expected,
                extracted_pixels(&actual),
                "width={width} seed={seed}"
            );
        }
    }
}

#[test]
fn staged_unsorted_runs_normalize_to_the_same_foreground_set() {
    let mut components = CCImage::new(33, 9, 300);
    for (y, left, right) in [
        (5, 28, 50),
        (0, -5, 4),
        (0, 3, 7),
        (1, 8, 9),
        (5, 28, 30),
        (9, 0, 20),
    ] {
        components.add_single_run(y, left, right);
    }
    components.analyze(0);
    let mut expected = BitImage::new(33, 9).unwrap();
    for x in 0..8 {
        expected.set(x, 0, true);
    }
    for x in 8..10 {
        expected.set(x, 1, true);
    }
    for x in 28..33 {
        expected.set(x, 5, true);
    }
    assert_eq!(pixel_components(&expected), extracted_pixels(&components));
}

#[test]
fn optional_grid_regrouping_preserves_pixels_and_descriptor_offsets() {
    let mut image = BitImage::new(193, 81).unwrap();
    for x in 2..191 {
        image.set(x, 5, true);
    }
    for (x, y) in [(2, 30), (7, 31), (191, 79), (192, 80)] {
        image.set(x, y, true);
    }
    let mut components = analyze_page(&image, 300, 0);
    components.largesize = 32;
    components.merge_and_split_ccs();
    components.sort_in_reading_order();
    let mut restored = BitImage::new(193, 81).unwrap();
    let mut total = 0;
    for group in extracted_pixels(&components) {
        for (x, y) in group {
            assert!(!restored.get_usize(x, y), "overlapping regrouped pixels");
            restored.set_usize(x, y, true);
            total += 1;
        }
    }
    assert_eq!(total, image.count_ones());
    assert_eq!(restored, image);
}

#[test]
fn empty_solid_and_diagonal_inputs_are_preserved_without_cleanup() {
    for pattern in 0..3 {
        let mut image = BitImage::new(65, 65).unwrap();
        for y in 0..65 {
            for x in 0..65 {
                image.set(x, y, pattern == 1 || (pattern == 2 && x == y));
            }
        }
        assert_eq!(
            pixel_components(&image),
            extracted_pixels(&analyze_page(&image, 300, 0))
        );
    }
}

#[test]
fn sparse_staged_coordinates_do_not_allocate_by_page_height_or_label_id() {
    let mut components = CCImage::new(3, i32::MAX, 300);
    components.add_single_run(1_000_000, 0, 0);
    components.add_single_run(1_000_001, 1, 1);
    components.analyze(0);
    assert_eq!(components.ccs.len(), 1);
    assert_eq!(components.ccs[0].npix, 2);
    components.runs[0].ccid = i32::MAX;
    components.runs[1].ccid = i32::MAX;
    components.make_ccs_from_ccids();
    assert_eq!(components.ccs.len(), 1);
    assert_eq!(components.ccs[0].npix, 2);
}

#[test]
fn maximal_runs_cover_each_row_without_crossing_white_pixels() {
    let mut image = BitImage::new(97, 4).unwrap();
    let intervals = [(0, 0, 1), (0, 31, 34), (0, 95, 97), (2, 9, 17)];
    for (row, left, end) in intervals {
        for column in left..end {
            image.set_usize(column, row, true);
        }
    }
    let mut staged = CCImage::new(97, 4, 240);
    staged.add_bitmap_runs(&image);
    let spans: Vec<_> = staged
        .runs
        .iter()
        .map(|r| (r.y as usize, r.x1 as usize, (r.x2 + 1) as usize))
        .collect();
    assert_eq!(spans, intervals);
    assert_eq!(staged.runs.iter().map(|r| r.x2 - r.x1 + 1).sum::<i32>(), 14);
}

#[test]
fn diagonal_contacts_connect_but_a_white_gap_separates_components() {
    let mut image = BitImage::new(17, 13).unwrap();
    for point in [(0, 0), (1, 1), (2, 2), (10, 8), (11, 8), (12, 9)] {
        image.set_usize(point.0, point.1, true);
    }
    let extracted = analyze_page(&image, 240, 0);
    assert_eq!(extracted_pixels(&extracted), pixel_components(&image));
    assert_eq!(
        extracted.ccs.iter().map(|c| c.npix).collect::<Vec<_>>(),
        [3, 3]
    );
    // Stable traversal matters to legacy symbol IDs and byte output.
    for _ in 0..5 {
        let repeated = analyze_page(&image, 240, 0);
        assert_eq!(
            repeated
                .ccs
                .iter()
                .map(|c| (c.bb, c.npix, c.frun, c.nrun))
                .collect::<Vec<_>>(),
            extracted
                .ccs
                .iter()
                .map(|c| (c.bb, c.npix, c.frun, c.nrun))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn materialized_shapes_use_half_open_bounds_and_original_page_positions() {
    let mut staged = CCImage::new(71, 23, 240);
    for row in 14..18 {
        staged.add_single_run(row, 63, 69);
    }
    staged.add_single_run(2, 4, 5);
    staged.analyze(0);
    let shapes = jbig2enc_rust::extract_symbols_for_jbig2(&staged);
    let mut geometry: Vec<_> = shapes
        .iter()
        .map(|(bitmap, x, y, w, h)| {
            assert_eq!((bitmap.width, bitmap.height), (*w as usize, *h as usize));
            assert_eq!(bitmap.count_ones(), (*w * *h) as usize);
            (*x, *y, *w, *h)
        })
        .collect();
    geometry.sort_unstable();
    assert_eq!(geometry, [(4, 2, 2, 1), (63, 14, 7, 4)]);
}

#[test]
fn cleanup_is_explicit_and_preserves_components_above_the_pixel_threshold() {
    let mut image = BitImage::new(53, 19).unwrap();
    for (x, y) in [(1, 1), (10, 4), (11, 4), (22, 8), (23, 8), (24, 8), (25, 8)] {
        image.set_usize(x, y, true);
    }
    let exact = analyze_page(&image, 300, 0);
    assert_eq!(extracted_pixels(&exact), pixel_components(&image));
    assert_eq!(
        exact
            .extract_shape_refs()
            .iter()
            .map(|s| s.black_pixels)
            .sum::<usize>(),
        7
    );
    let cleaned = analyze_page(&image, 300, 1);
    assert_eq!(cleaned.tinysize, 3);
    assert_eq!(
        extracted_pixels(&cleaned),
        [vec![(22, 8), (23, 8), (24, 8), (25, 8)]]
    );
    // The same source remains untouched: preprocessing is confined to output.
    assert_eq!(image.count_ones(), 7);
}
