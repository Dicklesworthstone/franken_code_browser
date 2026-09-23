# Privacy and source handling

FrankenCodeBrowser 0.1.0 is a local source browser for macOS. This page describes its current
native-app behavior. Plan §§17, 19 and 22 set broader design goals that are not all implemented.

## Current native app

- You choose a local project folder. The app reads source files beneath it to build the atlas,
  show syntax-highlighted text, open a file in the reader, and search captured source.
- The Mac App Store build uses the App Sandbox and a read-only folder grant. It keeps a
  security-scoped bookmark on this Mac so it can reopen a recently selected project. A saved path
  alone is not treated as permission. The direct-download build uses its own local recent-folder
  preference and is separately signed and notarized.
- Prepared source and image data can be cached in the user's private macOS Caches directory to
  speed later openings. Those derived files may contain text from selected source files. Recent
  folder bookmarks and preferences are stored locally in the app's user defaults. The App Store
  build's container holds its own cache and preferences.
- The native app has no account, advertising, analytics, or source-upload feature. The App Store
  build requests no network-client entitlement. Opening a project does not run its code or fetch
  remote resources. Apple and macOS may separately process App Store purchases, downloads, crash
  reports, and system diagnostics under Apple's own policies.
- Removing the app may leave its local cache and preferences on the Mac. They can be inspected or
  removed through Finder in the app's container or user Library; removing the cache means it will
  be rebuilt when a project is opened again.

For questions about this policy, use the [project issue tracker](https://github.com/Dicklesworthstone/franken_code_browser/issues)
or the developer's [contact page](https://www.jeffreyemanuel.com/contact). Do not include private
source, credentials, or personal paths in a public issue.

## Longer-term privacy requirements

The application must not upload source, send telemetry, download a model or fetch document assets
from the network by default. Opening a root grants bounded read access to that root. A symlink,
Markdown link, deep link or robot request does not grant access to another root.

Saved root paths are not native permissions. Restore access through the qualified native route;
revocation blocks fresh reads and pending deliveries/exports while native operations drain safely.
Already returned host data, clipboard contents and completed exports cannot be retracted by FCB.
The selected host policy states whether previously displayed captures are withdrawn.

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
