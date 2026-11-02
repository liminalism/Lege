//! A conservative structural guard for explicit lossy substitution. It does
//! not recognize characters or establish input-pixel fidelity for lossy modes.
use crate::jbig2cc::analyze_page;
use crate::jbig2classify::count_enclosed_white_components_4;
use crate::jbig2sym::BitImage;

#[derive(Debug, PartialEq, Eq)]
struct Topology {
    components: usize,
    holes: usize,
    thin_endpoints: usize,
}

fn topology(bitmap: &BitImage) -> Option<Topology> {
    // A large unusual component is preserved through refinement instead of
    // triggering an unbounded topology scan in a lossy glyph heuristic.
    if bitmap.width > 256 || bitmap.height > 256 {
        return None;
    }
    let components = analyze_page(bitmap, 300, 0).extract_shape_refs().len();
    // Unlike the cheap matching signature, this also counts tiny counters.
    // Complementary 4-connected background with 8-connected foreground avoids
    // treating a diagonal corner contact as an open counter.
    let holes = count_enclosed_white_components_4(bitmap);
    let mut thin_endpoints = 0;
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            if !bitmap.get_usize(x, y) {
                continue;
            }
            let neighbors = (-1i32..=1)
                .flat_map(|dy| (-1i32..=1).map(move |dx| (dx, dy)))
                .filter(|&(dx, dy)| dx != 0 || dy != 0)
                .filter(|&(dx, dy)| {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    nx >= 0 && ny >= 0 && bitmap.get_usize(nx as usize, ny as usize)
                })
                .count();
            thin_endpoints += usize::from(neighbors == 1);
        }
    }
    Some(Topology {
        components,
        holes,
        thin_endpoints,
    })
}

pub(crate) fn substitution_preserves_topology(source: &BitImage, prototype: &BitImage) -> bool {
    if source == prototype {
        return true;
    }
    match (topology(source), topology(prototype)) {
        (Some(source), Some(prototype)) => source == prototype,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bitmap(rows: &[&str]) -> BitImage {
        let mut image = BitImage::new(rows[0].len() as u32, rows.len() as u32).unwrap();
        for (y, row) in rows.iter().enumerate() {
            for (x, pixel) in row.bytes().enumerate() {
                image.set_usize(x, y, pixel == b'1');
            }
        }
        image
    }
    #[test]
    fn one_pixel_counter_changes_are_rejected_even_in_small_glyphs() {
        let open = bitmap(&["11111", "10001", "10000", "10001", "11111"]);
        let closed = bitmap(&["11111", "10001", "10001", "10001", "11111"]);
        assert!(!substitution_preserves_topology(&open, &closed));
    }
    #[test]
    fn component_and_thin_endpoint_changes_are_rejected() {
        let single = bitmap(&["00000", "11111", "00000"]);
        let split = bitmap(&["00000", "11011", "00000"]);
        let fork = bitmap(&["00100", "00100", "11111"]);
        assert!(!substitution_preserves_topology(&single, &split));
        assert!(!substitution_preserves_topology(&single, &fork));
        assert!(substitution_preserves_topology(&single, &single));
    }

    #[test]
    fn counters_use_complementary_background_connectivity() {
        let open = bitmap(&["01111", "00001", "10001", "10001", "11111"]);
        let closed = bitmap(&["01111", "10001", "10001", "10001", "11111"]);
        assert_eq!(topology(&open).unwrap().holes, 0);
        assert_eq!(topology(&closed).unwrap().holes, 1);
        assert!(!substitution_preserves_topology(&open, &closed));
    }
}
