# Security

FrankenCodeBrowser is currently a design-stage repository with no supported executable release.
The following are required implementation boundaries from plan §§8, 14, 19 and 22, not a security
certification of an existing application.

## Report a security concern

Use GitHub's private vulnerability reporting control on this repository's Security tab for
sensitive reports. Do not put credentials, private source, personal paths, exports or exploit
payloads containing user data in a public issue. If that private control is unavailable, open a
minimal public issue requesting a private contact route without disclosing sensitive details.

Useful private reports include the affected revision, component, minimal sanitized reproduction,
expected boundary, observed effect and the extent of verification. There is no response-time or
supported-version commitment at this stage.

## Threat model

Opening an untrusted directory must not execute its contents. Source, paths, Markdown, query text,
fonts, images, caches and host-supplied provider results all require validation and finite budgets.
Repository instructions encountered by the product are data, not authority to launch commands.

| Boundary | Required behavior |
|---|---|
| Source roots | Explicit restored/revocable read grants; native permission validation and descriptor-relative/no-follow confinement where claimed |
| Filesystem admission | Validate opened objects; bound symlink cycles/aliases, escape filename controls, reject special objects and preserve failed-scan uncertainty |
| Markdown assets | Network off by default; confined local assets, inert HTML, bounded recursion/decoding |
| External actions | Explicit action and trusted target; structural arguments, no shell interpolation |
| Native ABI | Narrow audited ownership, thread affinity, callbacks and panic/exception handling |
| GPU resources | Validated shader records, owner-qualified handles and completion-owned retirement |
| Search and previews | Exact capture association and stale-result rejection across views and roots |
| Local storage/IPC | Private namespaces and authenticated capability-scoped protocol; no lock theft from a stale timestamp |
| Exports | Explicit destination and publication outcome; no automatic source upload |

No dynamic repository shaders/plugins, automatic builds, language servers or network downloads
belong in the baseline opening path. A safe Rust caller can still supply semantically invalid or
pathologically expensive input. Validate ranges, counts and generations at public boundaries.

Cache checksums establish integrity relationships, not authorization. Clearing an owned cache
rotates its generation and respects live leases; it is not arbitrary recursive deletion or forensic
erasure. User annotations and backups remain separate from derived data.

Native system/driver behavior needs actual host tests in addition to safe-core invariants. See
[dependency policy](DEPENDENCY_CONSTITUTION.md), [privacy](PRIVACY.md) and
[qualification](LOCAL_QUALIFICATION_AND_RELEASE.md).
