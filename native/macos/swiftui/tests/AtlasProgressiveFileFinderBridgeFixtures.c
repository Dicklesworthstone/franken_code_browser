/* Fixed C symbols exercise the real Swift ABI marshaler, not the Rust engine. */
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define HANDLE UINT64_C(9007199254740993)
#define GENERATION UINT64_C(18446744073709551614)
#define REVISION UINT64_C(9007199254740995)
static unsigned allocations, frees;
static int bad;
void progressive_fixture_bad(int value) { bad = value; }
unsigned progressive_fixture_allocations(void) { return allocations; }
unsigned progressive_fixture_frees(void) { return frees; }
static char *answer(const char *text) {
    if (bad == 3) return NULL;
    char *result;
    if (bad == 1) {
        result = malloc(2); assert(result); result[0] = (char)255; result[1] = 0;
    } else if (bad == 2) {
        result = malloc(4 * 1024 * 1024 + 2); assert(result);
        memset(result, 'x', 4 * 1024 * 1024 + 1); result[4 * 1024 * 1024 + 1] = 0;
    } else { result = strdup(text); assert(result); }
    ++allocations; return result;
}
uint64_t fcb_file_finder_create(void) { return HANDLE; }
char *fcb_file_finder_open(uint64_t handle, const char *root, uint64_t generation,
                         const uint8_t *query, uint64_t query_bytes, uint32_t mode,
                         uint8_t match_case, uint64_t max_results) {
    static const uint8_t expected[] = {0xc3, 0xa9, '.', 'r', 's'};
    assert(handle == HANDLE && generation == GENERATION && !strcmp(root, "/repo/\xc3\xa9"));
    assert(query_bytes == sizeof(expected) && !memcmp(query, expected, sizeof(expected)));
    assert(mode <= 2 && match_case <= 1 && max_results == 256);
    if (mode == 0) return answer(match_case ? "exact-sensitive" : "exact-lowercase");
    if (mode == 1) return answer(match_case ? "prefix-sensitive" : "prefix-lowercase");
    return answer(match_case ? "fuzzy-sensitive" : "fuzzy-lowercase");
}
char *fcb_file_finder_info(uint64_t handle) { assert(handle == HANDLE); return answer("info"); }
char *fcb_file_finder_page(uint64_t handle, uint64_t generation, uint64_t revision) {
    assert(handle == HANDLE && generation == GENERATION && revision == REVISION); return answer("page");
}
char *fcb_file_finder_advance(uint64_t handle, uint64_t generation, uint64_t after, uint64_t batches) {
    assert(handle == HANDLE && generation == GENERATION && after == REVISION && batches == 16); return answer("advance");
}
char *fcb_file_finder_stop(uint64_t handle, uint64_t generation) {
    assert(handle == HANDLE && generation == GENERATION); return answer("stop");
}
char *fcb_file_finder_open_reader(uint64_t handle, uint64_t reader, uint64_t generation,
                                uint64_t revision, uint64_t file, uint64_t max_source_bytes) {
    assert(handle == HANDLE && reader == 999 && generation == GENERATION && revision == REVISION);
    assert(file == 20001 && max_source_bytes == 4194304); return answer("reader");
}
uint8_t fcb_file_finder_cancel(uint64_t handle) { assert(handle == HANDLE); return 1; }
uint8_t fcb_file_finder_close(uint64_t handle) { assert(handle == HANDLE); return 1; }
void fcb_free_string(char *value) { if (value) { ++frees; free(value); } }
