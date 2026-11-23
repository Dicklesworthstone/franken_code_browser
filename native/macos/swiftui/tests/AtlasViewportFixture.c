#include <stdint.h>
#include <stdlib.h>
#include <stdbool.h>

/* Link-only foreign symbols. These tests inject immutable capture packets into
 * the actual native worker; none of these stubs pretends to discover or parse. */
typedef int32_t (*poll_fn)(void *);
char *fcb_project_catalog_with_policy_cancelable(const char *root, uint64_t limit, uint64_t policy,
        poll_fn poll, void *context) {
    (void)root; (void)limit; (void)policy; (void)poll; (void)context; return NULL;
}

// The worker now links the directory route. Viewport-source fixtures must not
// silently accept metadata discovery as a source read.
#include <stdint.h>
#include <assert.h>
char *fcb_project_directory_catalog_cancelable(const char *root, const uint8_t *directory,
        uint64_t directory_len, uint64_t max_files, uint64_t policy, int32_t (*poll)(void *), void *context) {
    (void)root; (void)directory; (void)directory_len; (void)max_files; (void)policy; (void)poll; (void)context;
    assert(0 && "directory catalog used during viewport source test");
    return 0;
}

char *fcb_atlas_layout_cancelable(const char *root, poll_fn poll, void *context) {
    (void)root; (void)poll; (void)context; return NULL;
}
char *fcb_source_document_cancelable(const char *path, poll_fn poll, void *context) {
    (void)path; (void)poll; (void)context; return NULL;
}
char *fcb_source_document_cached_cancelable(uint64_t handle, const char *path, uint8_t *key, poll_fn poll, void *context) {
    (void)handle; (void)path; (void)key; (void)poll; (void)context; return NULL;
}
uint8_t *fcb_source_cache_get(uint64_t handle, const char *key, uint64_t *length) {
    (void)handle; (void)key; *length = 0; return NULL;
}
uint64_t fcb_source_cache_open(const char *root) {
    (void)root; assert(0 && "live cache opened during viewport artifact fixture"); return 0;
}
bool fcb_source_cache_close(uint64_t handle) { (void)handle; return false; }
char *fcb_source_document_cached(uint64_t handle, const char *path, uint8_t *key) {
    (void)handle; (void)path; (void)key; assert(0 && "legacy source capture used during viewport fixture"); return NULL;
}
bool fcb_source_cache_put(uint64_t handle, const char *key, const uint8_t *bytes, uint64_t length) {
    (void)handle; (void)key; (void)bytes; (void)length;
    assert(0 && "live artifact write used during viewport fixture"); return false;
}
bool fcb_source_cache_repair(uint64_t handle, const char *key, const uint8_t *bytes, uint64_t length) {
    (void)handle; (void)key; (void)bytes; (void)length;
    assert(0 && "live artifact repair used during viewport fixture"); return false;
}
char *fcb_source_cache_stats(uint64_t handle) { (void)handle; return NULL; }
void fcb_source_cache_free(uint8_t *bytes, uint64_t length) { (void)length; free(bytes); }
void fcb_free_string(char *value) { free(value); }
char *fcb_text_parcel_positions(const char *const *paths, const double *areas, uint64_t count, double aspect) {
    (void)paths; (void)areas; (void)count; (void)aspect; return NULL;
}
