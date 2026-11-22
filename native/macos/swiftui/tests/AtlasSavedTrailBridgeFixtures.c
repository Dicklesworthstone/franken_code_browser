#include "fcb_saved_repository.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>

/* Header-checked marshaling and owned-response fixture. Real FCBT/archive
 * resolution and payload verification remain in the Rust service fixtures. */
static uint64_t calls[9];
static int result_mode;
void fixture_saved_trail_mode(int mode) { result_mode = mode; }
uint64_t fixture_saved_trail_count(uint64_t index) { assert(index < 9); return calls[index]; }
static char *reply(void) {
    if (result_mode == 1) return NULL;
    if (result_mode == 2) {
        char *p = malloc(2); assert(p); p[0] = (char)0xff; p[1] = 0; return p;
    }
    size_t length = result_mode == 3 ? 8 * 1024 * 1024 + 1 : (result_mode == 4 ? 0 : 2);
    char *p = malloc(length + 1); assert(p); memset(p, 'x', length); p[length] = 0;
    if (length == 2) { p[0] = '{'; p[1] = '}'; }
    return p;
}
void fcb_free_string(char *pointer) { assert(pointer); calls[4]++; free(pointer); }
char *fcb_saved_trail_attach(uint64_t handle, uint64_t generation, const char *path) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9);
    assert(path && strcmp(path, "/trail-e\xcc\x81.fcbt") == 0);
    calls[0]++; return reply();
}
char *fcb_saved_trail_page(uint64_t handle, uint64_t generation, uint64_t start, uint64_t limit) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9 && start == 1000 && limit == 24);
    calls[1]++; return reply();
}
char *fcb_saved_trail_detach(uint64_t handle, uint64_t generation) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9);
    calls[2]++; return reply();
}
char *fcb_saved_open_trail_reader(uint64_t handle, uint64_t reader, uint64_t generation, uint64_t item) {
    assert(handle == UINT64_MAX - 10 && reader == UINT64_MAX - 8 && generation == UINT64_MAX - 9 && item == 1023);
    calls[3]++; return reply();
}
char *fcb_saved_trail_write(uint64_t handle, uint64_t operation,
    uint64_t parent_generation, const char *parent_digest, uint64_t member,
    uint64_t start, uint64_t end, const uint8_t *title, uint64_t title_length,
    const uint8_t *rationale, uint64_t rationale_length, const char *destination) {
    assert(handle == UINT64_MAX - 10 && member == 1);
    assert(destination && strcmp(destination, "/write-e\xcc\x81.fcbt") == 0);
    assert(parent_digest);
    if (operation == UINT64_MAX - 1) {
        assert(parent_generation == UINT64_MAX - 9 && strlen(parent_digest) == 64);
        for (size_t i = 0; i < 64; i++) assert(parent_digest[i] == 'a');
        assert(start == 209 && end == 209);
    } else {
        assert((operation == UINT64_MAX || operation == UINT64_MAX - 2) && parent_generation == 0);
        assert(parent_digest[0] == 0 && start == 202 && end == 208);
    }
    if (operation == UINT64_MAX - 2) {
        assert(title_length == 0 && rationale_length == 0);
    } else {
        static const uint8_t expected_title[] = {'e', 0xcc, 0x81, 0, 'T'};
        static const uint8_t expected_rationale[] = {'w', 'h', 'y', 0, 0xc3, 0xa9, '\n'};
        assert(title && title_length == sizeof expected_title && memcmp(title, expected_title, sizeof expected_title) == 0);
        assert(rationale && rationale_length == sizeof expected_rationale && memcmp(rationale, expected_rationale, sizeof expected_rationale) == 0);
    }
    calls[5]++; return reply();
}
char *fcb_saved_trail_write_receipt(uint64_t handle, uint64_t operation) {
    assert(handle == UINT64_MAX - 10 && operation == UINT64_MAX);
    calls[6]++; return reply();
}
char *fcb_saved_trail_export(uint64_t handle, uint64_t operation, uint64_t generation,
    const char *digest, const char *destination, uint64_t entries, uint64_t verification_bytes,
    uint64_t selected_bytes, uint64_t output_bytes, uint64_t output_scalars) {
    assert(handle == UINT64_MAX - 10 && operation == UINT64_MAX && generation == UINT64_MAX - 9);
    assert(digest && strlen(digest) == 64);
    for (size_t i = 0; i < 64; i++) assert(digest[i] == 'a');
    assert(destination && strcmp(destination, "/pack-e\xcc\x81.json") == 0);
    assert(entries == 1024 && verification_bytes == 64 * 1024 * 1024);
    assert(selected_bytes == 4 * 1024 * 1024 && output_bytes == 16 * 1024 * 1024 && output_scalars == output_bytes);
    calls[7]++; return reply();
}
char *fcb_saved_trail_export_receipt(uint64_t handle, uint64_t operation) {
    assert(handle == UINT64_MAX - 10 && operation == UINT64_MAX);
    calls[8]++; return reply();
}
