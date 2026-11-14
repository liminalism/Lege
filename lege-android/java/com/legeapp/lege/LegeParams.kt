package com.legeapp.lege

/**
 * Job configuration, read field-by-field by `lege-android/src/config.rs`.
 *
 * Every property is `@JvmField` on purpose: the native side looks these up by
 * name with `GetFieldID`, so they must be real fields under exactly these
 * names rather than Kotlin properties behind accessors.
 *
 * This exposes the Android-safe processing controls. Worker and buffer sizing
 * intentionally stay automatic: native code derives them from the app memory
 * class supplied to `LegeNative.nativeInit`.
 */
class LegeParams {
    /** Output page height in pixels. 0 keeps the pipeline default. */
    @JvmField
    var targetHeight: Int = 0

    /**
     * `"pdf"` or `"djvu"`. DjVu routes the job to the linked `djvu_encoder`
     * rather than the PDF writer; both run entirely in-process, so neither
     * needs a helper executable.
     */
    @JvmField
    var outputFormat: String = "pdf"

    /**
     * Run layout detection (the embedded RF-DETR model). Requires a working
     * Vulkan device; if the GPU is unusable the pipeline logs a warning, turns
     * this off for the run, and continues rather than failing.
     */
    @JvmField
    var enableLayoutDetection: Boolean = true

    /** Add an OCR text layer using the embedded PP-OCR models. */
    @JvmField
    var enableOcr: Boolean = false

    @JvmField
    var highQualityOutput: Boolean = false

    /** Treat the source as inverted (light text on dark). */
    @JvmField
    var invertInput: Boolean = false

    /** `"5"` or `"1-20"`, one-based inclusive. Null processes everything. */
    @JvmField
    var pageRange: String? = null

    /** `"none"`, `"center"`, `"crop"`, or `"reflow"`. */
    @JvmField
    var marginMode: String = "none"

    @JvmField
    var slowOcr: Boolean = false

    @JvmField
    var jpegCompat: Boolean = false

    /** Dither image regions instead of retaining their original raster crops. */
    @JvmField
    var ditherImages: Boolean = false

    /**
     * Flatten page: straighten photographed pages and split two-page spreads
     * with the embedded page-flattening model. Needs a hardware GPU; without
     * one the pipeline skips it. Flat scans are left alone.
     */
    @JvmField
    var flattenPage: Boolean = false

    /** Filesystem destination for the optional EPUB companion file. */
    @JvmField
    var epubSidecarPath: String? = null
}
