#include "fcb_saved_repository.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>

/* Public-header-checked foreign boundary. These responses test Swift marshaling
 * and ownership; real archive/posting behavior is exercised in saved_sessions_tests. */
static uint64_t calls[14];
static int result_mode;
void fixture_saved_index_mode(int mode) { result_mode = mode; }
uint64_t fixture_saved_index_count(uint64_t index) { assert(index < 14); return calls[index]; }
static char *reply(void) {
    if (result_mode == 1) return NULL;
    if (result_mode == 2) { char *p = malloc(2); assert(p); p[0] = (char)0xff; p[1] = 0; return p; }
    size_t length = result_mode == 3 ? 8 * 1024 * 1024 + 1 : (result_mode == 4 ? 0 : 2);
    char *p = malloc(length + 1); assert(p); memset(p, 'x', length); p[length] = 0;
    if (length == 2) { p[0] = '{'; p[1] = '}'; }
    return p;
}
void fcb_free_string(char *pointer) { assert(pointer); calls[6]++; free(pointer); }
uint64_t fcb_saved_create(void) { calls[0]++; return UINT64_MAX - 10; }
char *fcb_saved_attach_index(uint64_t handle, uint64_t generation, const char *path, const char *pin) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9);
    assert(path && strcmp(path, "/index-e\xcc\x81.fcbd") == 0);
    assert(pin && strlen(pin) == 64);
    for (size_t i = 0; i < 64; ++i) { assert(pin[i] == 'a'); }
    calls[1]++; return reply();
}
char *fcb_saved_detach_index(uint64_t handle, uint64_t generation) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9); calls[2]++; return reply();
}
char *fcb_saved_info(uint64_t handle) { assert(handle == UINT64_MAX - 10); calls[3]++; return reply(); }
uint8_t fcb_saved_cancel(uint64_t handle) { assert(handle == UINT64_MAX - 10); calls[4]++; return 1; }
uint8_t fcb_saved_close(uint64_t handle) { assert(handle == UINT64_MAX - 10); calls[5]++; return 1; }

#define STUB2(name) char *name(uint64_t a, uint64_t b) { (void)a; (void)b; abort(); }
#define STUB3(name) char *name(uint64_t a, uint64_t b, uint64_t c) { (void)a; (void)b; (void)c; abort(); }
#define STUB4(name) char *name(uint64_t a, uint64_t b, uint64_t c, uint64_t d) { (void)a; (void)b; (void)c; (void)d; abort(); }
char *fcb_saved_open(uint64_t a, const char *b) { (void)a; (void)b; abort(); }
STUB3(fcb_saved_members)
char *fcb_saved_search(uint64_t a, uint64_t b, const char *c, uint64_t d) { (void)a; (void)b; (void)c; (void)d; abort(); }
char *fcb_saved_search_counted(uint64_t handle, uint64_t generation, const char *needle, uint64_t matches) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9);
    assert(needle && strcmp(needle, "e\xcc\x81") == 0 && (matches == 0 || matches == 4096));
    calls[7]++; return reply();
}
char *fcb_saved_search_matching(uint64_t handle, uint64_t generation, const char *needle,
    uint64_t case_sensitive, uint64_t count_all_matches, uint64_t matches) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9);
    assert(needle && strcmp(needle, "e\xcc\x81") == 0);
    if (calls[9] == 0) { assert(case_sensitive == 0 && count_all_matches == 0 && matches == 4096); }
    else if (calls[9] == 2) { assert(case_sensitive == 1 && count_all_matches == 1 && matches == 4096); }
    else { assert(case_sensitive == 0 && count_all_matches == 1 && matches == 0); }
    calls[9]++; return reply();
}
STUB4(fcb_saved_results)
STUB2(fcb_saved_clear_results)
STUB4(fcb_saved_open_hit_reader)
char *fcb_saved_expression_search(uint64_t a, uint64_t b, const char *c, uint64_t d, uint64_t e) {
    (void)a; (void)b; (void)c; (void)d; (void)e; abort();
}
char *fcb_saved_expression_search_counted(uint64_t handle, uint64_t generation, const char *query,
    uint64_t matches, uint64_t bytes) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9);
    assert(query && strcmp(query, "e\xcc\x81 path:src/") == 0 && (matches == 0 || matches == 4096));
    assert(bytes == 67108864); calls[8]++; return reply();
}
STUB4(fcb_saved_expression_results)
STUB2(fcb_saved_clear_expression)
STUB4(fcb_saved_open_expression_hit_reader)
STUB4(fcb_saved_prepare_relationships)
char *fcb_saved_relationships(uint64_t a, uint64_t b, uint64_t c, uint64_t d, uint64_t e, uint64_t f) {
    (void)a; (void)b; (void)c; (void)d; (void)e; (void)f; abort();
}
STUB2(fcb_saved_clear_relationships)
STUB3(fcb_saved_open_member_reader)
char *fcb_saved_open_relationship_reader(uint64_t a, uint64_t b, uint64_t c, uint64_t d, uint64_t e) {
    (void)a; (void)b; (void)c; (void)d; (void)e; abort();
}
char *fcb_saved_prepare_imports(uint64_t handle, uint64_t generation, uint64_t links, uint64_t bytes) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9);
    assert(links == 4096 && bytes == 33554432); calls[10]++; return reply();
}
char *fcb_saved_imports(uint64_t handle, uint64_t generation, uint64_t direction,
    uint64_t member, uint64_t start, uint64_t limit) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9);
    assert(direction == 2 && member == 65535 && start == 4096 && limit == 128); calls[11]++; return reply();
}
char *fcb_saved_clear_imports(uint64_t handle, uint64_t generation) {
    assert(handle == UINT64_MAX - 10 && generation == UINT64_MAX - 9); calls[12]++; return reply();
}
char *fcb_saved_open_import_reader(uint64_t handle, uint64_t reader, uint64_t generation, uint64_t id, uint64_t side) {
    assert(handle == UINT64_MAX - 10 && reader == UINT64_MAX - 8 && generation == UINT64_MAX - 9);
    assert(id == 4096 && side == 1); calls[13]++; return reply();
}
/* The production transport also contains these independent capabilities. Keep
 * every referenced symbol header-checked even when this fixture does not use it. */
char *fcb_saved_structure_search(uint64_t a, uint64_t b, uint64_t c, uint64_t d,
    const char *e, uint64_t f, uint64_t g) {
    (void)a; (void)b; (void)c; (void)d; (void)e; (void)f; (void)g; abort();
}
STUB4(fcb_saved_structure_results)
STUB2(fcb_saved_clear_structure)
STUB4(fcb_saved_open_structure_hit_reader)
STUB4(fcb_saved_document_images_authorize)
char *fcb_saved_document_image_prepare(uint64_t a, uint64_t b, uint64_t c, uint64_t d, uint64_t e, uint64_t f) {
    (void)a; (void)b; (void)c; (void)d; (void)e; (void)f; abort();
}
