/* Production Swift transport linked to a bounded injected C ABI. This fixture
 * performs no discovery, source I/O, or engine search. */
#include "../../../../crates/fcb-bridge/include/fcb_atlas_stream_search.h"
#include "../../../../crates/fcb-bridge/include/fcb_atlas_sessions.h"
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

enum { CREATE, OPEN, SEARCH, PAGE, HIT, CLEAR, CANCEL, CLOSE, POLLS,
    PROGRESS_SEARCH, PROGRESS_CALLBACKS, STOP_POLLS, STOP_SEEN, PROGRESS_CANCELED, COUNTERS };
static uint32_t counts[COUNTERS], allocations, frees;
static int32_t mode;
static char *owned;
static const uint64_t owner_value = UINT64_C(0xfedcba9876543210);
static const uint64_t generation_value = UINT64_C(0x8000000100000007);
typedef int32_t (*workspace_fixture_poll)(void *context);
extern void workspace_stream_fixture_cancel(void *context);
void workspace_stream_fixture_reset(void) {
    assert(owned == NULL && allocations == frees);
    memset(counts, 0, sizeof(counts)); allocations = frees = 0; mode = 0;
}
void workspace_stream_fixture_mode(int32_t value) { mode = value; }
uint32_t workspace_stream_fixture_count(uint32_t kind) { assert(kind < COUNTERS); return counts[kind]; }
uint32_t workspace_stream_fixture_allocations(void) { return allocations; }
uint32_t workspace_stream_fixture_frees(void) { return frees; }

static void poll_context(workspace_fixture_poll poll, void *context) {
    assert(poll != NULL && context != NULL); ++counts[POLLS]; assert(poll(context) == 0);
    if (mode == 4 || mode == 5) {
        workspace_stream_fixture_cancel(context); ++counts[POLLS]; assert(poll(context) != 0);
    }
}
/* Full injected wire examples pass through the actual Swift bridge and decoder.
 * They deliberately retain a high-bit owner/generation and a Unicode literal. */
#define OWNER "\"owner\":\"18364758544493064720\",\"source_manifest\":\"1\",\"layout_revision\":\"1\""
#define ROOT "\"root_path\":{\"encoding\":\"unix-bytes\",\"hex\":\"2f7265706f2dc3a9\",\"display\":\"/repo-é\"}"
#define GENERATION "\"query_generation\":\"9223372041149743111\",\"last_attempt\":\"9223372041149743111\""
#define COMMON OWNER "," ROOT "," GENERATION "," \
    "\"scope\":\"catalogued-whole-files\",\"strategy\":\"streaming-whole-file\",\"mode\":\"exact-decoded-literal\"," \
    "\"capture_kind\":\"literal-witnesses-only\",\"source_observation\":\"per-file-streams-not-atomic-workspace\"," \
    "\"catalogued_files\":\"1\",\"discovery_policy\":\"product-defaults/no-rule-files-v1\",\"excluded_entries\":\"0\""
#define NEEDLE "\"needle\":\"exact é😀\""
#define ROW "\"index\":\"0\",\"occurrence_id\":\"1\",\"file_id\":\"1\",\"node_ordinal\":\"1\",\"source_revision\":\"1\"," \
    "\"path\":{\"encoding\":\"unix-bytes\",\"hex\":\"6c617267652e747874\",\"display\":\"large.txt\"}," \
    "\"original_range\":{\"start\":\"32\",\"end\":\"44\"},\"encoding\":\"utf8\",\"observed_length\":\"64\",\"final_observed_length\":\"64\"," \
    "\"consistency\":\"unchanged-metadata-not-atomic\",\"file_state\":\"complete-observed-input\",\"file_complete\":true,\"file_error_code\":null," \
    "\"witness_sha256\":\"1dd22bef0082e070caf6cfda614fc6c4d589c241ab0bbc380025fb7117a31a84\""
static const char *success(unsigned kind) {
    if (kind == OPEN) {
        return "{\"schema\":\"fcb.atlas-session/1\",\"status\":\"ok\",\"command\":\"info\"," OWNER "," ROOT ","
            "\"catalogued_files\":\"1\",\"workspace_files\":\"1\",\"discovery_complete\":true,\"scope\":\"all\","
            "\"retention\":\"frozen-catalog-and-spatial-index\"}";
    }
    if (kind == SEARCH) {
        return "{\"schema\":\"fcb.atlas-stream-search/1\",\"status\":\"ok\",\"command\":\"search\"," COMMON "," NEEDLE ","
            "\"discovery_complete\":true,\"search_complete\":true,\"truncated\":false,\"stop_reason\":null,"
            "\"files_examined\":\"1\",\"files_unexamined\":\"0\",\"incomplete_files\":\"0\",\"unavailable_files\":\"0\","
            "\"unsupported_files\":\"0\",\"failed_files\":\"0\",\"matching_files\":\"1\",\"retained_hits\":\"1\",\"matches_seen\":\"1\","
            "\"payload_bytes_read\":\"64\",\"scanned_input_bytes\":\"64\",\"read_calls\":\"1\",\"interrupted_read_calls\":\"0\","
            "\"peak_input_buffer_bytes\":\"64\",\"retained_witness_bytes\":\"12\","
            "\"limits\":{\"max_matches\":\"256\",\"max_scan_bytes\":\"4294967296\",\"max_read_calls\":\"1048576\"},"
            "\"page_offset\":\"0\",\"page_count\":\"1\",\"hits\":[{" ROW "}],\"next_offset\":null,"
            "\"diagnostics\":[],\"diagnostics_truncated\":false}";
    }
    assert(kind == HIT);
    return "{\"schema\":\"fcb.atlas-stream-search/1\",\"status\":\"ok\",\"command\":\"hit\"," COMMON "," NEEDLE ","
        "\"hit\":{" ROW ",\"text\":\"exact é😀\",\"original_hex\":\"657861637420c3a9f09f9880\"},"
        "\"additional_source_bytes_read\":\"0\",\"additional_read_calls\":\"0\"}";
}
static char *response(unsigned kind) {
    assert(owned == NULL);
    if (mode == 1 || mode == 5) return NULL;
    if (mode == 2) {
        owned = malloc(2); assert(owned != NULL); owned[0] = (char)255; owned[1] = 0;
    } else if (mode == 3) {
        size_t length = 8 * 1024 * 1024 + 1;
        owned = malloc(length + 1); assert(owned != NULL); memset(owned, 'x', length); owned[length] = 0;
    } else if (mode == 7) {
        const char *value = success(kind); size_t length = strlen(value) + 1;
        owned = malloc(length); assert(owned != NULL); memcpy(owned, value, length);
    } else {
        const char value[] = "{\"fixture\":\"workspace-stream\"}";
        owned = malloc(sizeof(value)); assert(owned != NULL); memcpy(owned, value, sizeof(value));
    }
    ++allocations; return owned;
}
uint64_t fcb_atlas_create(void) { ++counts[CREATE]; return mode == 6 ? 0 : owner_value; }
char *fcb_atlas_open_cancelable(uint64_t owner, const char *root, uint64_t maximum_files,
    workspace_fixture_poll poll, void *context) {
    assert(owner == owner_value && maximum_files == 20000 && root != NULL);
    assert(strcmp(root, "/repo-\xc3\xa9") == 0); ++counts[OPEN]; poll_context(poll, context);
    assert(strcmp(root, "/repo-\xc3\xa9") == 0); return response(OPEN);
}
char *fcb_atlas_stream_search_cancelable(uint64_t owner, const char *needle, uint64_t maximum_matches,
    uint64_t maximum_bytes, uint64_t maximum_calls, workspace_fixture_poll poll, void *context) {
    assert(owner == owner_value && needle != NULL && maximum_matches == 256);
    assert(maximum_bytes == UINT64_C(4294967296) && maximum_calls == 1048576);
    assert(strcmp(needle, "exact \xc3\xa9\xf0\x9f\x98\x80") == 0);
    ++counts[SEARCH]; poll_context(poll, context);
    assert(strcmp(needle, "exact \xc3\xa9\xf0\x9f\x98\x80") == 0); return response(SEARCH);
}
char *fcb_atlas_stream_search_progress(uint64_t owner, const char *needle, uint64_t maximum_matches,
    uint64_t maximum_bytes, uint64_t maximum_calls, workspace_fixture_poll poll,
    workspace_fixture_poll stop, fcb_atlas_stream_progress_callback progress, void *context) {
    assert(owner == owner_value && needle != NULL && maximum_matches == 256);
    assert(maximum_bytes == UINT64_C(4294967296) && maximum_calls == 1048576);
    assert(strcmp(needle, "exact \xc3\xa9\xf0\x9f\x98\x80") == 0);
    assert(poll != NULL && stop != NULL && progress != NULL && context != NULL);
    ++counts[PROGRESS_SEARCH]; ++counts[POLLS]; assert(poll(context) == 0);
    ++counts[STOP_POLLS];
    if (stop(context) != 0) { ++counts[STOP_SEEN]; return response(SEARCH); }
    uint8_t borrowed[] = "{\"fixture\":\"workspace-progress\"}";
    const uint8_t invalid[] = { 255 };
    ++counts[PROGRESS_CALLBACKS];
    if (mode == 8) progress(invalid, sizeof(invalid), context);
    else if (mode == 9) progress(borrowed, UINT64_C(8) * 1024 * 1024 + 1, context);
    else if (mode == 10) progress(NULL, 1, context);
    else if (mode == 11) progress(borrowed, 0, context);
    else progress(borrowed, sizeof(borrowed) - 1, context);
    // A retained callback pointer would now see changed bytes. Swift must have
    // copied exactly the supplied range before the synchronous callback returned.
    memset(borrowed, 'x', sizeof(borrowed));
    ++counts[POLLS];
    if (poll(context) != 0) { ++counts[PROGRESS_CANCELED]; return response(SEARCH); }
    ++counts[STOP_POLLS];
    if (stop(context) != 0) ++counts[STOP_SEEN];
    assert(strcmp(needle, "exact \xc3\xa9\xf0\x9f\x98\x80") == 0);
    return response(SEARCH);
}

char *fcb_atlas_stream_search_page(uint64_t owner, uint64_t generation, uint64_t start, uint64_t count) {
    assert(owner == owner_value && generation == generation_value && start == 224 && count == 32);
    ++counts[PAGE]; return response(PAGE);
}
char *fcb_atlas_stream_search_hit(uint64_t owner, uint64_t generation, uint64_t index) {
    assert(owner == owner_value && generation == generation_value && index == (mode == 7 ? 0 : 255));
    ++counts[HIT]; return response(HIT);
}
char *fcb_atlas_stream_search_clear(uint64_t owner) {
    assert(owner == owner_value); ++counts[CLEAR]; return response(CLEAR);
}
uint8_t fcb_atlas_cancel(uint64_t owner) { assert(owner == owner_value); ++counts[CANCEL]; return mode == 6 ? 0 : 1; }
uint8_t fcb_atlas_close(uint64_t owner) { assert(owner == owner_value); ++counts[CLOSE]; return mode == 6 ? 0 : 1; }
void fcb_free_string(char *pointer) {
    assert(pointer != NULL && pointer == owned && allocations == frees + 1);
    owned = NULL; ++frees; free(pointer);
}
