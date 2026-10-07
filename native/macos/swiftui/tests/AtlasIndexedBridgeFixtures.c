/* ABI-shape/allocation fixtures only. These symbols do NOT implement Rust. */
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
static const uint64_t handle = UINT64_C(18446744073709551500);
static unsigned calls, allocated, freed;
static int mode;
extern void indexed_fixture_cancel(void *);
void indexed_fixture_reset(void) { calls = allocated = freed = 0; mode = 0; }
void indexed_fixture_mode(int next) { mode = next; }
unsigned indexed_fixture_calls(void) { return calls; }
unsigned indexed_fixture_live(void) { return allocated - freed; }
static char *result(void) {
    if (mode == 1) return NULL;
    char *p;
    if (mode == 2) { p = malloc(2); assert(p); p[0] = (char)255; p[1] = 0; }
    else if (mode == 3) { size_t n = 8 * 1024 * 1024 + 1; p = malloc(n + 1); assert(p); memset(p, 'x', n); p[n] = 0; }
    else { p = strdup("{\"owned\":true}"); assert(p); }
    ++allocated; return p;
}
static void poll_now(int32_t (*poll)(void *), void *ctx) {
    assert(poll && ctx && poll(ctx) == 0);
    if (mode == 4) { indexed_fixture_cancel(ctx); assert(poll(ctx) != 0); }
}
uint64_t fcb_atlas_create(void) { ++calls; return handle; }
char *fcb_atlas_open_cancelable(uint64_t h, const char *root, uint64_t files, int32_t (*poll)(void *), void *ctx) {
    assert(h == handle && !strcmp(root, "/repo") && files == 4096); ++calls; poll_now(poll, ctx); return result();
}
char *fcb_atlas_index_begin(uint64_t h, uint64_t g, uint64_t files, uint64_t per_file, uint64_t bytes, uint64_t grams) {
    assert(h == handle && g == 1 && files == 4096 && per_file == 1024 * 1024 && bytes == 32 * 1024 * 1024 && grams == 2 * 1024 * 1024);
    ++calls; return result();
}
char *fcb_atlas_index_step_cancelable(uint64_t h, uint64_t g, int32_t (*poll)(void *), void *ctx) {
    assert(h == handle && g == 1); ++calls; poll_now(poll, ctx); return result();
}
char *fcb_atlas_search_indexed_begin(uint64_t h, uint64_t g, uint64_t index, const char *needle, uint64_t hits, uint64_t bytes) {
    assert(h == handle && g == 44 && index == 1 && !strcmp(needle, "exact text") && hits == 1000 && bytes == 32 * 1024 * 1024);
    ++calls; return result();
}
char *fcb_atlas_search_step_cancelable(uint64_t h, uint64_t g, int32_t (*poll)(void *), void *ctx) {
    assert(h == handle && g == 44); ++calls; poll_now(poll, ctx); return result();
}
char *fcb_atlas_search_page(uint64_t h, uint64_t g, uint64_t start, uint64_t limit) {
    assert(h == handle && g == 44 && start == 64 && limit == 128); ++calls; return result();
}
char *fcb_atlas_index_open_reader(uint64_t h, uint64_t reader, uint64_t index, uint64_t file, uint64_t revision) {
    assert(h == handle && reader == UINT64_C(9007199254740999) && index == 1 && file == 321 && revision == 1);
    ++calls; return result();
}
uint8_t fcb_atlas_close(uint64_t h) { assert(h == handle); ++calls; return 1; }
void fcb_free_string(char *p) { if (p) { ++freed; free(p); } }
