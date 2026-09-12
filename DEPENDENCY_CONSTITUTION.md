# Dependency constitution

This document summarizes plan §§3, 4, 6, 26 and 27. It defines admission policy, not a claim that
the planned dependency graph is currently available or compliant.

## Shipping closure

The admitted implementation universe is Rust standard/toolchain libraries, FCB crates, Asupersync,
FrankenMarkdown and explicitly selected first-party FrankenSuite components. Audit transitive normal
dependencies and the complete resolved build/runtime closure. A first-party root crate can still
pull a noncompliant third-party implementation transitively.

No third-party convenience exception is inherited from example repositories. In particular there
is no implicit serde/serde_json allowance, alternate async runtime, graphics binding, font engine,
parser, regex engine, search engine, serialization package or foreign production service.
Copying, renaming or vendoring an outside implementation does not satisfy first-party ownership.

Cargo features are additive. Empty defaults on one edge do not remove unconditional dependencies
or features enabled elsewhere. Resolve each supported external consumer separately and test host
feature unification. `--all-features` is not the release configuration.

## Named platform allowances

- Apple frameworks for AppKit, Metal, CoreText/CoreGraphics, file events, input, accessibility and
  display timing through the audited first-party boundary.
- Rust compiler/standard library, a qualified dated nightly, linker, pinned Apple SDK and offline
  Metal compiler.
- Project-authored MSL shaders; authoritative product semantics stay in Rust.
- Signing and notarization tooling for native distribution.

These allowances do not authorize wgpu, winit, metal/objc binding crates, Electron, a WebView,
HarfBuzz, FreeType, or C/C++/Objective-C support libraries hidden behind a Rust wrapper.

## Unsafe boundary

Authoritative FCB crates forbid unsafe code. The proposed `franken-macos` system bridge lives in its
own first-party repository, consumed by FCB and FrankenMarkdown's optional Mac adapter, and may contain
only the minimum audited ABI implementation behind safe owned APIs. Record SDK signatures,
retained/borrowed ownership, nullability, integer widths, thread affinity, callbacks, reentrancy,
panic/exception boundaries and GPU resource lifetimes. A safe function name is not a soundness proof.

Inherited first-party unsafe boundaries, especially selected storage VFS code, are inventoried
separately. Do not broaden the bridge into parser, indexing, plugin or product logic. Never add
blanket unsafe `Send`/`Sync` or raw pointers disguised as integers to simplify integration.

## Upstream admission map

| Owner | Required narrow contract | Main qualification concern |
|---|---|---|
| Asupersync | Desktop/embedding runtime slice | Real closure, explicit runtime/clock/worker ownership, bounded cancellation |
| FrankenMarkdown | Shared lexers, provenance, flow/display, fonts/math/diagrams | Upstream implementation, bounded steps, existing output compatibility |
| FrankenSQLite | Selected persistence facade/VFS | Thread-affine actor, coherent runtime, SQL subset, commit/recovery semantics |
| FrankenTUI | Non-terminal panes and wide prefix utilities | Clean profile, checked fixed-point sums and structural edits |
| FrankenNetworkX | Compact directed views and selected kernels | Validated CSR, multiplicity/evidence separation, bounded deterministic behavior |
| FrankenTerm | Glyph keys and atlas policy | Narrow extraction; no whole terminal GUI or assumed deferred transfers |
| FrankenManim | Digest/envelope/cache and retained primitives | Dependency factoring, actual integrity contracts, bounded ownership |
| FrankenThreeD | Handles/resource identity | Owner identity in addition to slot/generation; no assumed absent renderer |
| CASS | Bounded readiness/evidence selection | Clean extraction, explicit omissions and approximate token counts |

All reusable Markdown work belongs in FrankenMarkdown immediately. An FCB-local parser,
highlighter, typesetter, flow engine or generic Markdown exporter is not an acceptable interim
implementation. Shared work in other domains lands in its owning repository.

## Integration record

For each admitted upstream extension, retain:

1. Owner repository and exact public API actually consumed.
2. Inspected status versus implemented status.
3. Committed revision, package/source identity, license and selected features.
4. Resolved normal/build dependencies and native/unsafe boundaries.
5. Upstream tests and an independently built FCB consumer result.
6. Compatibility, memory and performance limits; any remaining blocked capability.

Create the plan's `SUITE.lock`-style manifest when real selections exist. Research file blob hashes
are not commit pins. A downstream consumer cannot rely on a sibling's private `[patch]` settings.
Development may temporarily use local paths, but accepted release dependencies must resolve to
committed compatible versions without a neighboring dirty checkout.

## Development tools and oracles

External development-only comparison/fuzz tools may run outside the shipping closure. Record
their inputs, versions and role, and prevent them from becoming production dependencies through
build scripts, bundled generated code or subprocess fallbacks. Shell/Git tooling used to develop
FCB does not authorize the product to execute commands while opening an untrusted repository.

G0 admits the exact foundation slice and tracks unintegrated components separately. Missing runtime
or native-boundary proof blocks G0. Every new shipping edge must pass admission; no full release
passes with an unresolved shipping closure violation.
