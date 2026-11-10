#include "fcb_city.h"
#include <stdlib.h>

/* Link-only symbols. Production Swift worker/model tests inject wire replies;
 * these stubs never pretend to provide Rust geometry, a GPU or source I/O. */
uint64_t fcb_city_create(void) { return 0; }
char *fcb_city_open(uint64_t h, uint64_t l, const uint64_t *i, const double *r,
    const uint64_t *b, const uint8_t *k, uint64_t n, uint32_t c) {
    (void)h; (void)l; (void)i; (void)r; (void)b; (void)k; (void)n; (void)c; return NULL;
}
char *fcb_city_info(uint64_t h) { (void)h; return NULL; }
char *fcb_city_view(uint64_t h, uint64_t g, double w, double v, double x, double y,
    double s, double a, double t) {
    (void)h; (void)g; (void)w; (void)v; (void)x; (void)y; (void)s; (void)a; (void)t; return NULL;
}
char *fcb_city_fit(uint64_t h, uint64_t g, double w, double v) {
    (void)h; (void)g; (void)w; (void)v; return NULL;
}
char *fcb_city_pan(uint64_t h, uint64_t g, uint64_t b, double x, double y) {
    (void)h; (void)g; (void)b; (void)x; (void)y; return NULL;
}
char *fcb_city_zoom(uint64_t h, uint64_t g, uint64_t b, double x, double y, double f) {
    (void)h; (void)g; (void)b; (void)x; (void)y; (void)f; return NULL;
}
char *fcb_city_orbit(uint64_t h, uint64_t g, uint64_t b, double a, double t) {
    (void)h; (void)g; (void)b; (void)a; (void)t; return NULL;
}
char *fcb_city_resize(uint64_t h, uint64_t g, uint64_t b, double w, double v) {
    (void)h; (void)g; (void)b; (void)w; (void)v; return NULL;
}
char *fcb_city_present(uint64_t h, uint64_t g, uint64_t f, uint64_t d) {
    (void)h; (void)g; (void)f; (void)d; return NULL;
}
char *fcb_city_pick(uint64_t h, uint64_t f, uint64_t d, double x, double y) {
    (void)h; (void)f; (void)d; (void)x; (void)y; return NULL;
}
uint32_t fcb_city_cancel(uint64_t h) { (void)h; return 0; }
uint32_t fcb_city_close(uint64_t h) { (void)h; return 0; }
void fcb_free_string(char *text) { free(text); }
