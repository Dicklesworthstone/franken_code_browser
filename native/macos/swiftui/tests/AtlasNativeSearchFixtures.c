// Fixed C-boundary fixture, not a substitute Rust engine. Sequential tests only.
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <assert.h>
#include <stdatomic.h>

static char *responses[5][8]; // open, begin, step, page, captured reader import
static unsigned cursors[5];
static _Atomic unsigned counts[9]; // create/open/begin/step/page/close/alloc/free/import
static int cancel_step, refuse_create;
extern void fixture_mark_canceled(void *);

void fixture_reset(void) {
    for (unsigned i = 0; i < 5; ++i) {
        for (unsigned j = 0; j < 8; ++j) { free(responses[i][j]); responses[i][j] = NULL; }
        cursors[i] = 0;
    }
    for (unsigned i = 0; i < 9; ++i) atomic_store(&counts[i], 0);
    cancel_step = refuse_create = 0;
}
void fixture_set(unsigned kind, unsigned index, const char *text) {
    assert(kind < 5 && index < 8);
    free(responses[kind][index]); responses[kind][index] = strdup(text);
}
unsigned fixture_count(unsigned key) { assert(key < 9); return atomic_load(&counts[key]); }
void fixture_cancel_step(void) { cancel_step = 1; }
void fixture_refuse_create(void) { refuse_create = 1; }
static char *next(unsigned kind) {
    unsigned index = cursors[kind]++;
    assert(index < 8);
    if (!responses[kind][index]) return NULL;
    ++counts[6]; return strdup(responses[kind][index]);
}
uint64_t fcb_atlas_create(void) { ++counts[0]; return refuse_create ? 0 : 42; }
char *fcb_atlas_open_cancelable(uint64_t handle, const char *root, uint64_t files,
                              int32_t (*poll)(void *), void *context) {
    assert(handle == 42 && strcmp(root, "/project") == 0 && files == 4096);
    assert(poll && context); ++counts[1];
    if (poll(context)) return NULL;
    return next(0);
}
char *fcb_atlas_search_begin(uint64_t handle, uint64_t generation, const char *query,
                           uint64_t matches, uint64_t files, uint64_t file_bytes, uint64_t source_bytes) {
    assert(handle == 42 && generation == 1 && query && strlen(query));
    assert(matches == 1000 && files == 4096 && file_bytes == 1024 * 1024 && source_bytes == 32 * 1024 * 1024);
    ++counts[2]; return next(1);
}
char *fcb_atlas_search_step_cancelable(uint64_t handle, uint64_t generation,
                                     int32_t (*poll)(void *), void *context) {
    assert(handle == 42 && generation == 1 && poll && context); ++counts[3];
    if (cancel_step) fixture_mark_canceled(context);
    // Return an allocated response even on cancellation to test the release race.
    int canceled = poll(context); assert(canceled == (cancel_step != 0));
    return next(2);
}
char *fcb_atlas_search_page(uint64_t handle, uint64_t generation, uint64_t start, uint64_t limit) {
    assert(handle == 42 && generation == 1 && start >= 64 && start <= 1000 && limit == 128);
    ++counts[4]; return next(3);
}
char *fcb_atlas_search_open_reader(uint64_t handle, uint64_t reader, uint64_t generation, uint64_t hit) {
    assert(handle == 42 && reader == 100 && generation == 1 && hit == 1);
    assert(counts[5] == 0); ++counts[8]; return next(4);
}
uint8_t fcb_atlas_close(uint64_t handle) { assert(handle == 42); ++counts[5]; return 1; }
void fcb_free_string(char *text) { if (text) { ++counts[7]; free(text); } }
