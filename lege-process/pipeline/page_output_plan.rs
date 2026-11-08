//! Phase 6 page output plan.
//!
//! Lege builds the plan; the renderer executes one request per product.
//!
//! Every product of a page names the base output raster as its image decode
//! reference ([`lege_pdf_read::DecodeReference`]). A JPEG or JPEG 2000 scan
//! is decoded at a reduced resolution chosen from the draw's footprint, so
//! without a shared reference the 800-pixel analysis render and the base
//! render would decode the page's scan twice, at two resolutions. With it the
//! analysis render decodes at the base's resolution, the base render reuses
//! that decode, and both render from the same source pixels; this holds with
//! the renderer's decoded-image cache on or off, since the reference is part
//! of each request. Products larger than the base (the slow-OCR surface, 2x
//! region crops, or the analysis render itself when the output target is
//! under the inference size) still decode at their own resolution. Only the
//! base sets the shared level, so the base pixels are the same whichever
//! other products the configuration enables.

use lege_pdf_read::{
    AnalysisTarget, BaseTarget, DeviceCrop, GraySuitability, OcrTarget, PageOutputPlan,
    RasterProduct, RegionTarget,
};

use crate::pipeline::config::PipelineConfig;

#[derive(Debug, Clone, Copy)]
pub struct PagePlanInput {
    pub output_width: u32,
    pub output_height: u32,
    pub gray_suitability: GraySuitability,
}

pub fn base_is_gray(config: &PipelineConfig, suitability: GraySuitability) -> bool {
    if config.text_format() == "jpeg" {
        return false;
    }
    match suitability {
        GraySuitability::ColorFallback => false,
        GraySuitability::Exact | GraySuitability::AcceptableForBilevel => true,
    }
}

/// The base output raster's size: the decode reference of every product of
/// the page (see the module documentation).
fn base_size(input: PagePlanInput) -> (u32, u32) {
    (input.output_width.max(1), input.output_height.max(1))
}

fn base_product(config: &PipelineConfig, input: PagePlanInput) -> RasterProduct {
    let (width, height) = base_size(input);
    let product = if base_is_gray(config, input.gray_suitability) {
        RasterProduct::gray8(width, height)
    } else {
        RasterProduct::rgb8(width, height)
    };
    product.with_decode_reference(width, height)
}

fn analysis_target(config: &PipelineConfig, input: PagePlanInput) -> Option<AnalysisTarget> {
    if !config.enable_layout_detection() {
        return None;
    }
    let long_edge = config.inference_size().max(1);
    let (w, h) = if input.output_width >= input.output_height {
        let h = ((long_edge as u64 * input.output_height.max(1) as u64)
            / input.output_width.max(1) as u64)
            .max(1) as u32;
        (long_edge, h)
    } else {
        let w = ((long_edge as u64 * input.output_width.max(1) as u64)
            / input.output_height.max(1) as u64)
            .max(1) as u32;
        (w, long_edge)
    };
    let (base_width, base_height) = base_size(input);
    Some(AnalysisTarget {
        product: RasterProduct::rgb8(w, h).with_decode_reference(base_width, base_height),
    })
}

fn ocr_target(config: &PipelineConfig, input: PagePlanInput) -> Option<OcrTarget> {
    if !(config.enable_ocr() && config.slow_ocr_enabled()) {
        return None;
    }
    let ocr_height = config.source_render_height();
    if ocr_height <= input.output_height {
        return None;
    }
    let width = ((input.output_width.max(1) as u64 * ocr_height as u64)
        / input.output_height.max(1) as u64)
        .max(1) as u32;
    let (base_width, base_height) = base_size(input);
    Some(OcrTarget {
        product: RasterProduct::rgb8(width, ocr_height)
            .with_decode_reference(base_width, base_height),
    })
}

pub fn plan_page_output(config: &PipelineConfig, input: PagePlanInput) -> PageOutputPlan {
    PageOutputPlan {
        analysis: analysis_target(config, input),
        base: BaseTarget {
            product: base_product(config, input),
            gray_suitability: input.gray_suitability,
        },
        regions: Vec::new(),
        ocr: ocr_target(config, input),
    }
}

/// Render pixels per device pixel for a JP2 image region. The crop is rendered
/// at this scale and jp2lam box-filters it into the device box in linear light
/// inside its rate loop, instead of encoding the renderer's device-size crop:
/// on 21 device-resolution book-scan figures (Central Park, 2026-09-05) that was
/// 0.85x the bytes at the same verified floor, for about 1.4x the region encode
/// time and 4x the region render pixels. Codecs other than JP2 emit the crop
/// as-is and must render at 1.
pub const JP2_REGION_RENDER_SCALE: u32 = 2;

/// Longest side the region encoder accepts (`encode_region_image`).
const MAX_REGION_SIDE: u32 = 8192;

pub fn region_target(
    bbox: [f32; 4],
    output_width: u32,
    output_height: u32,
    render_scale: u32,
) -> Option<RegionTarget> {
    let x1 = bbox[0].floor().clamp(0.0, output_width as f32) as i32;
    let y1 = bbox[1].floor().clamp(0.0, output_height as f32) as i32;
    let x2 = bbox[2].ceil().clamp(0.0, output_width as f32) as i32;
    let y2 = bbox[3].ceil().clamp(0.0, output_height as f32) as i32;
    let width = (x2 - x1).max(0) as u32;
    let height = (y2 - y1).max(0) as u32;
    if width == 0 || height == 0 {
        return None;
    }
    let render_scale = render_scale.max(1);
    let render_scale = if width.max(height).saturating_mul(render_scale) > MAX_REGION_SIDE {
        1
    } else {
        render_scale
    };
    Some(RegionTarget {
        product: RasterProduct::rgb8(width * render_scale, height * render_scale)
            .with_crop(DeviceCrop {
                x: x1,
                y: y1,
                width,
                height,
                page_width: output_width,
                page_height: output_height,
            })
            // The crop's page is the base output raster, the page's shared
            // decode reference; a 2x region still decodes at its own scale.
            .with_decode_reference(output_width.max(1), output_height.max(1)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lege_pdf_read::RasterFormat;

    fn base_config() -> PipelineConfig {
        let mut config = PipelineConfig::new().expect("config");
        config.set_enable_layout_detection(false);
        config.set_enable_ocr(false);
        config.set_text_format("ccitt4").expect("format");
        config.set_target_height(1200).expect("height");
        config.set_high_res_render_height(1200).expect("render");
        config
    }

    fn input(suitability: GraySuitability) -> PagePlanInput {
        PagePlanInput {
            output_width: 900,
            output_height: 1200,
            gray_suitability: suitability,
        }
    }

    #[test]
    fn common_bilevel_base_is_gray_without_analysis_or_ocr() {
        let plan = plan_page_output(&base_config(), input(GraySuitability::Exact));
        assert_eq!(plan.base.product.format, RasterFormat::Gray8);
        assert!(plan.analysis.is_none());
        assert!(plan.ocr.is_none());
    }

    #[test]
    fn color_risky_page_falls_back_to_rgb_base() {
        let plan = plan_page_output(&base_config(), input(GraySuitability::ColorFallback));
        assert_eq!(plan.base.product.format, RasterFormat::Rgb8);
    }

    #[test]
    fn jpeg_base_mode_stays_rgb_even_when_gray_suitable() {
        let mut config = base_config();
        config.set_text_format("jpeg").expect("format");
        let plan = plan_page_output(&config, input(GraySuitability::Exact));
        assert_eq!(plan.base.product.format, RasterFormat::Rgb8);
    }

    #[test]
    fn layout_mode_adds_low_resolution_analysis_surface() {
        let mut config = base_config();
        config.set_enable_layout_detection(true);
        let plan = plan_page_output(&config, input(GraySuitability::AcceptableForBilevel));
        let analysis = plan.analysis.expect("analysis");
        assert_eq!(
            analysis.product.width.max(analysis.product.height),
            config.inference_size()
        );
    }

    #[test]
    fn slow_ocr_requests_its_own_surface() {
        let mut config = base_config();
        config.set_enable_ocr(true);
        config.set_slow_ocr(true);
        config.set_slow_ocr_scale(2.0);
        config.set_target_height(600).expect("height");
        config.set_high_res_render_height(1200).expect("render");
        let plan = plan_page_output(
            &config,
            PagePlanInput {
                output_width: 450,
                output_height: 600,
                gray_suitability: GraySuitability::Exact,
            },
        );
        assert!(plan.ocr.expect("ocr").product.height > 600);
    }

    #[test]
    fn region_target_carries_crop_and_size() {
        let target = region_target([100.4, 200.9, 300.2, 500.6], 900, 1200, 1).expect("region");
        let crop = target.product.crop.expect("crop");
        assert_eq!((crop.x, crop.y), (100, 200));
        assert_eq!((crop.width, crop.height), (201, 301));
        assert_eq!((target.product.width, target.product.height), (201, 301));
    }

    /// A scaled render keeps the crop in device space (placement and base-layer
    /// masking read it) and only the rendered product grows.
    #[test]
    fn scaled_region_keeps_its_device_crop() {
        let target = region_target([100.4, 200.9, 300.2, 500.6], 900, 1200, 2).expect("region");
        let crop = target.product.crop.expect("crop");
        assert_eq!(
            (crop.x, crop.y, crop.width, crop.height),
            (100, 200, 201, 301)
        );
        assert_eq!((crop.page_width, crop.page_height), (900, 1200));
        assert_eq!((target.product.width, target.product.height), (402, 602));
        // Too large to scale: falls back to the device crop.
        let huge = region_target([0.0, 0.0, 5000.0, 5000.0], 5000, 5000, 2).expect("region");
        assert_eq!((huge.product.width, huge.product.height), (5000, 5000));
    }

    #[test]
    fn degenerate_region_is_dropped() {
        assert!(region_target([10.0, 10.0, 10.0, 40.0], 900, 1200, 1).is_none());
    }

    /// Every product of a page names the base output raster as its decode
    /// reference, so the analysis render decodes the page's scan at the
    /// base's resolution and shares that decode; the OCR surface and regions
    /// carry the same reference and never set it.
    #[test]
    fn every_product_names_the_base_raster_as_decode_reference() {
        let mut config = base_config();
        config.set_enable_layout_detection(true);
        config.set_enable_ocr(true);
        config.set_slow_ocr(true);
        config.set_slow_ocr_scale(2.0);
        let plan = plan_page_output(&config, input(GraySuitability::Exact));
        let base = Some(lege_pdf_read::DecodeReference {
            width: 900,
            height: 1200,
        });
        let analysis = plan.analysis.expect("analysis").product;
        let ocr = plan.ocr.expect("ocr").product;
        assert!(analysis.height < 1200 && ocr.height > 1200);
        for product in [analysis, plan.base.product, ocr] {
            assert_eq!(product.decode_reference, base, "{product:?}");
        }
        for scale in [1, 2] {
            let region = region_target([10.0, 20.0, 110.0, 220.0], 900, 1200, scale)
                .expect("region")
                .product;
            assert_eq!(region.decode_reference, base);
        }
    }
}
