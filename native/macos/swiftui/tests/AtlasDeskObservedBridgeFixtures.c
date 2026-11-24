#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <assert.h>
#include "../../../../crates/fcb-bridge/include/fcb_desk.h"

static uint64_t arguments[8], call_count, free_count, body_length;
static uint8_t body_copy[16384];
static char label_copy[257];
static unsigned mode;

void desk_observed_fixture_reset(void) {
    memset(arguments, 0, sizeof(arguments));
    memset(body_copy, 0, sizeof(body_copy));
    memset(label_copy, 0, sizeof(label_copy));
    call_count = free_count = body_length = 0;
    mode = 0;
}
void desk_observed_fixture_mode(uint64_t value) { mode = (unsigned)value; }
uint64_t desk_observed_fixture_argument(uint64_t index) { return index < 8 ? arguments[index] : 0; }
uint64_t desk_observed_fixture_calls(void) { return call_count; }
uint64_t desk_observed_fixture_frees(void) { return free_count; }
uint64_t desk_observed_fixture_body_length(void) { return body_length; }
uint64_t desk_observed_fixture_body_byte(uint64_t index) { return index < body_length ? body_copy[index] : 0; }
uint64_t desk_observed_fixture_label_byte(uint64_t index) { return index < sizeof(label_copy) ? (uint8_t)label_copy[index] : 0; }

static void record(const uint64_t *values, size_t count) {
    assert(count <= 8);
    memset(arguments, 0, sizeof(arguments));
    memcpy(arguments, values, count * sizeof(*values));
    call_count++;
}
static char *reply(void) {
    if (mode == 1) return NULL;
    if (mode == 2) {
        char *out = malloc(2); assert(out);
        out[0] = (char)0xff; out[1] = 0; return out;
    }
    if (mode == 3 || mode == 4) {
        size_t bytes = mode == 3 ? 16u * 1024u * 1024u + 1u : 5u * 1024u * 1024u;
        char *out = malloc(bytes + 1); assert(out);
        memset(out, 'x', bytes); out[bytes] = 0; return out;
    }
    char *out = malloc(3); assert(out); memcpy(out, "{}", 3); return out;
}
void fcb_free_string(char *value) { if (value) { free_count++; free(value); } }

char *fcb_desk_import_observation(uint64_t h, uint64_t revision, uint64_t attempt,
    uint64_t reader, uint64_t observation, uint64_t start, uint64_t end) {
    const uint64_t values[] = {h, revision, attempt, reader, observation, start, end};
    record(values, 7); return reply();
}
char *fcb_desk_import_stream_excerpt(uint64_t h, uint64_t revision, uint64_t attempt,
    uint64_t stream, uint64_t family, uint64_t generation, uint64_t query_revision, uint64_t index) {
    const uint64_t values[] = {h, revision, attempt, stream, family, generation, query_revision, index};
    record(values, 8); return reply();
}
char *fcb_desk_observed_passage(uint64_t h, uint64_t revision, uint64_t passage) {
    const uint64_t values[] = {h, revision, passage};
    record(values, 3); return reply();
}
char *fcb_desk_edit_observed_passage(uint64_t h, uint64_t revision, uint64_t attempt,
    uint64_t passage, uint64_t sequence, const char *label, const uint8_t *body, uint64_t bytes) {
    const uint64_t values[] = {h, revision, attempt, passage, sequence, bytes};
    record(values, 6);
    assert(label && strlen(label) <= 256 && bytes <= sizeof(body_copy) && (body || !bytes));
    memcpy(label_copy, label, strlen(label) + 1);
    body_length = bytes;
    if (bytes) memcpy(body_copy, body, (size_t)bytes);
    return reply();
}
char *fcb_desk_forget_observed_passage(uint64_t h, uint64_t revision, uint64_t attempt,
    uint64_t passage, uint64_t sequence) {
    const uint64_t values[] = {h, revision, attempt, passage, sequence};
    record(values, 5); return reply();
}

/* The production bridge remains one unit. Unrelated functions are explicit
 * inert link fixtures; none is used by this observed-passage ABI regression. */
#define U __attribute__((unused))
#define STUB(name, args) char *name args { return NULL; }
uint64_t fcb_desk_create(void) { return 1; }
STUB(fcb_desk_start, (uint64_t h U))
STUB(fcb_desk_state, (uint64_t h U))
STUB(fcb_desk_open, (uint64_t h U, uint64_t r U, uint64_t a U, const char *p U))
STUB(fcb_desk_apply, (uint64_t h U, uint64_t r U, uint64_t a U, uint64_t k U,
    uint64_t s U, uint64_t b U, uint64_t e U, const char *l U))
STUB(fcb_desk_annotate, (uint64_t h U, uint64_t r U, uint64_t a U, uint64_t p U,
    const char *l U, const uint8_t *b U, uint64_t n U))
STUB(fcb_desk_edit_bookmark, (uint64_t h U, uint64_t r U, uint64_t a U, uint64_t p U,
    const char *l U, const uint8_t *b U, uint64_t n U))
STUB(fcb_desk_suggest_reattachment, (uint64_t h U, uint64_t r U, uint64_t b U,
    uint64_t s U, uint64_t p U, uint64_t w U, uint64_t e U))
STUB(fcb_desk_reattach_range, (uint64_t h U, uint64_t r U, uint64_t a U, uint64_t b U,
    uint64_t s U, uint64_t p U, uint64_t f U, uint64_t sr U, uint64_t start U, uint64_t end U))
STUB(fcb_desk_window, (uint64_t h U, uint64_t r U, uint64_t p U, uint64_t o U, uint64_t b U))
STUB(fcb_desk_import_reader, (uint64_t h U, uint64_t r U, uint64_t a U, uint64_t reader U,
    uint64_t start U, uint64_t end U))
STUB(fcb_desk_open_reader, (uint64_t h U, uint64_t r U, uint64_t p U, uint64_t destination U))
STUB(fcb_desk_restore, (uint64_t h U, uint64_t r U, uint64_t a U, const char *p U))
STUB(fcb_desk_save, (uint64_t h U, uint64_t r U, const char *p U, uint64_t b U))
STUB(fcb_desk_export, (uint64_t h U, uint64_t r U, const char *p U,
    uint64_t source U, uint64_t bytes U, uint64_t scalars U))
uint8_t fcb_desk_checkpoint_verify_fd(int32_t fd U, uint64_t n U, const uint8_t *d U) { return 0; }
uint8_t fcb_desk_cancel(uint64_t h U) { return 1; }
uint8_t fcb_desk_close(uint64_t h U) { return 1; }
