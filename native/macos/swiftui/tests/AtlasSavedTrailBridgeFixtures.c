#include "fcb_saved_repository.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>

/* Header-checked marshaling and owned-response fixture. Real FCBT/archive
 * resolution and payload verification remain in the Rust service fixtures. */
static uint64_t calls[5];
static int result_mode;
void fixture_saved_trail_mode(int mode) { result_mode = mode; }
uint64_t fixture_saved_trail_count(uint64_t index) { assert(index < 5); return calls[index]; }
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
