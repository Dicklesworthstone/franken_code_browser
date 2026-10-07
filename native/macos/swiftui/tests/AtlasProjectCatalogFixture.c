// Fixed ABI fixture: no Rust discovery, filesystem scan, or source parser.
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <assert.h>
static char *response;
static unsigned calls, allocated, freed;
static int cancel_during_call;
extern void project_fixture_cancel(void *);
void project_fixture_set(const char *json, int cancel) {
    free(response); response = json ? strdup(json) : NULL;
    calls = allocated = freed = 0; cancel_during_call = cancel;
}
unsigned project_fixture_count(unsigned kind) { return kind == 0 ? calls : (kind == 1 ? allocated : freed); }
char *fcb_project_catalog_cancelable(const char *root, uint64_t limit, int32_t (*poll)(void *), void *context) {
    ++calls;
    assert(strcmp(root, "/project") == 0 && limit == 20000 && poll && context);
    assert(poll(context) == 0);
    if (cancel_during_call) { project_fixture_cancel(context); assert(poll(context) != 0); }
    if (!response) return NULL;
    ++allocated; return strdup(response);
}
void fcb_free_string(char *p) { if (p) { ++freed; free(p); } }
// Referenced by the unchanged compatibility/source operations. Calling any
// of them from catalogReport is a regression, not permitted fixture behavior.
char *fcb_atlas_layout_cancelable(const char *r, int32_t (*p)(void *), void *c) {
    (void)r; (void)p; (void)c; assert(0 && "legacy catalog unexpectedly used"); return NULL;
}
char *fcb_source_document_cancelable(const char *r, int32_t (*p)(void *), void *c) {
    (void)r; (void)p; (void)c; assert(0 && "source read during metadata open"); return NULL;
}
char *fcb_source_document_cached_cancelable(uint64_t h, const char *r, uint8_t *k, int32_t (*p)(void *), void *c) {
    (void)h; (void)r; (void)k; (void)p; (void)c; assert(0 && "source cache used during metadata open"); return NULL;
}
uint8_t *fcb_source_cache_get(uint64_t h, const char *k, uint64_t *n) {
    (void)h; (void)k; (void)n; assert(0 && "artifact read during metadata open"); return NULL;
}
void fcb_source_cache_free(uint8_t *p, uint64_t n) { (void)p; (void)n; assert(0 && "artifact release during metadata open"); }
