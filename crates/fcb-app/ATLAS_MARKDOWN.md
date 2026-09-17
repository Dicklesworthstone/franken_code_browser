# Atlas search to captured Markdown

The atlas can open a retained source-text hit's file through the same document
reader as `fcb markdown`, without rereading the working tree or changing map
geometry.

```sh
fcb atlas . --text 'installation' --markdown-hit 0 --json
fcb atlas . --text 'installation' --markdown-hit 0 \
  --markdown-heading installation --markdown-lines 30 --json
fcb atlas . --text 'needle' --preview-hit 0 --markdown-hit 0 --json
```

`--markdown-hit` explicitly treats that captured file as Markdown; it is not
extension detection. The index is zero-based in the retained hits of THIS
invocation, never a durable identity to carry across separate CLI calls.
It requires `--text`, must be below `--match-limit`, and fails if that occurrence
was not actually retained. A filename/path result does not pin source bytes
and cannot be used for this route.

The default view starts at the document beginning, not at an invented rendered
position for the source match. `--markdown-heading` chooses an upstream canonical
heading slug, and `--markdown-lines` admits 1 through 1024 logical rows (default
40). Both require `--markdown-hit`. The source can match Markdown punctuation,
link destinations or stripped syntax; these are not automatically represented
as rendered-text highlights. `rendered_hit_highlight` remains false. The exact
original hit range, hexadecimal bytes and matched source text remain separate
from the logical document output.

## Identity and effects

The selected source comes from `AtlasTextOverlay::select_hit` and its borrowed
CompleteCapture. No filesystem path is reopened. Native path, FileId,
SourceRevision and atlas node continue to identify the same frozen observation.
The source/query/grant are revalidated around document preparation and before
accepting its output. Revocation can stop a new delivery but cannot retract
source bytes already given to an external consumer.

Markdown parsing, flow, headings and source maps still belong to FrankenMarkdown.
This command does not add another document engine. It invokes the shared
`markdown::write_capture` composition on the selected retained bytes. Opening
the document does not follow links, load images, process includes, execute
commands, fetch the network, write a file or claim a native presentation.

The document limit remains 64 KiB of complete UTF-8 source (BOM supported),
independent of the atlas's larger source-search capture allowance. Larger
selected files and UTF-16 documents return explicit document-reader errors,
not truncated documents. Original-byte/context previews and source search keep
their existing UTF-16 support. For an explicit current-file read with a larger
document allowance, the standalone `fcb markdown FILE --max-source-bytes N`
route supports up to 256 KiB; that is a new observation, not the old hit.

## Response

`document_preview` is null without `--markdown-hit`. Otherwise it contains the
selected hit/node/original bytes and a nested `fcb.document/1` document object.
The nested object's `additional_source_bytes_read` is zero. Original match
identity is distinct from the document's rendered UTF-8 coordinate domain.
The ordinary `text_preview` can be requested independently in the same result.

Search completeness and document visibility remain independent. A valid full
document does not promote truncated search, unavailable captures or incomplete
discovery to complete. A bounded document window also yields exit 3. An error
discards the private candidate response; interrupted stdout does not append a
second JSON object. The existing 8 MiB total response admission still applies.

Native hosts can compose `AtlasTextOverlay::select_hit`, `selection.capture()`
and `fcb::document::reader::DocumentReader::prepare` directly. Retain their
objects across navigation, and revalidate the source overlay/grant before a
new action. Do not launch the one-shot CLI on each camera gesture.

Tests: `crates/fcb-app/tests/atlas_document_cli.rs`, atlas parser cases, and
`crates/fcb/tests/atlas_document_workflow.rs`. The latter exercises real directory
capture/search and then modifies the live file before opening the old retained
Markdown. Tests also cover revoked/stale delivery, unchanged geometry and source
I/O counts, unsupported encoding, source admission, missing hits/headings and
partial-search preservation. Code-first, batch verification pending: the
Rust suites and native/RCH qualification were not executed by this authoring
session. These additions do not close native or release gates.
