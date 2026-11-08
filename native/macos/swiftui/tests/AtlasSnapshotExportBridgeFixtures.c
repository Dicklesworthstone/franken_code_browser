/* ABI shape and owned-allocation fixture; this does not write any archive. */
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
static unsigned calls, allocated, freed;
static int mode;
extern void snapshot_export_fixture_cancel(void *);
void snapshot_export_fixture_reset(void) { calls = allocated = freed = 0; mode = 0; }
void snapshot_export_fixture_mode(int value) { mode = value; }
unsigned snapshot_export_fixture_calls(void) { return calls; }
unsigned snapshot_export_fixture_live(void) { return allocated - freed; }

char *fcb_snapshot_save_cancelable(const char *root, const char *destination,
    uint64_t files, uint64_t file_bytes, uint64_t source_bytes, uint64_t ignores,
    int32_t (*poll)(void *), void *context) {
    assert(root && destination && !strcmp(root, "/repo") && !strcmp(destination, "/out/project.fcbs"));
    assert(poll && context && poll(context) == 0);
    if (mode == 7) assert(files == 1 && file_bytes == 0 && source_bytes == 0 && ignores == 0);
    else assert(files == 20000 && file_bytes == 1024 * 1024 && source_bytes == 64 * 1024 * 1024 && ignores == 1);
    ++calls;
    if (mode == 4 || mode == 5) {
        snapshot_export_fixture_cancel(context);
        assert(poll(context) != 0);
    }
    if (mode == 1 || mode == 5) return NULL;
    char *result;
    if (mode == 2) {
        result = malloc(2); assert(result); result[0] = (char)255; result[1] = 0;
    } else if (mode == 3) {
        size_t length = 8 * 1024 * 1024 + 1;
        result = malloc(length + 1); assert(result); memset(result, 'x', length); result[length] = 0;
    } else {
        result = strdup("{\"owned\":true}"); assert(result);
    }
    ++allocated; return result;
}
void fcb_free_string(char *pointer) { if (pointer) { ++freed; free(pointer); } }
