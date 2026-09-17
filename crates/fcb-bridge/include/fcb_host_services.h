#ifndef FCB_HOST_SERVICES_H
#define FCB_HOST_SERVICES_H

#include <stdint.h>
#include "fcb_reader_sessions.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Synchronous worker services. Do not call from a redraw/input callback.
 * Every non-null input is readable, NUL-terminated UTF-8 and remains stable
 * until return. NULL/invalid UTF-8 inputs return NULL. A non-null return is
 * owned by this exact library instance: release it once with fcb_free_string.
 * Do not mutate returned bytes/terminators or free them with a foreign allocator.
 * No function claims native presentation, atomic snapshots or race-safe
 * confinement against replacement of ancestor directories.
 *
 * Structured calls return the application's bounded JSON, including ordinary
 * error and partial results. A non-null pointer alone does NOT mean completion.
 * NULL means no complete response could be handed off (or invalid C input).
 * The one-shot functions BELOW make independent source observations. For a
 * pinned source across calls, use the included fcb_reader_sessions.h API.
 */

/* Legacy exact whole UTF-8 source, <= 4 MiB; NULL on oversize, invalid UTF-8,
 * embedded NUL, changed source or read refusal. Never truncates or rewrites.
 * Empty source returns a non-null allocated empty string. */
char *fcb_read_file(const char *path);

/* fcb.cli/1 source-window JSON; offsets/lengths are original bytes. */
char *fcb_read_file_window(const char *path, uint64_t offset, uint64_t bytes);

/* fcb.cli/1 source-line JSON; first line is one-based, not a visual row. */
char *fcb_read_file_lines(const char *path, uint64_t line, uint64_t lines);

/* fcb.cli/1 exact captured-workspace text search. No expression reinterpretation. */
char *fcb_search_workspace(const char *root, const char *query);

/* Metadata-only fcb.cli/1 + fcb.atlas/1 viewport plan, including explicit
 * partial discovery and reversible paths. This does not acknowledge a frame. */
char *fcb_atlas_plan(const char *root);

/* Compatibility world/files/path/x/y/w/h/bytes/n/tex JSON, fcb.host-atlas/1.
 * Source profiles are bounded, neutral line density, NOT syntax highlighting.
 * n is encoded profile-row count; source_lines is a separate string or null.
 * NULL on incomplete discovery or paths unsupported by the legacy UTF-8 schema.
 * The versioned plan above remains available for those machine-readable states. */
char *fcb_atlas_layout(const char *root);

/* fcb.document/1 logical Markdown flow. Line is a one-based rendered row;
 * width is logical character cells, not native font measurement. */
char *fcb_markdown_window(const char *path, uint64_t line, uint64_t lines, uint64_t width);
char *fcb_markdown_heading(const char *path, const char *heading, uint64_t lines, uint64_t width);

/* NULL is a no-op. Non-null must be a live, unmodified result from this library. */
void fcb_free_string(char *result);

#ifdef __cplusplus
}
#endif
#endif
