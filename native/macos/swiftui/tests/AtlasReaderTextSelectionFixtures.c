/* Real Swift pointer marshaling against the public C declaration. Deliberately
 * out-of-range coordinates prove full-width ABI transport, not Rust admission.
 * These replies are transport fixtures, not source/capture verification. */
#include "../../../../crates/fcb-bridge/include/fcb_reader_sessions.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>

static uint64_t calls, allocated, freed;
static int32_t mode;
static char *pending;
void selection_fixture_reset(void) {
    assert(pending == NULL);
    calls = allocated = freed = 0;
    mode = 0;
}
void selection_fixture_mode(int32_t value) {
    assert(value >= 0 && value <= 5 && pending == NULL);
    mode = value;
}
uint64_t selection_fixture_calls(void) { return calls; }
uint64_t selection_fixture_allocated(void) { return allocated; }
uint64_t selection_fixture_freed(void) { return freed; }
uint64_t selection_fixture_live(void) { return allocated - freed; }

char *fcb_reader_resolve_selection(uint64_t handle, uint64_t window_start,
    uint64_t window_end, uint64_t utf8_start, uint64_t utf8_end) {
    assert(handle == UINT64_MAX - 10);
    assert(window_start == UINT64_MAX - 9 && window_end == UINT64_MAX - 8);
    assert(utf8_start == UINT64_MAX - 7 && utf8_end == UINT64_MAX);
    assert(pending == NULL);
    ++calls;
    if (mode == 1) return NULL;
    if (mode == 0) {
        const char *reply = "{\"fixture\":\"e\xcc\x81 \xf0\x9f\xa6\x80\\r\\n\\u0000\"}";
        size_t length = strlen(reply);
        pending = malloc(length + 1);
        assert(pending);
        memcpy(pending, reply, length + 1);
    } else if (mode == 3) {
        pending = malloc(3);
        assert(pending);
        pending[0] = (char)0xff; pending[1] = 'x'; pending[2] = 0;
    } else {
        const size_t limit = 4 * 1024 * 1024;
        size_t length = mode == 2 ? 0 : (mode == 4 ? limit + 1 : limit);
        pending = malloc(length + 1);
        assert(pending);
        memset(pending, 'x', length);
        pending[length] = 0;
    }
    ++allocated;
    return pending;
}
void fcb_free_string(char *pointer) {
    assert(pointer != NULL && pointer == pending && freed < allocated);
    ++freed;
    free(pointer);
    pending = NULL;
}
