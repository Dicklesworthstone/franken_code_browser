# Small-text tile-memory qualification

The retained grayscale presenter admits source densities from 0.04 pixels per
source point on Apple-family GPUs. Below 0.1, the renderer accumulates glyph
contributions in RGBA16Float tile memory and resolves once to BGRA8 within the
same render pass. At larger densities the existing BGRA pipeline remains active.
CPU fallbacks, coverage admission, source geometry, clipping, instance order and
presentation fences remain authoritative. Unsupported devices retain the 0.1
floor. No text is replaced by rectangles or reduced-resolution substitutes.

Why: repeated UNorm blend rounding failed the independent source-image oracle
at density 0.08/phase 0.25 for frozen fcb-analysis/src/lib.rs tile 66: maximum row
error 13.19%, above the existing 12% gate. Floating accumulation lowered that case
to 0.31%. Do not simply lower the old renderer's density floor.

Proof on M4 Pro/macOS 26.2:

- 192 comparisons across 12 actual project tiles, densities 0.1/0.08/0.06/0.04 and
 four quarter-pixel phases passed the unchanged independent CoreText image gate.
- The always-floating reference reached worst-row 3.81% and global 0.75% error.
- The single-pass memoryless prototype matched the two-pass floating reference
 byte-for-byte in 56 comparisons at 50,001 and 800,016 instances.
- Integrated default and fixed-capture scalar Metal suites passed. The latter
 includes the new density interval, rejects density 0.039, and checks two
 concurrently submitted command buffers against exact separate image goldens.

The memoryless attachment is render-target-only, cleared on entry and discarded
on exit. Only the final BGRA attachment is stored. There is no full-frame float
image in external memory, readback, extra command queue or GPU wait. Command
buffers retain their attachments; the presenter still bounds outstanding flights
to two. The precision path bounds target area to 32 million pixels.

Run the existing native glyph suite with FCB_METAL_FIXED_CAPTURE=1 and
FCB_CAPTURE_SCALAR=1. For real project coverage, run AtlasRenderProfile against
an immutable corpus/oracle/cache with FCB_RGB_SCENE_PREPARE=1,
FCB_RGB_CORPUS_PIXELS=1, FCB_CAPTURE_SCALAR=1, FCB_RENDER_LAYOUT=parcel and
FCB_LOW_DENSITY_QUALIFICATION=1. Thresholds are unchanged.

Paired prototype GPU timing favored memoryless over two-pass float in 17/20 and
16/20 rounds. Variance prevents a qualified speedup claim; this is not a measured
comparison against the original production path or delivered FPS. Warm cached
image layers still cover most full-atlas text: lower density alone is not a cure
for all zoom stalls. Raw evidence is retained locally under
/Volumes/USB_NVME/fcb-zoom-20260922/overview/.

Apple references:
https://developer.apple.com/documentation/metal/mtlstoragemode/memoryless
https://developer.apple.com/videos/play/wwdc2020/10632/
