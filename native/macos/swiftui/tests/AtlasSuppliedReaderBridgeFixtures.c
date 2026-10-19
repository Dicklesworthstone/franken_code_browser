/* Pointer/length/allocation fixture for the actual Swift reader bridge.
 * This is not a substitute for the Rust supplied-reader integration tests. */
#include "../../../../crates/fcb-bridge/include/fcb_reader_supplied.h"
#include "../../../../crates/fcb-bridge/include/fcb_reader_comparison.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>

static const uint64_t fixture_handle = UINT64_C(18446744073709551500);
static const uint8_t original[] = {0xef, 0xbb, 0xbf, 'a', 'b', 'c', 0, 'e', 0xcc, 0x81, '\r', '\n'};
static uint32_t calls, allocated, freed;
static int32_t mode;
void supplied_fixture_reset(void) { calls = allocated = freed = 0; mode = 0; }
void supplied_fixture_mode(int32_t value) { mode = value; }
uint32_t supplied_fixture_calls(void) { return calls; }
uint32_t supplied_fixture_live(void) { return allocated - freed; }
uint64_t fcb_reader_create(void) { return fixture_handle; }
char *fcb_reader_open_bytes(uint64_t handle, const char *label, const uint8_t *bytes, uint64_t length) {
    assert(handle == fixture_handle && label && !strcmp(label, "/project/e\xcc\x81.md"));
    if (mode == 1) { assert(length == 0); }
    else { assert(bytes && length == sizeof(original) && !memcmp(bytes, original, sizeof(original))); }
    ++calls;
    if (mode == 2) return NULL;
    char *result = strdup("{\"fixture\":true}");
    assert(result); ++allocated; return result;
}
char *fcb_reader_open(uint64_t handle, const char *path, uint64_t length) {
    (void)handle; (void)path; (void)length;
    assert(0 && "supplied reader must not live-open"); return NULL;
}
char *fcb_reader_window(uint64_t handle, uint64_t offset, uint64_t length) {
    (void)handle; (void)offset; (void)length; return NULL;
}
char *fcb_reader_lines(uint64_t handle, uint64_t first, uint64_t count, uint64_t length) {
    (void)handle; (void)first; (void)count; (void)length; return NULL;
}
char *fcb_reader_fork(uint64_t source, uint64_t destination) {
    (void)source; (void)destination; return NULL;
}
char *fcb_reader_compare(uint64_t before, uint64_t after, uint64_t generation,
    uint64_t edits, uint64_t work) {
    assert(before == fixture_handle && after == fixture_handle - 1);
    assert(generation == UINT64_MAX && edits == 512 && work == UINT64_C(67108864));
    ++calls;
    char *result = strdup("{\"comparison_fixture\":true}");
    assert(result); ++allocated; return result;
}
uint8_t fcb_reader_cancel(uint64_t handle) { assert(handle == fixture_handle); return 1; }
uint8_t fcb_reader_close(uint64_t handle) { assert(handle == fixture_handle); return 1; }
void fcb_free_string(char *pointer) { if (pointer) { ++freed; free(pointer); } }
