// ABI/ownership fixture only. No source discovery or structural extraction.
#include "../../../../crates/fcb-bridge/include/fcb_catalog_structure.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>

enum { RESPONSE_BYTES = 8 * 1024 * 1024, FIELD_COUNT = 5, ARGUMENT_COUNT = 16 };
static uint64_t calls[8], arguments[ARGUMENT_COUNT], allocated, freed;
static unsigned char *fields[FIELD_COUNT];
static uint64_t field_lengths[FIELD_COUNT];
static int field_nulls[FIELD_COUNT];
static int response_mode;
static uint8_t status_byte = 1;
static char *live_reply;

void structure_fixture_reset(void) {
    assert(live_reply == NULL);
    for (unsigned i = 0; i < FIELD_COUNT; ++i) {
        free(fields[i]); fields[i] = NULL; field_lengths[i] = 0; field_nulls[i] = 1;
    }
    memset(calls, 0, sizeof calls);
    memset(arguments, 0, sizeof arguments);
    allocated = freed = 0; response_mode = 0; status_byte = 1;
}
void structure_fixture_mode(int32_t mode) {
    assert(live_reply == NULL && mode >= 0 && mode <= 6);
    response_mode = mode;
}
void structure_fixture_status(uint8_t value) { status_byte = value; }
uint64_t structure_fixture_calls(uint32_t kind) {
    assert(kind < 8); return calls[kind];
}
uint64_t structure_fixture_argument(uint32_t index) {
    assert(index < ARGUMENT_COUNT); return arguments[index];
}
uint64_t structure_fixture_allocations(void) { return allocated; }
uint64_t structure_fixture_frees(void) { return freed; }
uint64_t structure_fixture_field_length(uint32_t index) {
    assert(index < FIELD_COUNT); return field_lengths[index];
}
uint8_t structure_fixture_field_is_null(uint32_t index) {
    assert(index < FIELD_COUNT); return (uint8_t)field_nulls[index];
}
uint8_t structure_fixture_field_byte(uint32_t index, uint64_t offset) {
    assert(index < FIELD_COUNT && offset < field_lengths[index]);
    return fields[index][offset];
}
static void capture_bytes(unsigned index, const uint8_t *bytes, size_t length) {
    assert(index < FIELD_COUNT);
    free(fields[index]); fields[index] = NULL;
    field_nulls[index] = bytes == NULL; field_lengths[index] = length;
    if (bytes) {
        fields[index] = malloc(length + 1); assert(fields[index]);
        memcpy(fields[index], bytes, length); fields[index][length] = 0;
    } else { assert(length == 0); }
}
static void capture_text(unsigned index, const char *text, size_t limit) {
    size_t length = 0;
    if (text) {
        while (text[length]) { assert(length < limit); ++length; }
    }
    capture_bytes(index, (const uint8_t *)text, length);
}
static void begin_call(unsigned kind) {
    assert(kind < 8 && live_reply == NULL); ++calls[kind];
    for (unsigned i = 0; i < ARGUMENT_COUNT; ++i) { arguments[i] = UINT64_MAX; }
}
static char *response(void) {
    assert(live_reply == NULL);
    if (response_mode == 1) { return NULL; }
    size_t length;
    switch (response_mode) {
        case 2: length = 0; break;
        case 3: length = 2; break;
        case 4: case 5: length = RESPONSE_BYTES + 1; break;
        case 6: length = RESPONSE_BYTES; break;
        default: length = strlen("{\"fixture\":\"structure-\xce\xbb\"}"); break;
    }
    // Mode 5 is readable for exactly the bounded strnlen probe, with no NUL.
    live_reply = malloc(length + (response_mode == 5 ? 0 : 1));
    assert(live_reply); ++allocated;
    if (response_mode == 0) {
        memcpy(live_reply, "{\"fixture\":\"structure-\xce\xbb\"}", length);
    } else if (response_mode == 3) {
        live_reply[0] = (char)0xff; live_reply[1] = (char)0xfe;
    } else if (length != 0) { memset(live_reply, 'x', length); }
    if (response_mode != 5) { live_reply[length] = 0; }
    return live_reply;
}

uint64_t fcb_catalog_structure_create(void) {
    begin_call(0); return UINT64_C(0xfedcba9876543210);
}
char *fcb_catalog_structure_open_matching_with_policy(uint64_t handle, const char *root,
        uint64_t generation, const uint8_t *query, uint64_t query_bytes,
        uint64_t mode, uint64_t name_mode, uint64_t start_after,
        const char *extensions, const char *filenames, uint64_t path_kind,
        const char *path, uint64_t max_matches, uint64_t max_scan_bytes,
        uint64_t max_read_calls, uint64_t case_sensitive, uint64_t discovery_policy) {
    begin_call(1);
    assert(query && query_bytes > 0 && query_bytes <= 1024);
    uint64_t values[] = { handle, generation, query_bytes, mode, name_mode, start_after,
        path_kind, max_matches, max_scan_bytes, max_read_calls, case_sensitive, discovery_policy };
    memcpy(arguments, values, sizeof values);
    capture_text(0, root, 16 * 1024);
    capture_bytes(1, query, (size_t)query_bytes);
    capture_text(2, extensions, 1024); capture_text(3, filenames, 1024);
    capture_text(4, path, 1024 * 1024);
    return response();
}
char *fcb_catalog_structure_advance(uint64_t handle, uint64_t generation,
        uint64_t revision, uint64_t steps) {
    begin_call(2);
    arguments[0] = handle; arguments[1] = generation; arguments[2] = revision; arguments[3] = steps;
    return response();
}
char *fcb_catalog_structure_page(uint64_t handle, uint64_t generation,
        uint64_t revision, uint64_t offset, uint64_t count) {
    begin_call(3);
    arguments[0] = handle; arguments[1] = generation; arguments[2] = revision;
    arguments[3] = offset; arguments[4] = count;
    return response();
}
char *fcb_catalog_structure_stop(uint64_t handle, uint64_t generation) {
    begin_call(4); arguments[0] = handle; arguments[1] = generation; return response();
}
char *fcb_catalog_structure_open_hit_reader(uint64_t handle, uint64_t reader,
        uint64_t generation, uint64_t revision, uint64_t index) {
    begin_call(5);
    arguments[0] = handle; arguments[1] = reader; arguments[2] = generation;
    arguments[3] = revision; arguments[4] = index;
    return response();
}
uint8_t fcb_catalog_structure_cancel(uint64_t handle) {
    begin_call(6); arguments[0] = handle; return status_byte;
}
uint8_t fcb_catalog_structure_close(uint64_t handle) {
    begin_call(7); arguments[0] = handle; return status_byte;
}
void fcb_free_string(char *pointer) {
    assert(pointer && pointer == live_reply);
    ++freed; free(pointer); live_reply = NULL;
}
