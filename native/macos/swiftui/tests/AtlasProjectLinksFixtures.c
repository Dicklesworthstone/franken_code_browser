#include "fcb_atlas_links.h"
#include "fcb_atlas_imports.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>

static uint64_t calls[9];
static int result_mode;
void fixture_links_mode(int mode) { result_mode = mode; }
uint64_t fixture_links_count(uint64_t index) { assert(index < 9); return calls[index]; }
static char *reply(void) {
    if (result_mode == 1) return NULL;
    if (result_mode == 2) { char *p = malloc(3); assert(p); p[0] = (char)0xff; p[1] = 'x'; p[2] = 0; return p; }
    size_t length = result_mode == 3 ? 8 * 1024 * 1024 + 1 : (result_mode == 4 ? 0 : 2);
    char *p = malloc(length + 1); assert(p);
    memset(p, 'x', length); p[length] = 0;
    if (result_mode == 0) { p[0] = '{'; p[1] = '}'; }
    return p;
}
void fcb_free_string(char *pointer) { assert(pointer); calls[4]++; free(pointer); }
char *fcb_atlas_links_begin_cancelable(uint64_t handle, uint64_t generation, uint64_t index,
    uint64_t links, uint64_t bytes, int32_t (*poll)(void *), void *context) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9 && index == UINT64_MAX - 8);
    assert(links == 4096 && bytes == 33554432 && poll && context);
    assert(poll(context) == (result_mode == 5 ? 1 : 0));
    calls[0]++; return reply();
}
char *fcb_atlas_links_step_cancelable(uint64_t handle, uint64_t generation,
    int32_t (*poll)(void *), void *context) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9 && poll && context);
    assert(poll(context) == (result_mode == 5 ? 1 : 0));
    calls[1]++; return reply();
}
char *fcb_atlas_links_page(uint64_t handle, uint64_t generation, uint64_t direction,
    const char *path, uint64_t start, uint64_t limit) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9 && start == UINT64_MAX - 7 && limit == 64);
    assert((direction == 0 && path == NULL) || (direction == 2 && path && strcmp(path, "ff2e7273") == 0));
    calls[2]++; return reply();
}
char *fcb_atlas_links_open_reader(uint64_t handle, uint64_t reader, uint64_t generation, uint64_t link, uint64_t side) {
    assert(handle == UINT64_MAX - 10 && reader == UINT64_MAX - 6 && generation == UINT64_MAX - 9);
    assert(link == UINT64_MAX - 5 && side <= 1);
    calls[3]++; return reply();
}

char *fcb_atlas_imports_begin_cancelable(uint64_t handle, uint64_t generation, uint64_t index,
    uint64_t links, uint64_t bytes, int32_t (*poll)(void *), void *context) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9 && index == UINT64_MAX - 8);
    assert(links == 4096 && bytes == 33554432 && poll && context);
    assert(poll(context) == (result_mode == 5 ? 1 : 0));
    calls[5]++; return reply();
}
char *fcb_atlas_imports_step_cancelable(uint64_t handle, uint64_t generation,
    int32_t (*poll)(void *), void *context) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9 && poll && context);
    assert(poll(context) == (result_mode == 5 ? 1 : 0));
    calls[6]++; return reply();
}
char *fcb_atlas_imports_page(uint64_t handle, uint64_t generation, uint64_t direction,
    const char *path, uint64_t start, uint64_t limit) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9 && start == UINT64_MAX - 7 && limit == 64);
    assert((direction == 0 && path == NULL) || (direction == 2 && path && strcmp(path, "ff2e7273") == 0));
    calls[7]++; return reply();
}
char *fcb_atlas_imports_open_reader(uint64_t handle, uint64_t reader, uint64_t generation, uint64_t link, uint64_t side) {
    assert(handle == UINT64_MAX - 10 && reader == UINT64_MAX - 6 && generation == UINT64_MAX - 9);
    assert(link == UINT64_MAX - 5 && side <= 1);
    calls[8]++; return reply();
}
