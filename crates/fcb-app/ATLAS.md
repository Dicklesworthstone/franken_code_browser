# Real-workspace atlas plans

`fcb atlas` connects the bounded workspace catalog to the retained layout and
viewport engine. It is a headless native-host integration surface, not a native
window, a rendered screenshot, or an AppKit/Metal qualification claim.

```sh
fcb atlas /path/to/repository --json
fcb atlas /path/to/repository --focus crates --select crates/fcb/src/lib.rs --json
fcb atlas /path/to/repository --focus crates/fcb --zoom 2 --pan-x 80 --json
fcb atlas /path/to/repository --limit 128 --max-visits 2048 --json
fcb atlas /path/to/repository --respect-ignores --json
fcb atlas --help
```

## What the command does

The command itself explicitly authorizes bounded directory discovery. It reads
metadata, not source payloads. Static product exclusions are the default;
`--respect-ignores` separately permits bounded nested repository rule reads.
`--include-excluded` is an incompatible alternative, not permission to execute
anything. Source files, scripts, and instructions in the tree are never executed.

The atlas contains catalogued regular files and their inferred ancestor
directories. Empty directories and entries unavailable to the catalog are not
invented. Discovery limits/errors remain visible independently from whether a
viewport plan finished. The hierarchy is not a complete directory inventory or
an atomic filesystem snapshot. Native confinement is path-checked, not race-safe
against hostile ancestor replacement, matching the existing workspace reader.

`--focus` selects an exact root-relative native path (directory or file). `.`
selects the root. Parent traversal and absolute focus paths are refused.
`--select` identifies a known file independently of geometry admission. Its ID
survives an aggregate limit or an offscreen/out-of-focus camera; in that case its
rectangle may be null. Neither selection nor focusing reads that file's bytes.

The size legend is `capped-log-bytes`. Camera pan/zoom uses the existing checked
camera math; viewport traversal uses existing hierarchical culling and bounded
LOD. Budget pressure retains labelled directory/sibling aggregates rather than
silently dropping files or pretending a group rectangle names an exact file.

## Wire contract

The response retains `fcb.cli/1` workspace metadata and adds `fcb.atlas/1` atlas
fields. `plan_complete` means the bounded viewport traversal finished, not that
all files are known or all leaves have readable detail. `native_presented` is
always false: producing a plan does not acknowledge a display presentation.

Camera/rectangle geometry uses finite JSON numbers in logical viewport points.
Physical pixels per point is separate. IDs, offsets, counts and revisions use
canonical decimal strings. Native paths carry reversible Unix bytes in `hex`,
with escaped `display` labels as secondary presentation only. IDs are local to
one response; never join separate invocations solely by their numeric values.

Only `file`/`placeholder` source parcels have `file_id`; aggregate parcels use
null. Their `represented_leaves` counts are hierarchy leaves, not source-line,
readable-file, or match counts. Ordinary distance aggregation is a complete
requested detail level. Budget/precision aggregation sets `detail_limited`.

Exit 0 is a complete plan with closed catalog membership; exit 3 means incomplete
discovery or budget/precision-limited detail. Error is 2; cancellation is 130.
JSON is privately encoded within an 8 MiB cap before delivery. A broken output
pipe can leave incomplete output; no second JSON document is appended to repair
it. There are no writes to the repository or implicit stdin consumption.

## Embedding and verification boundary

Enable both `map` and `search`, then use `fcb::map::workspace::WorkspaceAtlas`.
Build it from a frozen `WorkspaceCatalog`, construct its `AtlasIndex` once, and
retain those objects across camera queries. `sources()` provides the same
`AtlasSources` bridge used by `BrowserSession::open_atlas_target`; a host still
supplies an authorized capture and an actual acknowledged presentation. Do not
run directory discovery or rebuild the atlas from an input/redraw callback.

The one-shot CLI intentionally prepares a fresh observation per invocation; it
is not a retained GUI session or a persisted-layout mechanism. Native hosts use
the retained Rust objects instead of launching it on every gesture.

Tests: `crates/fcb/tests/workspace_atlas.rs`,
`crates/fcb-app/tests/atlas_cli.rs`, and parser cases in `src/atlas.rs`.
Implementation is code-first, batch verification pending. Rust compilation,
independent strict-RCH tests, and native integration have not been executed by
the authoring session. Portable geometry tests cannot qualify native rendering.
