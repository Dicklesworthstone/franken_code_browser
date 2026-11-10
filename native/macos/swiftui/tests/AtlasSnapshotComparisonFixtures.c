#include "fcb_snapshot_comparison.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>

static uint64_t calls[10];
static int result_mode;
void fixture_snapshot_comparison_mode(int mode) { result_mode = mode; }
uint64_t fixture_snapshot_comparison_count(uint64_t index) { assert(index < 10); return calls[index]; }
static char *reply(void) {
    if (result_mode == 1) return NULL;
    if (result_mode == 2) { char *p = malloc(3); assert(p); p[0] = (char)0xff; p[1] = 'x'; p[2] = 0; return p; }
    size_t length = result_mode == 3 ? 4 * 1024 * 1024 + 1 : (result_mode == 4 ? 0 : 2);
    char *p = malloc(length + 1); assert(p);
    memset(p, 'x', length); p[length] = 0;
    if (length == 2) { p[0] = '{'; p[1] = '}'; }
    return p;
}
void fcb_free_string(char *pointer) { assert(pointer); calls[9]++; free(pointer); }
uint64_t fcb_snapshot_comparison_create(void) { calls[0]++; return UINT64_MAX - 10; }
char *fcb_snapshot_comparison_open_cancelable(uint64_t handle, const char *before, const char *after,
    int32_t (*poll)(void *), void *context) {
    assert(handle == UINT64_MAX - 10 && before && after && poll && context);
    assert(strcmp(before, "/before-e\xcc\x81.fcbs") == 0 && strcmp(after, "/after-\xc3\xa9.fcbs") == 0);
    assert(poll(context) == (result_mode == 5 ? 1 : 0)); calls[1]++; return reply();
}
char *fcb_snapshot_comparison_info(uint64_t handle) {
    assert(handle == UINT64_MAX - 10); calls[2]++; return reply();
}
char *fcb_snapshot_comparison_begin_cancelable(uint64_t handle, uint64_t generation,
    int32_t (*poll)(void *), void *context) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9 && poll && context);
    assert(poll(context) == (result_mode == 5 ? 1 : 0)); calls[3]++; return reply();
}
char *fcb_snapshot_comparison_step_cancelable(uint64_t handle, uint64_t generation, uint64_t maximum,
    int32_t (*poll)(void *), void *context) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9 && maximum == 256 && poll && context);
    assert(poll(context) == (result_mode == 5 ? 1 : 0)); calls[4]++; return reply();
}
char *fcb_snapshot_comparison_page(uint64_t handle, uint64_t generation, uint64_t filter, uint64_t start, uint64_t limit) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9);
    assert(filter < 9 && start == UINT64_MAX - 7 && limit == 64); calls[5]++; return reply();
}
char *fcb_snapshot_comparison_open_reader(uint64_t handle, uint64_t reader, uint64_t generation, uint64_t entry, uint64_t side) {
    assert(handle == UINT64_MAX - 10 && reader == UINT64_MAX - 6 && generation == UINT64_MAX - 9);
    assert(entry == UINT64_MAX - 5 && side <= 1); calls[6]++; return reply();
}
uint8_t fcb_snapshot_comparison_cancel(uint64_t handle) {
    assert(handle == UINT64_MAX - 10); calls[7]++; return 1;
}
uint8_t fcb_snapshot_comparison_close(uint64_t handle) {
    assert(handle == UINT64_MAX - 10); calls[8]++; return 1;
}
