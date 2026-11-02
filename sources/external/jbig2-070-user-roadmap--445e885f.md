# jbig2enc-rust 0.7.0 — proposed development roadmap

After examining the current `0.6.0` source, I would make 0.7.0 a symbol-encoding quality and efficiency release, rather than expanding the decoder or adding more JBIG2 features indiscriminately.

Your encoder is already relatively advanced. The documented 350-page benchmark reports:

| Mode                        | Output size | Encoding time |
| --------------------------- | ----------- | ------------- |
| Original C jbig2enc, symbol | 2,566.5 KB  | 24.58 s       |
| Rust generic                | 5,232.2 KB  | 2.47 s        |
| Rust symbol                 | 2,227.1 KB  | 6.05 s        |
| Rust symbol-unify           | 2,025.9 KB  | 16.93 s       |

Those are repository-reported results, not benchmarks I independently reran.

The more important finding is that refinement is partly implemented already. Your current encoder supports per-instance text-region refinement (`SBREFINE`), including dimension and position adjustments and arithmetic coding of refined glyphs.

But the [dictionary-level refinement planner](https://github.com/liminalism/Lege/blob/main/lege-codecs/jbig2enc-rust/src/encode/document/symbol/dictionary.rs?utm_source=chatgpt.com) is deliberately a stub in the production path, with a more ambitious reference implementation behind test-only configuration.

That changes how I would approach the next release. You don't need to start refinement from scratch. You need to make existing refinement machinery useful to the production optimizer, select it economically, and prove that the resulting output preserves characters correctly.

I would organize 0.7.0 around three objectives:

1. Better compression: fewer expensive residual regions, more efficient symbol dictionaries and improved near-match handling.
2. Trustworthy output: a genuinely pixel-exact symbol mode, plus explicit safeguards for intentionally lossy modes.
3. Practical performance: keep encoding costs, dictionary memory and PDF decoding latency under control even for books hundreds of pages long.

## 1. Highest priority: Replace the all-global dictionary policy

This is the most consequential architectural opportunity I found.

In [`planning.rs`](https://github.com/liminalism/Lege/blob/main/lege-codecs/jbig2enc-rust/src/encode/document/symbol/planning.rs?utm_source=chatgpt.com), the PDF path explicitly selects every discovered symbol for the global dictionary:

```
let multi_page_candidates: Vec<usize> = if self.state.pdf_mode {
    (0..self.global_symbols.len()).collect()
} else {
    // Select symbols shared across pages
    // ...
};
```

Later it creates no page-local symbol dictionaries for PDF mode.

The accompanying comment says PDF page streams cannot contain local dictionaries. That restriction is not imposed by the PDF specification. PDF image streams may contain page-associated JBIG2 segments, including symbol dictionaries; only segments designated as global belong in `/JBIG2Globals`.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://developer.adobe.com\&sz=32)

developer.adobe.com



This means the current PDF architecture unnecessarily prevents you from selecting which symbols are worth sharing across pages.

For a 500-page book, that's potentially expensive. A symbol appearing on one page only might be promoted into a dictionary that every page can reference, even though the symbol provides no cross-page reuse benefit.

I'd introduce three classes of symbols:

Document or group dictionary

Symbols reused frequently across pages

Page-local dictionary

Useful only on one page

Generic residual

Unique marks, irregular or unsuitable components

The selection must be based on total encoded cost, not merely frequency. A page-local symbol that appears once may cost more as a dictionary entry plus text placement than as generic bits.

### What to implement

First, make local dictionaries work in PDF mode and test them against the native decoder, jbig2dec, PDFium and Poppler. Then compare full-global, selective-global and selective-global-plus-local strategies.

After that, investigate dictionary groups of 32, 64, 128 pages and whole-book size. Grouping is especially interesting for long books, but would require extending the PDF-output API so the caller can associate each page with the appropriate globals stream.

I would not assume that 64 pages is optimal. Measure the relationship between file size, global-dictionary bytes, decoder memory and cold-page latency.

This is more than compression optimization. It aligns the codec with Lege's original objective of producing pages that open quickly on constrained readers.

## 2. Stop treating every residual as one rectangular generic region

This is probably the best relatively contained compression improvement.

In [`serialize.rs`](https://github.com/liminalism/Lege/blob/main/lege-codecs/jbig2enc-rust/src/encode/document/symbol/serialize.rs?utm_source=chatgpt.com), `build_instance_residual_bitmap()` finds the minimum bounding rectangle encompassing all residual instances on a page and creates a single bitmap covering that rectangle.

Imagine a page with two unclassified symbols near opposite corners. Their combined bounding rectangle could span almost the entire page, even though those two components contain very few black pixels.

Arithmetic coding can compress the empty space, so the cost isn't proportional to the rectangle's raw area. Nevertheless, that layout makes the coder process irrelevant blank areas and can reduce compression efficiency.

I would add a cost-aware residual-region packer.

It would group nearby residual components into bounded rectangles and choose between individual, clustered and whole-page residual representations. The encoder should compare actual resulting byte counts when the candidates are sufficiently cheap to test, including segment headers.

A practical first experiment would compare:

| Residual representation               | Expected advantage                                |
| ------------------------------------- | ------------------------------------------------- |
| Current single bounding rectangle     | Minimal segment overhead                          |
| Several tight rectangles              | Avoids large empty areas                          |
| Row-band regions                      | Good for text and isolated punctuation            |
| Generic residual vs page-local symbol | Lets unique glyphs use the cheaper representation |

This can be developed without altering the decoder. It is also attractive for small marks, punctuation, mathematical notation and unusual characters that your matching system correctly refuses to unify.

Acceptance test: On an unchanged corpus, no missing pixels or segment-ordering problems, with lower residual encoding cost on the targeted pages. Record both the encoded residual bytes and the CPU time; accept the new layout only where it wins.

## 3. Make refinement a measured encoding decision

I would not simply enable `text_refine` everywhere.

Your source already observes that refinement adds overhead on clean text, and the production code distinguishes direct substitution from refined instances. The missing piece is selecting refinement according to its actual benefit.

In 0.7.0, give the optimizer four candidate representations for a glyph occurrence or family:

| Candidate                     | Fidelity                | Cost characteristics                                          |
| ----------------------------- | ----------------------- | ------------------------------------------------------------- |
| Exact dictionary symbol reuse | Pixel-exact             | Excellent when an identical symbol is already stored          |
| Reference symbol + refinement | Potentially pixel-exact | Pays refinement-data and signaling costs                      |
| New dictionary symbol         | Pixel-exact             | More dictionary bytes, but may benefit subsequent occurrences |
| Generic coding                | Pixel-exact             | Often best for unusual or isolated components                 |

For intentionally lossy output, a fifth candidate can substitute a similar symbol without refinement, but that must remain explicitly distinguishable.

The production planner should estimate costs cheaply first, then measure likely winners by encoding the relevant candidate streams. Because arithmetic coding depends on prior contexts, the cost of a refinement isn't just its differing-pixel count. The estimator needs calibration against actual output.

There is already a family-refinement prototype in `dictionary.rs`, but its existing `family_refinement_gain()` uses a simple heuristic involving pixel errors, shifts and estimated dictionary storage. I would reuse its candidate discovery, not its cost formula as the final authority.

The bigger architectural improvement is to decide jointly whether to keep a symbol in the dictionary, refine its occurrences or put them in generic residuals. Doing those decisions separately can produce a locally reasonable but globally inefficient result.

## 4. Add a genuinely lossless symbol-dictionary preset

This would be a major product improvement, particularly given that other projects are already using your `encode_single_image_lossless()` API.

At present, that API produces generic-region encoding. Your source also contains mechanisms for refinement that can reconstruct nonidentical glyphs, but there is no clearly advertised, end-to-end guarantee that symbol-mode output is identical to the supplied bilevel input.

I would introduce a preset along these lines:

```
let config = Jbig2Config::text_lossless();
```

That name is illustrative; the important part is its contract.

This new mode should preserve every input pixel, including isolated dots and unusual marks. Exact symbol matches would require no refinement. Near matches would require appropriate refinement to reproduce the source exactly, otherwise they would be stored separately or encoded generically.

It must also disable any cleanup that changes the input bitmap. Your component-extraction code accepts a `losslevel` argument, and the symbol path currently uses a nonzero cleanup level in ordinary non-lossless configurations. Pixel-exact symbol coding requires separating that preprocessing decision from the encoding decision.

I would keep distinct concepts:

- Lossless to input: every supplied bilevel pixel is preserved.
- Lossless to preprocessed image: the encoder is exact, but optional cleanup already changed the image.
- Lossy substitution: near-matched glyphs are deliberately unified.

These distinctions matter more than the usual “visually lossless” terminology. A symbol substitution can change a digit or letter while producing an image that still appears entirely natural.

For 0.7.0, a full decoded-bitmap equality test should be the release gate for the first mode. This gives downstream PDF applications a meaningful new encoding choice: JBIG2 symbol compression without the risk of silently changing text.

## 5. Improve symbol matching without making it more permissive

Your comparator is already reasonably sophisticated. It uses word-wise XOR/popcount comparisons, shifted matching, structural signatures, and SIMD acceleration.

I wouldn't rewrite it around a neural classifier or significantly raise the matching tolerance. The next improvement should be in how candidates are selected and judged.

In [`unify_match.rs`](https://github.com/liminalism/Lege/blob/main/lege-codecs/jbig2enc-rust/src/encode/document/symbol/unify_match.rs?utm_source=chatgpt.com), the acceptance decisions use pixel errors, shifts, shape signatures and several configurable thresholds. That works, but it's still a heuristic representation of a more meaningful question: will assigning this glyph to this prototype produce a better encoded result at the required fidelity?

I'd add two stages:

Stage A — fast candidate discovery. Preserve your existing signature buckets, hash matching and SIMD comparisons. Introduce explicit candidate limits so an unusually large collection of similar-looking components cannot trigger uncontrolled comparison work.

Stage B — rate-distortion evaluation. For the best few candidate prototypes, evaluate predicted encoding cost and the damage from any proposed substitution. When fidelity is mandatory, calculate the refinement or exact-storage cost rather than accepting damage.

For lossy modes, I'd particularly emphasize topology: enclosed holes, connected components and stroke endpoints. Your classifier already has a hole-count feature, so this is an extension of existing work rather than a new recognition pipeline.

A small distortion budget is not sufficient by itself. A single pixel can distinguish a `6` from an `8`, or close the opening in a `c`. Such a substitution should not be treated like harmless random edge noise merely because the overall pixel-difference score is low.

I would also retain an `exact` and `safe-refinement` path independently of the lossy matching heuristics. This prevents compression-quality tuning from weakening the fidelity guarantee.

## 6. Improve memory and decoding performance

Here I would borrow an idea from the separate ScanPress and SmallPDF projects we found, but adapt it to your own architecture.

The current encoder already exposes useful stage metrics, including comparator calls, exact hits, refined hits and symbol statistics. I would add a proper performance report that distinguishes:

| Measurement                             | Why it matters                          |
| --------------------------------------- | --------------------------------------- |
| Global dictionary encoded bytes         | Storage and cold-load overhead          |
| Decoded global dictionary memory        | Memory needed to use shared symbols     |
| Maximum live encoder memory             | Large-book processing                   |
| Exact / refined / substituted instances | Whether compression is relying on loss  |
| Generic residual bytes and coverage     | How much material escapes symbol coding |
| Per-page decode latency, cold and warm  | E-ink reading responsiveness            |
| Symbol comparisons per input component  | Matching scalability                    |

The most useful optimization would then be to add a constrained encoding objective: minimize file size subject to fidelity and decoder-resource limits.

That is preferable to assigning arbitrary numerical weights to size and latency. For example, a reader-oriented preset could impose a maximum decoded dictionary size and a maximum estimated page-decode workload, then choose the smallest candidate satisfying those constraints.

Actual hardware measurements should determine suitable limits.

I'd also investigate the memory cost of holding all source pages, extracted symbols and instance bitmaps simultaneously during a full-document encode. A bounded-group processing API may be more valuable than aggressive low-level allocation optimization if peak memory is dominated by document-scale retained state.

## 7. A release issue I would not ignore: Licensing

Your README declares `MIT OR Apache-2.0`, but also cautions that the symbol-matching path includes code adapted from GPL-licensed DjVuLibre.

More concretely, [`src/encode/cc.rs`](https://github.com/liminalism/Lege/blob/main/lege-codecs/jbig2enc-rust/src/encode/cc.rs?utm_source=chatgpt.com) describes itself as a Rust port of DjVuLibre's `cjb2.cpp`, while also calling the implementation clean-room.

Those descriptions need reconciling. An independently implemented algorithm isn't necessarily subject to the original implementation's copyright, but a translated or adapted implementation can be.

This matters because downstream users are treating your library as permissively licensed, including commercial-oriented PDF applications.

Before publishing 0.7.0, I'd perform a source-provenance review of the relevant encoder modules and obtain appropriate licensing review. If necessary, replace adapted code with an independently developed implementation, preserving behavioral tests. Don't simply remove attribution or license notices.

I would classify this as a release-readiness issue, not merely documentation polish.

## 8. What I would leave out of 0.7.0

I found several further opportunities, but I wouldn't make them prerequisites for the symbol-mode release.

Your decoder already handles substantially more of the JBIG2 specification than your encoder emits. That means adding Huffman encoding, additional generic-region templates, generic typical-prediction encoding, and MMR encoding may eventually improve compression or speed, but those features would considerably expand the release scope.

The generic encoder also has a potential performance opportunity: your own benchmark reports 2.47 seconds versus 1.15 seconds for the C encoder. Profiling its arithmetic-coding path and testing row-level typical prediction could help. However, I would first establish how much of that difference comes from core coding versus surrounding work.

I would postpone these features until a measured profile identifies one as a particularly attractive improvement.

## 9. Concrete 0.7.0 execution plan

I would split this into milestones that can be implemented and validated independently. Each should land with a benchmark result and regression tests before work proceeds to the next one.

| Phase | Deliverable                                                                                         | Priority     |
| ----- | --------------------------------------------------------------------------------------------------- | ------------ |
| A     | Establish benchmark corpus and fidelity contracts; document existing refinement behavior accurately | Required     |
| B     | Cost-aware generic residual segmentation                                                            | High         |
| C     | Page-local PDF dictionaries and selective global symbol assignment                                  | High         |
| D     | Cost-based symbol/refinement planner, using existing production refinement encoding                 | High         |
| E     | Pixel-exact symbol preset with decoded-bitmap verification                                          | High         |
| F     | Bounded dictionary memory, grouping experiments and richer performance metrics                      | High         |
| G     | Source license/provenance review, interoperability and API compatibility audit                      | Release gate |
| H     | Generic-region encoding improvements or advanced dictionary refinement                              | Optional     |

### Benchmark and validation requirements

Your existing `multi_page_benchmark.rs` is a good starting point. One detail I noticed is that its symbol and symbol-unify benchmark configurations explicitly set `auto_thresh = false`, whereas the normal text preset enables it. For 0.7.0, benchmark both settings and report the normal shipping configuration separately.

I would establish a fixed collection of scanned documents with text-only pages, difficult aged scans, multilingual glyphs, mathematical notation, very sparse pages, and books with hundreds of pages.

For each new optimization, report output bytes, encoding time, peak resident memory, decoded pixel differences and per-page decode latency. The pixel-exact preset must have zero differing pixels against the supplied bilevel source. Intentionally lossy modes should report differences rather than merely passing a perceptual-similarity threshold.

Keep the existing byte-identical regression tests for unchanged modes, add differential decoding against jbig2dec and at least one independent PDF renderer, and exercise standalone/PDF streams with odd page widths, shared globals and local dictionaries.

For API compatibility, I would preserve the existing `encode_single_image_lossless()` behavior and avoid silently changing `Jbig2Config::text()` in a way that could surprise existing users. New fidelity behavior should have an explicit preset, with a migration path for consumers who want it.

### Suggested release targets

These are engineering targets I would use to evaluate 0.7.0, not predictions of achievable results.

# 5–15%

Stretch goal: smaller symbol-mode output

On a representative text-heavy corpus, relative to 0.6.0

# 0

Differing pixels in the new lossless symbol preset

Exact equality against every input bilevel page

# ≤ 1.10×

Encoder time for the normal symbol preset

Guardrail against significant regression, not a goal to slow it down

# ≤ 1.00×

Cold-page decode latency

No regression against 0.6.0 on the reader-oriented benchmark corpus

Proposed acceptance objectives. Relax the encoding-time target for an explicitly slower maximum-compression mode, not for the default.

## Final assessment

The largest architectural issue I found is the all-global PDF symbol-dictionary policy. The clearest missing capability is an explicitly lossless symbol mode. The most immediately testable compression improvement is smarter residual-region representation.

These fit together naturally. Once symbols can be allocated between global dictionaries, local dictionaries, refinement and generic regions, the encoder can make much better decisions than it can when every symbol is forced into one of only a few categories.

I would build 0.7.0 around that unified decision process, while leaving the existing codec machinery and optimized comparator largely intact.

One final point: the README currently says refinement-assisted coding is not implemented, even though `text_region_refine.rs` clearly contains functioning refinement-encoding machinery. Fixing that description matters both for contributors and for users comparing your library with alternatives. The published documentation also still shows 0.5.x dependency examples while the repository manifest is at 0.6.0.

My recommended 0.7.0 headline would be: Cost-optimized symbol dictionaries, exact-preservation mode, and bounded decoding resources.

That would be a meaningful advance for both Lege itself and the independent developers who are starting to use your codec.