//! A picture's box grown to the picture's own edge.
//!
//! The layout model draws a picture's box a few pixels inside the picture
//! about as often as outside it. The page pipeline overlays the box as a
//! photograph and binarizes everything else, so a box that is short leaves
//! the picture's outer rim to be binarized: a black frame around the overlay.
//! Each image box is therefore grown outward, a pixel row at a time, while
//! the row just outside still holds ink, up to a small limit and never into
//! a text box, and then padded by a few pixels.
//!
//! This is about what the overlay must cover, not about what the model
//! found: it adds no boxes and changes no labels.

use image::RgbImage;

use crate::engine::Detection;
use crate::types::LABEL_CLASSIFIER;

const INK_LUMA: u8 = 200;
const MIN_BOX_PX: f32 = 2.0;
const IMAGE_PAD_FRAC: f32 = 0.012;
const IMAGE_GROW_FRAC: f32 = 0.03;
const GROW_EDGES: [(usize, f32); 4] = [(0, -1.0), (2, 1.0), (1, -1.0), (3, 1.0)];

/// Grow every image box of `detections` to the edge of its picture on `image`.
pub fn grow_image_boxes_to_ink(detections: &mut [Detection], image: &RgbImage) {
    if image.width() == 0 || image.height() == 0 {
        return;
    }
    let luma = rgb_to_luma(image);
    grow_image_boxes(detections, &luma, image.width(), image.height());
}

fn grow_image_boxes(detections: &mut [Detection], luma: &[u8], width: u32, height: u32) {
    let blockers: Vec<[f32; 4]> = detections
        .iter()
        .filter(|d| !LABEL_CLASSIFIER.is_image_label(d))
        .map(|d| d.bbox)
        .collect();
    let max_grow = ((width.min(height) as f32) * IMAGE_GROW_FRAC).max(4.0);

    for det in detections.iter_mut() {
        if !LABEL_CLASSIFIER.is_image_label(det) {
            continue;
        }
        let original = det.bbox;
        let mut bbox = original;
        bbox = grow_rect_to_ink(bbox, luma, width, height, max_grow, &blockers);
        let pad = ((width.min(height) as f32) * IMAGE_PAD_FRAC)
            .max(2.0)
            .min(6.0);
        bbox[0] -= pad;
        bbox[1] -= pad;
        bbox[2] += pad;
        bbox[3] += pad;
        bbox = clamp_bbox_away(bbox, &blockers);
        // Blockers may limit newly-added padding, but must never collapse an
        // image box the model already detected. Captions and labels are often
        // legitimately inside maps and plates.
        bbox[0] = bbox[0].min(original[0]);
        bbox[1] = bbox[1].min(original[1]);
        bbox[2] = bbox[2].max(original[2]);
        bbox[3] = bbox[3].max(original[3]);
        det.bbox = clamp_bbox(bbox, width, height);
    }
}

fn grow_rect_to_ink(
    mut bbox: [f32; 4],
    luma: &[u8],
    width: u32,
    height: u32,
    max_grow: f32,
    blockers: &[[f32; 4]],
) -> [f32; 4] {
    let steps = max_grow.round().max(1.0) as i32;
    for _ in 0..steps {
        let mut grew = false;
        for (edge, step) in GROW_EDGES {
            let mut candidate = bbox;
            candidate[edge] += step;
            if strip_has_ink(luma, width, height, outside_strip(bbox, edge))
                && !blocked(&candidate, blockers)
            {
                bbox = candidate;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    bbox
}

fn outside_strip(bbox: [f32; 4], edge: usize) -> [f32; 4] {
    match edge {
        0 => [bbox[0] - 1.0, bbox[1], bbox[0], bbox[3]],
        1 => [bbox[0], bbox[1] - 1.0, bbox[2], bbox[1]],
        2 => [bbox[2], bbox[1], bbox[2] + 1.0, bbox[3]],
        _ => [bbox[0], bbox[3], bbox[2], bbox[3] + 1.0],
    }
}

fn strip_has_ink(luma: &[u8], width: u32, height: u32, strip: [f32; 4]) -> bool {
    let [x0, y0, x1, y1] = strip;
    let x0 = x0.floor().clamp(0.0, width.saturating_sub(1) as f32) as u32;
    let y0 = y0.floor().clamp(0.0, height.saturating_sub(1) as f32) as u32;
    let x1 = x1.ceil().clamp(0.0, width as f32).max(x0 as f32 + 1.0) as u32;
    let y1 = y1.ceil().clamp(0.0, height as f32).max(y0 as f32 + 1.0) as u32;
    if x1 <= x0 || y1 <= y0 {
        return false;
    }
    let mut ink = 0u32;
    let mut total = 0u32;
    for y in y0..y1 {
        let row = &luma[(y * width + x0) as usize..(y * width + x1) as usize];
        ink += row.iter().filter(|&&p| p < INK_LUMA).count() as u32;
        total += x1 - x0;
    }
    total > 0 && ink * 8 >= total
}

fn rgb_to_luma(image: &RgbImage) -> Vec<u8> {
    image
        .pixels()
        .map(|p| {
            let r = p.0[0] as u32;
            let g = p.0[1] as u32;
            let b = p.0[2] as u32;
            ((77 * r + 150 * g + 29 * b) >> 8) as u8
        })
        .collect()
}

fn clamp_bbox_away(mut bbox: [f32; 4], blockers: &[[f32; 4]]) -> [f32; 4] {
    for other in blockers {
        if iou(&bbox, other) <= 0.0 {
            continue;
        }
        if other[0] >= bbox[0] && other[0] < bbox[2] {
            bbox[2] = bbox[2].min(other[0]);
        }
        if other[2] <= bbox[2] && other[2] > bbox[0] {
            bbox[0] = bbox[0].max(other[2]);
        }
        if other[1] >= bbox[1] && other[1] < bbox[3] {
            bbox[3] = bbox[3].min(other[1]);
        }
        if other[3] <= bbox[3] && other[3] > bbox[1] {
            bbox[1] = bbox[1].max(other[3]);
        }
    }
    bbox
}

fn clamp_bbox(bbox: [f32; 4], width: u32, height: u32) -> [f32; 4] {
    let x0 = bbox[0].clamp(0.0, width as f32);
    let y0 = bbox[1].clamp(0.0, height as f32);
    let x1 = bbox[2].clamp(0.0, width as f32).max(x0 + MIN_BOX_PX);
    let y1 = bbox[3].clamp(0.0, height as f32).max(y0 + MIN_BOX_PX);
    [x0, y0, x1.min(width as f32), y1.min(height as f32)]
}

fn blocked(bbox: &[f32; 4], blockers: &[[f32; 4]]) -> bool {
    blockers.iter().any(|other| iou(bbox, other) > 0.02)
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = a[2].min(b[2]);
    let y1 = a[3].min(b[3]);
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let aa = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0);
    let ba = (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    if aa + ba <= inter {
        0.0
    } else {
        inter / (aa + ba - inter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentCategory, class_id_for};

    fn det(name: &str, category: ContentCategory, bbox: [f32; 4]) -> Detection {
        Detection {
            class_id: class_id_for(name).unwrap_or(0),
            class_name: Some(name.to_string()),
            confidence: 0.9,
            bbox,
            category,
            context: None,
        }
    }

    fn page_with_picture(x0: u32, y0: u32, x1: u32, y1: u32) -> RgbImage {
        let mut image = RgbImage::from_pixel(400, 400, image::Rgb([255, 255, 255]));
        for y in y0..y1 {
            for x in x0..x1 {
                image.put_pixel(x, y, image::Rgb([60, 60, 60]));
            }
        }
        image
    }

    #[test]
    fn a_box_drawn_inside_the_picture_grows_to_its_edge() {
        let image = page_with_picture(100, 100, 300, 250);
        let mut detections = vec![det(
            "image",
            ContentCategory::Image,
            [106.0, 107.0, 293.0, 244.0],
        )];
        grow_image_boxes_to_ink(&mut detections, &image);
        let b = detections[0].bbox;
        // The whole picture is inside, with a few pixels of padding and no more.
        assert!(
            b[0] <= 100.0 && b[1] <= 100.0 && b[2] >= 300.0 && b[3] >= 250.0,
            "{b:?}"
        );
        assert!(
            b[0] >= 92.0 && b[1] >= 92.0 && b[2] <= 308.0 && b[3] <= 258.0,
            "{b:?}"
        );
    }

    #[test]
    fn growth_stops_at_a_text_box_and_never_shrinks_the_box() {
        let image = page_with_picture(100, 100, 300, 250);
        let caption = [100.0, 246.0, 300.0, 270.0];
        let original = [106.0, 107.0, 293.0, 244.0];
        let mut detections = vec![
            det("image", ContentCategory::Image, original),
            det("text", ContentCategory::Text, caption),
        ];
        grow_image_boxes_to_ink(&mut detections, &image);
        let b = detections[0].bbox;
        assert!(b[3] <= caption[1] + 0.5, "grew into the caption: {b:?}");
        assert!(
            b[0] <= original[0]
                && b[1] <= original[1]
                && b[2] >= original[2]
                && b[3] >= original[3]
        );
        assert_eq!(detections[1].bbox, caption);
    }

    #[test]
    fn a_box_on_blank_paper_only_gets_its_padding() {
        let image = RgbImage::from_pixel(400, 400, image::Rgb([255, 255, 255]));
        let mut detections = vec![det(
            "image",
            ContentCategory::Image,
            [100.0, 100.0, 200.0, 200.0],
        )];
        grow_image_boxes_to_ink(&mut detections, &image);
        let b = detections[0].bbox;
        assert!(b[0] >= 94.0 && b[2] <= 206.0, "{b:?}");
    }
}
