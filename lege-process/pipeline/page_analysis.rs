use image::RgbImage;

use crate::engine::Detection;
use crate::margin::ContentBounds;
use crate::pipeline::config::PipelineConfig;
use crate::pipeline::helper_functions::rounded_clamped_bbox;
use crate::types::LabelClassifier;

pub const BLANK_PAGE_FALLBACK_THRESHOLD: u8 = 96;

#[derive(Debug, Clone, Copy, Default)]
pub struct PageClassification {
    pub content_bounds: Option<ContentBounds>,
    pub is_blank: bool,
    pub is_full_page_image: bool,
}

pub fn compute_pixel_bounds_for_margin(
    image: &RgbImage,
    config: &PipelineConfig,
) -> Option<ContentBounds> {
    let binarized = margin_ink_mask(image, config);

    crate::margin::calculate_content_bounds_from_binary_mask(
        &binarized,
        image.width(),
        image.height(),
    )
}

fn margin_ink_mask(image: &RgbImage, config: &PipelineConfig) -> Vec<u8> {
    let options = crate::pipeline::policies::binarize_options_for(config, false);

    let mut binarized = crate::color::binarization::binarize_image_raw(
        image.as_raw(),
        image.width() as usize,
        image.height() as usize,
        &options,
    );

    if binarized.is_empty() {
        return binarized;
    }

    if binarized.iter().any(|&b| b > 1) {
        for value in &mut binarized {
            *value = if *value > 128 { 1 } else { 0 };
        }
    }

    // Infer the output polarity from the source luminance corresponding to
    // each binary value. This remains correct across CPU/GPU encodings and
    // explicit inversion, while avoiding the "minority is ink" assumption
    // that loses full-bleed dark pages.
    let source = image.as_raw();
    let mut luma_sum = [0u64; 2];
    let mut value_count = [0u64; 2];
    for (index, &value) in binarized.iter().enumerate() {
        let source_index = index.saturating_mul(3);
        if source_index + 2 >= source.len() {
            break;
        }
        let luma = (299u64 * source[source_index] as u64
            + 587u64 * source[source_index + 1] as u64
            + 114u64 * source[source_index + 2] as u64)
            / 1000;
        let bucket = usize::from(value != 0);
        luma_sum[bucket] += luma;
        value_count[bucket] += 1;
    }
    let foreground_value = match (value_count[0], value_count[1]) {
        (0, 0) => return Vec::new(),
        (0, _) => u8::from(luma_sum[1] / value_count[1] < 128),
        (_, 0) => 1 - u8::from(luma_sum[0] / value_count[0] < 128),
        _ => {
            let mean_zero = luma_sum[0] as f64 / value_count[0] as f64;
            let mean_one = luma_sum[1] as f64 / value_count[1] as f64;
            u8::from(mean_one < mean_zero)
        }
    };
    for value in &mut binarized {
        *value = u8::from(*value == foreground_value);
    }

    binarized
}

/// Finds substantial ink that is not covered by any layout detection.
///
/// The document crop template remains layout-driven. These bounds are only a
/// per-page safety guard, preventing a missed illustration or text block from
/// being clipped. Small peripheral furniture, specks, and thin scan borders are
/// deliberately rejected.
pub(crate) fn compute_missed_pixel_bounds_for_margin(
    image: &RgbImage,
    detections: &[Detection],
    config: &PipelineConfig,
) -> Option<ContentBounds> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width == 0 || height == 0 {
        return None;
    }

    let mut residual = margin_ink_mask(image, config);
    if residual.len() != width.saturating_mul(height) {
        return None;
    }

    const DETECTION_PAD: usize = 5;
    for det in detections {
        let x1 = (det.bbox[0].floor().max(0.0) as usize).saturating_sub(DETECTION_PAD);
        let y1 = (det.bbox[1].floor().max(0.0) as usize).saturating_sub(DETECTION_PAD);
        let x2 = ((det.bbox[2].ceil().max(0.0) as usize).saturating_add(DETECTION_PAD)).min(width);
        let y2 = ((det.bbox[3].ceil().max(0.0) as usize).saturating_add(DETECTION_PAD)).min(height);
        for y in y1.min(height)..y2 {
            let start = y * width + x1.min(width);
            let end = y * width + x2;
            residual[start..end].fill(0);
        }
    }

    // Join neighboring glyphs into lines/blocks before component analysis.
    const DILATE_RADIUS: usize = 3;
    let mut horizontal = vec![0u8; residual.len()];
    for y in 0..height {
        let row = y * width;
        let mut last_ink: Option<usize> = None;
        for x in 0..width {
            if residual[row + x] != 0 {
                last_ink = Some(x);
            }
            if last_ink.is_some_and(|last| x.saturating_sub(last) <= DILATE_RADIUS) {
                horizontal[row + x] = 1;
            }
        }
        last_ink = None;
        for x in (0..width).rev() {
            if residual[row + x] != 0 {
                last_ink = Some(x);
            }
            if last_ink.is_some_and(|last| last.saturating_sub(x) <= DILATE_RADIUS) {
                horizontal[row + x] = 1;
            }
        }
    }

    let mut dilated = vec![0u8; residual.len()];
    for x in 0..width {
        let mut last_ink: Option<usize> = None;
        for y in 0..height {
            if horizontal[y * width + x] != 0 {
                last_ink = Some(y);
            }
            if last_ink.is_some_and(|last| y.saturating_sub(last) <= DILATE_RADIUS) {
                dilated[y * width + x] = 1;
            }
        }
        last_ink = None;
        for y in (0..height).rev() {
            if horizontal[y * width + x] != 0 {
                last_ink = Some(y);
            }
            if last_ink.is_some_and(|last| last.saturating_sub(y) <= DILATE_RADIUS) {
                dilated[y * width + x] = 1;
            }
        }
    }

    let page_area = width.saturating_mul(height);
    let min_bbox_area = ((page_area as f32) * 0.005).ceil() as usize;
    let min_ink_area = ((page_area as f32) * 0.001).ceil() as usize;
    let thin_edge_w = ((width as f32) * 0.02).ceil() as usize;
    let thin_edge_h = ((height as f32) * 0.02).ceil() as usize;
    let mut visited = vec![false; dilated.len()];
    let mut stack = Vec::new();
    let mut combined: Option<ContentBounds> = None;

    for seed in 0..dilated.len() {
        if dilated[seed] == 0 || visited[seed] {
            continue;
        }
        visited[seed] = true;
        stack.clear();
        stack.push(seed);
        let mut min_x = width;
        let mut min_y = height;
        let mut max_x = 0usize;
        let mut max_y = 0usize;
        let mut ink_area = 0usize;

        while let Some(index) = stack.pop() {
            let x = index % width;
            let y = index / width;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            ink_area += usize::from(residual[index] != 0);

            let y0 = y.saturating_sub(1);
            let y1 = (y + 1).min(height - 1);
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(width - 1);
            for ny in y0..=y1 {
                for nx in x0..=x1 {
                    let next = ny * width + nx;
                    if !visited[next] && dilated[next] != 0 {
                        visited[next] = true;
                        stack.push(next);
                    }
                }
            }
        }

        let component_w = max_x.saturating_sub(min_x).saturating_add(1);
        let component_h = max_y.saturating_sub(min_y).saturating_add(1);
        let bbox_area = component_w.saturating_mul(component_h);
        let touches_edge = min_x == 0 || min_y == 0 || max_x + 1 == width || max_y + 1 == height;
        let thin_edge_strip =
            touches_edge && (component_w <= thin_edge_w || component_h <= thin_edge_h);
        if bbox_area < min_bbox_area || ink_area < min_ink_area || thin_edge_strip {
            continue;
        }

        let bounds = ContentBounds {
            min_x: min_x.saturating_sub(DETECTION_PAD) as u32,
            min_y: min_y.saturating_sub(DETECTION_PAD) as u32,
            max_x: (max_x + 1 + DETECTION_PAD).min(width) as u32,
            max_y: (max_y + 1 + DETECTION_PAD).min(height) as u32,
        };
        combined = Some(match combined {
            Some(existing) => ContentBounds {
                min_x: existing.min_x.min(bounds.min_x),
                min_y: existing.min_y.min(bounds.min_y),
                max_x: existing.max_x.max(bounds.max_x),
                max_y: existing.max_y.max(bounds.max_y),
            },
            None => bounds,
        });
    }

    combined
}

pub fn is_visually_blank_page(image: &RgbImage) -> bool {
    let raw = image.as_raw();
    if raw.len() < 3 {
        return true;
    }

    let pixel_count = (image.width() as usize).saturating_mul(image.height() as usize);
    if pixel_count == 0 {
        return true;
    }

    let step = (pixel_count / 200_000).max(1);
    let mut sampled = 0usize;
    let mut non_white = 0usize;
    let mut dark = 0usize;
    let mut very_dark = 0usize;
    let mut sum = 0f64;
    let mut sum_sq = 0f64;
    let mut nw_sum = 0f64;
    let mut nw_sum_sq = 0f64;

    for px_index in (0..pixel_count).step_by(step) {
        let base = px_index * 3;
        if base + 2 >= raw.len() {
            break;
        }

        let r = raw[base] as u16;
        let g = raw[base + 1] as u16;
        let b = raw[base + 2] as u16;
        let luma = (299u32 * r as u32 + 587u32 * g as u32 + 114u32 * b as u32) / 1000;

        sampled += 1;
        sum += luma as f64;
        sum_sq += (luma as f64) * (luma as f64);
        if luma < 245 {
            non_white += 1;
            nw_sum += luma as f64;
            nw_sum_sq += (luma as f64) * (luma as f64);
        }
        if luma < 180 {
            dark += 1;
        }
        if luma < 120 {
            very_dark += 1;
        }
    }

    if sampled == 0 {
        return true;
    }

    let non_white_ratio = non_white as f32 / sampled as f32;
    if non_white_ratio < 0.003 {
        return true;
    }

    let dark_ratio = dark as f32 / sampled as f32;
    let very_dark_ratio = very_dark as f32 / sampled as f32;
    let mean = sum / sampled as f64;
    let variance = (sum_sq / sampled as f64) - mean * mean;
    let stddev = variance.max(0.0).sqrt();

    // A featureless page is blank regardless of its absolute brightness:
    // scanner flyleaves/endpapers come out as uniform mid-gray (mean ~155,
    // stddev ~5) and have no ink/paper separation to binarize — a fixed
    // threshold turns them solid black. Content pages measure stddev ≥ ~16
    // (images) and ≥ ~25 (text) on the same material. Statistics are taken
    // over NON-WHITE pixels only so the white canvas that standardize/center
    // pastes around such a page cannot fake bimodal contrast.
    if non_white_ratio >= 0.3 {
        let nw_mean = nw_sum / non_white as f64;
        let nw_variance = (nw_sum_sq / non_white as f64) - nw_mean * nw_mean;
        let nw_stddev = nw_variance.max(0.0).sqrt();
        let nw_very_dark_ratio = very_dark as f32 / non_white as f32;
        if nw_stddev <= 10.0 && nw_very_dark_ratio < 0.002 {
            return true;
        }
    }

    mean >= 178.0 && stddev <= 18.0 && dark_ratio < 0.0015 && very_dark_ratio < 0.0002
}

pub fn detections_bbox_area_fraction(detections: &[Detection], width: u32, height: u32) -> f32 {
    let page_area = (width as f64) * (height as f64);
    if page_area <= 0.0 {
        return 0.0;
    }

    let mut sum: f64 = 0.0;
    for det in detections {
        let (x1, y1, x2, y2) = rounded_clamped_bbox(det.bbox, width, height);
        let w = (x2.saturating_sub(x1)) as f64;
        let h = (y2.saturating_sub(y1)) as f64;
        sum += w * h;
    }

    (sum / page_area).min(1.0) as f32
}

#[derive(Debug, Clone, Copy, Default)]
struct LayoutBlankEvidence {
    total_area_fraction: f32,
    text_area_fraction: f32,
    image_area_fraction: f32,
    max_area_fraction: f32,
}

fn layout_blank_evidence(
    detections: &[Detection],
    width: u32,
    height: u32,
    classifier: &LabelClassifier,
) -> LayoutBlankEvidence {
    let page_area = (width as f64) * (height as f64);
    if page_area <= 0.0 {
        return LayoutBlankEvidence::default();
    }

    let mut total = 0.0f64;
    let mut text = 0.0f64;
    let mut image = 0.0f64;
    let mut max_area = 0.0f64;

    for det in detections {
        let (x1, y1, x2, y2) = rounded_clamped_bbox(det.bbox, width, height);
        let area = (x2.saturating_sub(x1) as f64) * (y2.saturating_sub(y1) as f64);
        if area <= 0.0 {
            continue;
        }

        total += area;
        max_area = max_area.max(area);
        if classifier.is_image_label(det) {
            image += area;
        } else if classifier.is_text_label(det)
            && !matches!(det.category, crate::types::ContentCategory::Abandon)
        {
            text += area;
        }
    }

    LayoutBlankEvidence {
        total_area_fraction: (total / page_area).min(1.0) as f32,
        text_area_fraction: (text / page_area).min(1.0) as f32,
        image_area_fraction: (image / page_area).min(1.0) as f32,
        max_area_fraction: (max_area / page_area).min(1.0) as f32,
    }
}

pub fn should_force_blank_page_threshold(
    config: &PipelineConfig,
    has_no_filtered_detections: bool,
    page_is_visually_blank: bool,
    post_nms_detections: &[Detection],
    width: u32,
    height: u32,
    classifier: &LabelClassifier,
) -> bool {
    // The binarizer model leaves blank paper white on its own; only the
    // adaptive CPU fallback turns paper noise into ink.
    if !config.binarization().force_cpu
        && !config.binarization().fast
        && crate::color::binarizes_on_gpu()
    {
        return false;
    }

    // Without layout detection we normally have no detection boxes to analyse,
    // but callers may pass cached/post-NMS boxes from another phase. Use them
    // when present rather than discarding useful layout evidence.
    if !config.enable_layout_detection() && post_nms_detections.is_empty() {
        return page_is_visually_blank;
    }

    let evidence = layout_blank_evidence(post_nms_detections, width, height, classifier);

    const MAX_LAYOUT_BLANK_TOTAL_FRACTION: f32 = 0.006;
    const MAX_LAYOUT_BLANK_TEXT_FRACTION: f32 = 0.002;
    const MAX_LAYOUT_BLANK_IMAGE_FRACTION: f32 = 0.001;
    const MAX_LAYOUT_BLANK_SINGLE_BOX_FRACTION: f32 = 0.004;

    let visual_blank_with_no_real_layout = page_is_visually_blank && has_no_filtered_detections;

    // Tiny layout boxes are only supporting evidence. YOLO can reduce a real
    // sparse page to one small heading/number box, so layout output alone must
    // never erase pixels that the visual check says are nonblank.
    let tiny_post_nms_noise = page_is_visually_blank
        && !post_nms_detections.is_empty()
        && evidence.total_area_fraction <= MAX_LAYOUT_BLANK_TOTAL_FRACTION
        && evidence.text_area_fraction <= MAX_LAYOUT_BLANK_TEXT_FRACTION
        && evidence.image_area_fraction <= MAX_LAYOUT_BLANK_IMAGE_FRACTION
        && evidence.max_area_fraction <= MAX_LAYOUT_BLANK_SINGLE_BOX_FRACTION;

    visual_blank_with_no_real_layout || tiny_post_nms_noise
}

pub fn is_full_page_image(
    detections: &[Detection],
    page_width: u32,
    page_height: u32,
    content_bounds: Option<ContentBounds>,
) -> bool {
    if detections.len() == 1 {
        let det = &detections[0];
        if !crate::types::LABEL_CLASSIFIER.is_image_label(det) {
            return false;
        }
        let region_width = det.bbox[2] - det.bbox[0];
        let region_height = det.bbox[3] - det.bbox[1];
        let coverage = (region_width * region_height) as f32 / (page_width * page_height) as f32;
        coverage > 0.8
    } else if detections.is_empty() {
        if let Some(bounds) = content_bounds {
            let coverage =
                (bounds.width() * bounds.height()) as f32 / (page_width * page_height) as f32;
            coverage > 0.8
        } else {
            false
        }
    } else {
        false
    }
}

pub fn classify_page(
    detections: &[Detection],
    page_width: u32,
    page_height: u32,
    pixel_bounds: Option<ContentBounds>,
) -> PageClassification {
    let detection_bounds = if detections.is_empty() {
        None
    } else {
        crate::margin::calculate_content_bounds(detections, page_width, page_height, false)
    };

    let content_bounds = detection_bounds.or(pixel_bounds);
    let is_blank = detections.is_empty() && pixel_bounds.is_none();
    let is_full_page_image =
        is_full_page_image(detections, page_width, page_height, content_bounds);

    PageClassification {
        content_bounds,
        is_blank,
        is_full_page_image,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentCategory, LABEL_CLASSIFIER};

    fn test_detection(category: ContentCategory, bbox: [f32; 4]) -> Detection {
        let class_id = match category {
            ContentCategory::Text => 2,    // "text"
            ContentCategory::Image => 1,   // "image"
            ContentCategory::Table => 8,   // "table"
            ContentCategory::Abandon => 3, // "number"
        };
        Detection {
            class_id,
            class_name: Some(crate::types::class_name_for(class_id).to_string()),
            confidence: 0.8,
            bbox,
            category,
            context: None,
        }
    }

    fn cpu_binarizer_config() -> PipelineConfig {
        let mut config = PipelineConfig::default();
        let mut binarization = config.binarization().clone();
        binarization.force_cpu = true;
        config.set_binarization(binarization);
        config
    }

    #[test]
    fn aged_low_contrast_page_counts_as_visually_blank() {
        let image = RgbImage::from_pixel(100, 100, image::Rgb([210, 207, 198]));
        assert!(is_visually_blank_page(&image));
    }

    #[test]
    fn post_nms_tiny_noise_boxes_force_blank_fallback() {
        // The blank-page threshold belongs to the CPU fallback: the binarizer
        // model leaves blank paper white on its own.
        let mut config = cpu_binarizer_config();
        config.set_enable_layout_detection(true);
        let detections = vec![test_detection(
            ContentCategory::Abandon,
            [10.0, 10.0, 14.0, 16.0],
        )];

        assert!(should_force_blank_page_threshold(
            &config,
            false,
            true,
            &detections,
            1000,
            1000,
            &LABEL_CLASSIFIER,
        ));
    }

    #[test]
    fn tiny_layout_boxes_do_not_erase_a_nonblank_page() {
        let mut config = PipelineConfig::default();
        config.set_enable_layout_detection(true);
        let detections = vec![test_detection(
            ContentCategory::Abandon,
            [10.0, 10.0, 14.0, 16.0],
        )];

        assert!(!should_force_blank_page_threshold(
            &config,
            false,
            false,
            &detections,
            1000,
            1000,
            &LABEL_CLASSIFIER,
        ));
    }

    #[test]
    fn substantial_image_box_blocks_blank_fallback() {
        let mut config = PipelineConfig::default();
        config.set_enable_layout_detection(true);
        let detections = vec![test_detection(
            ContentCategory::Image,
            [100.0, 100.0, 600.0, 600.0],
        )];

        assert!(!should_force_blank_page_threshold(
            &config,
            false,
            true,
            &detections,
            1000,
            1000,
            &LABEL_CLASSIFIER,
        ));
    }

    #[test]
    fn no_layout_boxes_alone_does_not_force_nonvisual_blank_page() {
        let config = PipelineConfig::default();

        assert!(!should_force_blank_page_threshold(
            &config,
            true,
            false,
            &[],
            1000,
            1000,
            &LABEL_CLASSIFIER,
        ));
    }

    #[test]
    fn single_full_page_text_box_is_not_full_page_image() {
        let detections = vec![test_detection(
            ContentCategory::Text,
            [0.0, 0.0, 1000.0, 1000.0],
        )];

        assert!(!is_full_page_image(&detections, 1000, 1000, None));
    }

    #[test]
    fn single_full_page_figure_box_is_full_page_image() {
        let detections = vec![test_detection(
            ContentCategory::Image,
            [0.0, 0.0, 1000.0, 1000.0],
        )];

        assert!(is_full_page_image(&detections, 1000, 1000, None));
    }

    fn fill_black(image: &mut RgbImage, x1: u32, y1: u32, x2: u32, y2: u32) {
        for y in y1..y2 {
            for x in x1..x2 {
                image.put_pixel(x, y, image::Rgb([0, 0, 0]));
            }
        }
    }

    #[test]
    fn missed_pixel_guard_keeps_substantial_undetected_region() {
        let mut image = RgbImage::from_pixel(200, 200, image::Rgb([255, 255, 255]));
        fill_black(&mut image, 20, 40, 80, 120);
        fill_black(&mut image, 130, 60, 180, 150);
        let detections = vec![test_detection(
            ContentCategory::Text,
            [18.0, 38.0, 82.0, 122.0],
        )];

        let bounds =
            compute_missed_pixel_bounds_for_margin(&image, &detections, &PipelineConfig::default())
                .expect("missed illustration should be retained");

        assert!(bounds.min_x <= 130);
        assert!(bounds.max_x >= 180);
        assert!(bounds.min_y <= 60);
        assert!(bounds.max_y >= 150);
    }

    #[test]
    fn missed_pixel_guard_rejects_small_furniture_and_thin_edge_strip() {
        let mut image = RgbImage::from_pixel(200, 200, image::Rgb([255, 255, 255]));
        fill_black(&mut image, 95, 185, 105, 191);
        fill_black(&mut image, 0, 0, 1, 200);

        // The guard is what is under test, so the mask comes from the CPU
        // fallback: it is the same on every machine.
        assert!(
            compute_missed_pixel_bounds_for_margin(&image, &[], &cpu_binarizer_config()).is_none()
        );
    }

    #[test]
    fn missed_pixel_guard_masks_detected_content() {
        let mut image = RgbImage::from_pixel(200, 200, image::Rgb([255, 255, 255]));
        fill_black(&mut image, 20, 40, 180, 150);
        let detections = vec![test_detection(
            ContentCategory::Text,
            [18.0, 38.0, 182.0, 152.0],
        )];

        let result =
            compute_missed_pixel_bounds_for_margin(&image, &detections, &PipelineConfig::default());
        assert!(result.is_none(), "unexpected residual bounds: {result:?}");
    }

    #[test]
    fn missed_pixel_guard_keeps_substantial_full_bleed_content() {
        let mut image = RgbImage::from_pixel(200, 200, image::Rgb([255, 255, 255]));
        fill_black(&mut image, 0, 0, 200, 200);

        let bounds =
            compute_missed_pixel_bounds_for_margin(&image, &[], &PipelineConfig::default())
                .expect("full-bleed content must remain a safety guard");

        assert_eq!(bounds.min_x, 0);
        assert_eq!(bounds.min_y, 0);
        assert_eq!(bounds.max_x, 200);
        assert_eq!(bounds.max_y, 200);
    }
}
