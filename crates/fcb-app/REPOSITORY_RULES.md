# Bounded repository ignore rules

Implementation and regression tests are committed, code-first/batch-verification
pending. This document is not a Rust execution receipt or full Git-ignore
conformance claim. The implementation extends the existing first-party matcher,
source walker, workspace catalog, capture/search engines, and snapshot exporter.
It introduces no subprocess, network fetch, new dependency, or alternate parser.

## Commands

```sh
fcb inspect ROOT --workspace --respect-ignores --json
fcb search ROOT --workspace --respect-ignores --path 'src/lib' --json
fcb search ROOT --workspace --respect-ignores --text 'pub fn' --json
fcb search ROOT --workspace --respect-ignores --whole-file --text needle --json
fcb snapshot save ROOT --respect-ignores --output NEW_SNAPSHOT.fcbs --json
```

`--workspace` authorizes directory enumeration. `--respect-ignores` separately
selects rule-file reading. Ordinary workspace commands without this flag retain
static product exclusions and never read configuration as configuration. Path
search and workspace inspection still read no **source payload**, but rule-aware
versions can read configuration bytes and report them separately.

`--include-excluded` remains the explicit static all-regular-files scope (FCB-owned
artifacts are still excluded). It cannot be combined with `--respect-ignores`:
the two switches are not a hidden override of one another. Rule flags are refused
on single-file commands and offline snapshot inspection/search/reading. Literal
query values or filenames equal to a switch remain data when supplied as option
values or after `--`. The separate `symbols` command is unchanged by this work.

Snapshot destinations retain the existing exclusive no-overwrite contract. Save
still exports plaintext source bytes; this feature is not encryption, secret
redaction, or permission to publish an archive. Configuration read for policy is
not automatically added to exported members. If a rule file is also an admitted
ordinary source member, its later payload capture is independently counted.

## Policy evaluation

The walker observes `.gitignore` and then `.fcbignore` before enumerating each
included directory. Rules are relative to their containing directory. Parent
layers precede nested layers, later matching rules win, and sibling prefixes do
not leak into each other. An excluded directory is not entered to discover child
rules. Re-inclusion requires its parent path to be traversable. FCB-owned artifact
exclusions remain outside repository-rule override authority.

The existing supported Git-style subset includes byte-literal components,
`*`, `?`, bracket classes, segment `**`, root-relative patterns, trailing-slash
directory patterns, negation, escapes, comments, and trailing-space handling.
LF and CRLF files work; an initial UTF-8 BOM is stripped for parsing, while the
exact original observation remains available for identity. Native path bytes
remain authoritative, including backslashes and non-UTF-8 directory names.
Wildcard matching operates on bytes, not Unicode grapheme or case-folded names.

This is not complete Git compatibility. Global Git excludes/configuration,
`.git/info/exclude`, arbitrary rule-file locations, external tools, and nested
configuration formats other than these two filenames are not loaded. Invalid
UTF-8/NUL, malformed compiler inputs, POSIX class/collating syntax, and empty
path-component patterns such as `a//b` are refused rather than reinterpreted as a
different policy. Rule-file symlinks and special objects are not followed/read.

A rule file is accepted transactionally: a valid prefix preceding an invalid
pattern never becomes active. Its containing subtree is withheld on read,
encoding, unsupported-pattern, or admission failure. The walker can continue
independent siblings. Match-work exhaustion withholds that path instead of using
the last partially evaluated rule as an inclusion decision.

A missing `.gitignore` or `.fcbignore` is ordinary absence, not a policy failure.
An unreadable, changed, excessive, or unsupported present file is different.
Diagnostics identify the rule path and stable refusal code without echoing its
contents. Files below an unexamined directory do not become fake empty captures
or false tombstones: they remain unknown membership.

## Bounds and execution

The rule-aware API reserves compiled state, observations, diagnostics, and read
scratch before allocation. Default `RuleLimits` admit 16 KiB per rule file,
64 KiB of actual configuration reads per discovery session, 256 retained rule-file
observations, 1,024 rules, and 512 bytes per input pattern line. Compiled-state
admission is separately limited to 16 MiB. Diagnostics retain at most 32 bounded
native paths and count additional omitted reports.

Configuration checks are capped at 32,768; read calls at 65,536. The per-path
matcher allowance is 1,000,000 charged steps, with a 64,000,000-step session cap.
Host-selected limits can be smaller within hard API ceilings. The CLI currently
uses these fixed defaults; ordinary source-capture byte flags do not enlarge or
conceal configuration I/O. Combining different individual resource maxima may
still exceed the shared managed-memory budget and be refused.

Actual reads use buffers no larger than 4 KiB and a one-byte growth/EOF probe,
with grant/cancellation and global byte/call checks. No read-to-end follows a
mere metadata size check. Probes require remaining byte allowance too: exact
budget exhaustion can leave a fully read prefix unaccepted when EOF cannot be
verified within the remaining allowance. No speculative extra byte escapes the
session limit. The existing compatibility walker also receives bounded per-file
reads, but only the explicit rule-aware API supplies all session-wide policy caps.

Matching reuses the compiler's AST through an iterative wildcard engine; it does
not recursively enumerate every wildcard partition. Binary comparisons, class
membership, prefix checks, and backtracking spend explicit work. There is no
claim of constant wall-clock latency: directory and configuration I/O remain
worker operations. Generic filesystem calls are not an OS deadline.

Root grants and observed/opened file identity are checked, symlink rule files are
refused, and final metadata changes invalidate a read. The path-based native
boundary is **not** race-safe root confinement against hostile ancestor
replacement. These are read-only observations, not atomic filesystem snapshots.
No source/configuration command is executed by opening a repository.

## Coverage and saved identity

`BoundedDiscovery::open_rule_aware` exposes a private admitted `RepositoryRules`
owner. `WorkspaceCatalog::open_rule_aware` carries its evidence across stable
catalog publication while retiring traversal descriptors and queues. Existing
`open_metadata_only` and `WorkspaceCatalog::open` retain their original scope.
Ordinary library construction does not start rule reads or capture source.

A rejected rule subtree makes `discovery_complete` false. That propagates through
both native-path queries and exact captured/whole-file searches. Capturing every
known admitted member does not repair missing membership evidence. A zero-hit
query under unknown policy remains partial, not an exhaustive negative. It is
possible to have zero unavailable **known files** while discovery is incomplete.

Wire receipts add `rule_files_enabled` and a `rule_policy` object. They separate
configuration bytes/calls/checks, successfully read files, retained files/rules,
compiled admission, matching work, failed files, unresolved paths, and bounded
diagnostics. `observed_rules_complete` describes the evaluated policy;
`scope_complete` additionally requires complete discovery. Human output escapes
native path/control characters and labels partial rule scope explicitly.

With the `snapshot` library feature, frozen catalogs use a versioned SHA-256
policy identity over their exact rule observations in canonical native-path
order. It includes a completeness marker and length-prefixed paths/contents.
Edits therefore change the saved policy key, even when filenames or member counts
stay the same. The existing snapshot comparator can distinguish policy changes
from same-policy additions/deletions without a new archive format.

Without `snapshot`, search-only hosts get the named policy profile and retained
raw observations but no implicit store/hash dependency. The fingerprint is an
observation identity, not an authenticity certificate, freshness proof, or source
grant. Partial discovery and rule failures remain partial when exported and
restored. Offline search never reopens or re-executes original rule files.

## Tests

`fcb-source/tests/repository_rules.rs` exercises actual directory discovery,
one-entry pages, nested/application precedence, raw paths, excluded parents,
unsupported rules, symlinks, quotas, diagnostics, cancellation, and metadata-only
compatibility. `rule_admission_boundaries` checks probe admission and refused
empty-component syntax. The matcher unit tests compare against an independent
reference and exercise stack-safe repeated wildcards and explicit work refusal.

`fcb/tests/workspace_repository_rules.rs` drives real discovery into production
capture/index/query APIs and saved-source restoration; its provider rejects any
attempt to capture excluded or unknown-policy subtrees. `repository_rule_cli`
checks both workspace response layouts, snapshot export/reopen, control paths,
partial scopes, and the actual binary. It pins a policy digest independently
constructed with Python `struct`/`hashlib`. Parser tests verify permission and
switch-value boundaries. These tests are present, not claimed executed here.
