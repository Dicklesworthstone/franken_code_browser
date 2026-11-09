/* Injected ABI/allocation fixture for the production Swift transport. This
 * does not open a file, decode source, or establish range-reader correctness. */
#include "../../../../crates/fcb-bridge/include/fcb_observed_reader.h"
#include <assert.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>

enum { CREATE, OPEN, INFO, WINDOW, RETAINED, COPY, CANCEL, CLOSE, POLLS, FIND, HIT, OBSERVE_HIT, COUNTERS };
static uint32_t counts[COUNTERS], allocations, frees, encoding_bits;
static int32_t mode;
static char *owned;
static const uint64_t handle_value = UINT64_C(0xfedcba9876543210);
static const uint64_t offset_value = UINT64_C(0x0020000100000047);
static const uint64_t observation_value = UINT64_C(0x8000000100000007);
extern void observed_reader_fixture_cancel(void *context);

void observed_reader_fixture_reset(void) {
    assert(owned == NULL && allocations == frees);
    memset(counts, 0, sizeof(counts)); allocations = frees = encoding_bits = 0; mode = 0;
}
void observed_reader_fixture_mode(int32_t value) { mode = value; }
uint32_t observed_reader_fixture_count(uint32_t kind) { assert(kind < COUNTERS); return counts[kind]; }
uint32_t observed_reader_fixture_allocations(void) { return allocations; }
uint32_t observed_reader_fixture_frees(void) { return frees; }
uint32_t observed_reader_fixture_encodings(void) { return encoding_bits; }

static void poll_context(fcb_observed_reader_poll poll, void *context) {
    assert(poll != NULL && context != NULL);
    ++counts[POLLS]; assert(poll(context) == 0);
    if (mode == 4 || mode == 5) {
        observed_reader_fixture_cancel(context);
        ++counts[POLLS]; assert(poll(context) != 0);
    }
}
static char *response(void) {
    assert(owned == NULL);
    if (mode == 1 || mode == 5) return NULL;
    if (mode == 2) {
        owned = malloc(2); assert(owned != NULL); owned[0] = (char)255; owned[1] = 0;
    } else if (mode == 3) {
        size_t length = 8 * 1024 * 1024 + 1;
        owned = malloc(length + 1); assert(owned != NULL);
        memset(owned, 'x', length); owned[length] = 0;
    } else {
        static const char value[] = "{\"bridge_fixture\":\"observed-range\"}";
        owned = malloc(sizeof(value)); assert(owned != NULL); memcpy(owned, value, sizeof(value));
    }
    ++allocations; return owned;
}

uint64_t fcb_observed_reader_create(void) { ++counts[CREATE]; return mode == 6 ? 0 : handle_value; }
char *fcb_observed_reader_open(uint64_t handle, const char *root, const char *relative_hex,
    fcb_observed_reader_poll poll, void *context) {
    assert(handle == handle_value && root != NULL && relative_hex != NULL);
    assert(strcmp(root, "/repo-\xc3\xa9") == 0);
    assert(strcmp(relative_hex, "7372632fff5c220a2e7273") == 0);
    ++counts[OPEN]; poll_context(poll, context);
    /* Both borrowed C strings must remain valid throughout callback execution. */
    assert(strcmp(root, "/repo-\xc3\xa9") == 0);
    assert(strcmp(relative_hex, "7372632fff5c220a2e7273") == 0);
    return response();
}
char *fcb_observed_reader_info(uint64_t handle) {
    assert(handle == handle_value); ++counts[INFO]; return response();
}
char *fcb_observed_reader_window(uint64_t handle, uint64_t offset, uint64_t bytes,
    uint64_t encoding, fcb_observed_reader_poll poll, void *context) {
    assert(handle == handle_value && offset == offset_value && bytes == 65536);
    assert(encoding >= 1 && encoding <= 3);
    encoding_bits |= 1u << (unsigned)encoding;
    ++counts[WINDOW]; poll_context(poll, context); return response();
}
char *fcb_observed_reader_retained(uint64_t handle, uint64_t observation) {
    assert(handle == handle_value && observation == observation_value);
    ++counts[RETAINED]; return response();
}
char *fcb_observed_reader_copy(uint64_t handle, uint64_t observation, uint64_t start, uint64_t end) {
    assert(handle == handle_value && observation == observation_value);
    assert(start == offset_value + 3 && end == offset_value + 17);
    ++counts[COPY]; return response();
}
char *fcb_observed_reader_find(uint64_t handle, uint64_t generation, const char *needle,
    uint64_t encoding, uint64_t max_matches, uint64_t max_scan_bytes,
    fcb_observed_reader_poll poll, void *context) {
    assert(handle == handle_value && generation == observation_value);
    assert(needle != NULL && strcmp(needle, "exact \xc3\xa9\xf0\x9f\x98\x80") == 0);
    assert(encoding >= 1 && encoding <= 3);
    assert(max_matches == 256 && max_scan_bytes == UINT64_C(4294967296));
    ++counts[FIND]; poll_context(poll, context);
    assert(strcmp(needle, "exact \xc3\xa9\xf0\x9f\x98\x80") == 0);
    return response();
}
char *fcb_observed_reader_search_hit(uint64_t handle, uint64_t generation, uint64_t index) {
    assert(handle == handle_value && generation == observation_value && index == 255);
    ++counts[HIT]; return response();
}
char *fcb_observed_reader_observe_hit(uint64_t handle, uint64_t generation, uint64_t index,
    uint64_t context_bytes, uint64_t window_bytes, fcb_observed_reader_poll poll, void *context) {
    assert(handle == handle_value && generation == observation_value && index == 255);
    assert(context_bytes == 1024 && window_bytes == 65536);
    ++counts[OBSERVE_HIT]; poll_context(poll, context); return response();
}
uint8_t fcb_observed_reader_cancel(uint64_t handle) {
    assert(handle == handle_value); ++counts[CANCEL]; return mode == 6 ? 0 : 1;
}
uint8_t fcb_observed_reader_close(uint64_t handle) {
    assert(handle == handle_value); ++counts[CLOSE]; return mode == 6 ? 0 : 1;
}
void fcb_free_string(char *pointer) {
    assert(pointer != NULL && pointer == owned && allocations == frees + 1);
    owned = NULL; ++frees; free(pointer);
}
