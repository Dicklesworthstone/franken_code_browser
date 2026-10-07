/* Fixed C symbols for the production Swift marshaler, not the Rust engine. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <assert.h>

#define HANDLE UINT64_C(9007199254740993)
#define GENERATION UINT64_C(18446744073709551614)
static unsigned counts[8]; /* create, open, find, page, select, close, alloc, free */
static int bad;
unsigned file_fixture_count(unsigned key) { assert(key < 8); return counts[key]; }
void file_fixture_bad(int kind) { bad = kind; }
static char *answer(const char *text) {
    if (bad == 3) return NULL;
    char *result;
    if (bad == 1) { result = malloc(2); assert(result); result[0] = (char)255; result[1] = 0; }
    else if (bad == 2) {
        result = malloc(4 * 1024 * 1024 + 2); assert(result);
        memset(result, 'x', 4 * 1024 * 1024 + 1); result[4 * 1024 * 1024 + 1] = 0;
    } else { result = strdup(text); assert(result); }
    ++counts[6]; return result;
}
uint64_t fcb_atlas_create(void) { ++counts[0]; return HANDLE; }
char *fcb_atlas_open_cancelable(uint64_t handle, const char *root, uint64_t limit,
                              int32_t (*poll)(void *), void *context) {
    assert(handle == HANDLE && !strcmp(root, "/repo/\xc3\xa9") && limit == 20000 && poll && context);
    ++counts[1]; if (poll(context)) return NULL; return answer("opened");
}
char *fcb_atlas_find_files_cancelable(uint64_t handle, uint64_t generation, const char *query,
                                    uint64_t limit, uint8_t mode, uint8_t case_mode,
                                    int32_t (*poll)(void *), void *context) {
    assert(handle == HANDLE && generation == GENERATION && !strcmp(query, "\xc3\xa9.rs"));
    assert(limit == 256 && mode <= 2 && case_mode <= 1 && poll && context);
    ++counts[2]; if (poll(context)) return NULL;
    return answer(mode == 0 ? "fuzzy" : mode == 1 ? "exact" : "prefix");
}
char *fcb_atlas_file_results(uint64_t handle, uint64_t generation, uint64_t start, uint64_t limit) {
    assert(handle == HANDLE && generation == GENERATION && start == 64 && limit == 128);
    ++counts[3]; return answer("page");
}
char *fcb_atlas_file_select(uint64_t handle, uint64_t generation, uint64_t file) {
    assert(handle == HANDLE && generation == GENERATION && file == 255);
    ++counts[4]; return answer("selected");
}
uint8_t fcb_atlas_close(uint64_t handle) { assert(handle == HANDLE); ++counts[5]; return 1; }
void fcb_free_string(char *value) { if (value) { ++counts[7]; free(value); } }
