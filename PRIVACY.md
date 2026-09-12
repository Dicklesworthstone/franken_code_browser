# Privacy and source handling

This document states the planned behavior of FrankenCodeBrowser. There is no running application
or implemented privacy control to certify yet. Plan §§17, 19 and 22 define the full contract.

## Local by default

The application must not upload source, send telemetry, download a model or fetch document assets
from the network by default. Opening a root grants bounded read access to that root. A symlink,
Markdown link, deep link or robot request does not grant access to another root.

Embedded hosts explicitly provide source and resource capabilities. Shared immutable caches need
an authorized accounting/privacy domain; matching content hashes do not make another workspace's
private content discoverable. Closing one browser cannot expose another instance's annotations.

## Storage and retention

Source captures, lexical indexes, previews and thumbnails are source-derived data and can contain
secrets. Store them in user-private, owned namespaces with quotas and bounded generation retention.
Do not index the application's own cache/store/export scratch when it lies under a selected root.

Bookmarks, annotations and preferences are personal authoritative state. Index rebuilds and
derived-cache cleanup must preserve them and their backups. Clear operations identify an owned
namespace, revoke obsolete writers and report reclamation deferred by active leases. They do not
promise forensic erasure from filesystem snapshots, backups, OS caches or storage media.

## Diagnostics and export

Default logs contain bounded timings, counts, IDs and error classes rather than source payloads
or secret-bearing paths. Unredacted diagnostics and source-containing exports require an explicit
request. A report must disclose included snippets/images and the limits of redaction.

Reading trails retain exact source anchors. Exported packs may disclose secrets present in source;
export requires explicit destination and scope. Record byte/character budgets and omissions, label
token estimates, and distinguish canceled staging from completed destination publication.

Clipboard requests are separately bounded. Selecting a giant file does not eagerly materialize it,
and over-budget copying cannot silently truncate. Logical Unicode, exact original bytes and
rendered Markdown text are distinct operations.

Private source should not be attached to public bug reports. See [SECURITY.md](SECURITY.md).
