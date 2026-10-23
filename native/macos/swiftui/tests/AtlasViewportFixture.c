#include <stdint.h>
#include <stdlib.h>

/* Link-only foreign symbols. These tests inject immutable capture packets into
 * the actual native worker; none of these stubs pretends to discover or parse. */
typedef int32_t (*poll_fn)(void *);
char *fcb_project_catalog_cancelable(const char *root, uint64_t limit, poll_fn poll, void *context) {
    (void)root; (void)limit; (void)poll; (void)context; return NULL;
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
void fcb_source_cache_free(uint8_t *bytes, uint64_t length) { (void)length; free(bytes); }
void fcb_free_string(char *value) { free(value); }
char *fcb_text_parcel_positions(const char *const *paths, const double *areas, uint64_t count, double aspect) {
    (void)paths; (void)areas; (void)count; (void)aspect; return NULL;
}
