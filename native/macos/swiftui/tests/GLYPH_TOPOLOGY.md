# Four-vertex glyph qualification (2026-09-21)

One triangle strip per glyph/background instance replaces the six-vertex list.
Corners (0,0), (1,0), (0,1), (1,1) cover the same triangles, winding and diagonal.
Instance order, blend states, masks, clipping, floating-point expressions and
camera transforms are unchanged. No random inputs are used.

Run `sh scripts/compare_glyph_topology.sh` with FCB_PROFILE_OUTPUT set to a
new external directory and FCB_BRIDGE_LIB to the existing bridge archive.
The harness compares production against committed six-vertex source 7589f2b.
It retains its outputs and deletes nothing.

M4 Pro/macOS26.2 (25C56): both 50,001 and 800,016-instance scenes passed exact
8,785,152-byte BGRA equality at each of 28 transforms: seven scales from0.1
to4 and four quarter-pixel phases. Matching instance counts, nonempty pixels,
and zero fallback tiles were required. Three warmup rounds preceded20 paired
ABBA/BAAB rounds per workload, using real GPU start/end timestamps.

| Instances | Control median of round medians | Strip | Faster rounds |
| --- | --- | --- | --- |
| 50,001 | 5.378 ms | 4.151 ms | 15/20 |
| 800,016 | 42.570 ms | 30.030 ms | 19/20 |

These are diagnostic timings, NOT delivered FPS or a qualified speedup.
Control round-median CV was23.47% and15.17% on this heavily contended host.
The certain reduction is six to four vertex invocations per instance.
Default and fixed-capture scalar native Metal tests passed, including the
independent CoreText/retained-image references. Scoped UBS passed.

Raw evidence: /Volumes/USB_NVME/fcb-zoom-20260921/quad-strip/.
Main-thread frame delivery remains open. Off-main display-link, early present,
direct nextDrawable and custom layer-hosting trials did not qualify a win;
none is included in this change.
