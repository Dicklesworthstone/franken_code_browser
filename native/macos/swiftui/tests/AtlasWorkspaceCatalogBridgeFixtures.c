/* Native scheduling/ownership fixture only: no filesystem or matcher work. */
#include "../../../../crates/fcb-bridge/include/fcb_catalog_stream.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>

enum { OPEN, ADVANCE, PAGE, HIT, STOP, CANCEL, CLOSE, COUNTERS };
static const uint64_t owner_value = UINT64_C(0xfedcba9876543210);
static const uint64_t generation_value = UINT64_C(0x8000000100000007);
static char *receipts[5], *expected_query, *owned;
static uint64_t expected_mode, expected_case, expected_count, expected_start, expected_scope;
static uint64_t current_revision, expected_policy;
static uint32_t counts[COUNTERS], allocations, frees;
static int32_t response_mode;

void catalog_stream_fixture_reset(void) {
    assert(owned == NULL && allocations == frees);
    for (unsigned i = 0; i < 5; ++i) { free(receipts[i]); receipts[i] = NULL; }
    free(expected_query); expected_query = NULL;
    memset(counts, 0, sizeof(counts)); allocations = frees = 0; response_mode = 0; expected_policy = 0;
}
void catalog_stream_fixture_receipt(uint32_t kind, const char *text) {
    assert(kind < 5 && text != NULL); free(receipts[kind]); receipts[kind] = strdup(text);
    assert(receipts[kind] != NULL);
}
void catalog_stream_fixture_expected(const char *query, uint64_t mode, uint64_t sensitive,
    uint64_t counted, uint64_t start, uint64_t scope) {
    free(expected_query); expected_query = strdup(query); assert(expected_query != NULL);
    expected_mode = mode; expected_case = sensitive; expected_count = counted;
    expected_start = start; expected_scope = scope;
}
void catalog_stream_fixture_policy(uint64_t policy) { expected_policy = policy; }
void catalog_stream_fixture_mode(int32_t mode) { response_mode = mode; }
uint32_t catalog_stream_fixture_count(uint32_t kind) { assert(kind < COUNTERS); return counts[kind]; }
uint32_t catalog_stream_fixture_allocations(void) { return allocations; }
uint32_t catalog_stream_fixture_frees(void) { return frees; }

static char *response(unsigned kind) {
    assert(kind < 5 && owned == NULL);
    if (response_mode == 1) return NULL;
    if (response_mode == 2) {
        owned = malloc(2); assert(owned != NULL); owned[0] = (char)255; owned[1] = 0;
    } else if (response_mode == 3) {
        size_t size = 8 * 1024 * 1024 + 1;
        owned = malloc(size + 1); assert(owned != NULL); memset(owned, 'x', size); owned[size] = 0;
    } else { assert(receipts[kind] != NULL); owned = strdup(receipts[kind]); assert(owned != NULL); }
    ++allocations; return owned;
}

uint64_t fcb_catalog_stream_create(void) { return owner_value; }
char *fcb_catalog_stream_open(uint64_t owner, const char *root, uint64_t generation,
    const uint8_t *query, uint64_t query_bytes, uint64_t mode, uint64_t sensitive,
    uint64_t counted, uint64_t start, const char *extensions, const char *filenames,
    uint64_t path_kind, const char *path, uint64_t matches, uint64_t bytes, uint64_t calls) {
    assert(owner == owner_value && generation == generation_value && root != NULL);
    assert(strcmp(root, "/repo-\xc3\xa9") == 0 && query != NULL && expected_query != NULL);
    assert(query_bytes == strlen(expected_query) && memcmp(query, expected_query, (size_t)query_bytes) == 0);
    assert(mode == expected_mode && sensitive == expected_case && counted == expected_count && start == expected_start);
    assert(matches == 256 && bytes == UINT64_C(4294967296) && calls == 1048576);
    if (expected_scope == 0) {
        assert(extensions == NULL && filenames == NULL && path_kind == 0 && (path == NULL || !*path));
    }
    else if (expected_scope == 1) {
        assert(extensions != NULL && filenames != NULL && !*extensions && !*filenames && path_kind == 0);
    } else {
        assert(extensions != NULL && filenames != NULL && strcmp(extensions, "rs") == 0 && strcmp(filenames, "makefile") == 0);
        assert(path != NULL && path_kind == expected_scope - 1);
        assert(strcmp(path, path_kind == 1 ? "src" : "src/main.rs") == 0);
    }
    current_revision = 1; ++counts[OPEN]; return response(OPEN);
}
char *fcb_catalog_stream_open_with_policy(uint64_t owner, const char *root, uint64_t generation,
    const uint8_t *query, uint64_t query_bytes, uint64_t mode, uint64_t sensitive,
    uint64_t counted, uint64_t start, const char *extensions, const char *filenames,
    uint64_t path_kind, const char *path, uint64_t matches, uint64_t bytes, uint64_t calls,
    uint64_t policy) {
    assert(policy == expected_policy && policy <= 1);
    return fcb_catalog_stream_open(owner, root, generation, query, query_bytes, mode, sensitive,
        counted, start, extensions, filenames, path_kind, path, matches, bytes, calls);
}
char *fcb_catalog_stream_advance(uint64_t owner, uint64_t generation, uint64_t revision, uint64_t steps) {
    assert(owner == owner_value && generation == generation_value && revision == 1 && steps == 16);
    current_revision = 2; ++counts[ADVANCE]; return response(ADVANCE);
}
char *fcb_catalog_stream_page(uint64_t owner, uint64_t generation, uint64_t revision, uint64_t start, uint64_t count) {
    assert(owner == owner_value && generation == generation_value && revision == current_revision && start == 0 && count == 32);
    ++counts[PAGE]; return response(PAGE);
}
char *fcb_catalog_stream_hit(uint64_t owner, uint64_t generation, uint64_t revision, uint64_t position) {
    assert(owner == owner_value && generation == generation_value && revision == 2 && position == 1);
    ++counts[HIT]; return response(HIT);
}
char *fcb_catalog_stream_stop(uint64_t owner, uint64_t generation) {
    assert(owner == owner_value && generation == generation_value);
    current_revision = 2; ++counts[STOP]; return response(STOP);
}
uint8_t fcb_catalog_stream_cancel(uint64_t owner) { assert(owner == owner_value); ++counts[CANCEL]; return 1; }
uint8_t fcb_catalog_stream_close(uint64_t owner) { assert(owner == owner_value); ++counts[CLOSE]; return 1; }
void fcb_free_string(char *pointer) {
    assert(pointer != NULL && pointer == owned); ++frees; free(pointer); owned = NULL;
}
