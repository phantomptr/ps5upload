/* Host self-test for the installer daemon's HTTP range/header helpers and
 * the job coverage set / ring. No sockets, no Sony calls. */
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include <time.h>

#include "../installer/http_range.h"
#include "../installer/jobs.h"

static int failures = 0;
#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

/* 2025-01-01 00:00:00 UTC and 00:01:01 UTC. */
#define LM_EPOCH  1735689600
#define DATE_EPOCH 1735689661

static void test_range(void) {
    uint64_t s = 0, e = 0;
    /* full: bytes=0-99 on total 1000 */
    CHECK(inst_parse_range("bytes=0-99", 1000, &s, &e) == INST_RANGE_OK);
    CHECK(s == 0 && e == 99);
    /* open-ended: bytes=500- */
    CHECK(inst_parse_range("bytes=500-", 1000, &s, &e) == INST_RANGE_OK);
    CHECK(s == 500 && e == 999);
    /* suffix: bytes=-100 (last 100) */
    CHECK(inst_parse_range("bytes=-100", 1000, &s, &e) == INST_RANGE_OK);
    CHECK(s == 900 && e == 999);
    /* single byte bytes=0-0 */
    CHECK(inst_parse_range("bytes=0-0", 1000, &s, &e) == INST_RANGE_OK);
    CHECK(s == 0 && e == 0);
    /* past EOF end clamps: bytes=990-5000 on total 1000 */
    CHECK(inst_parse_range("bytes=990-5000", 1000, &s, &e) == INST_RANGE_OK);
    CHECK(s == 990 && e == 999);
    /* start past EOF -> unsatisfiable */
    CHECK(inst_parse_range("bytes=1000-1001", 1000, &s, &e) == INST_RANGE_UNSAT);
    /* suffix larger than file -> whole file */
    CHECK(inst_parse_range("bytes=-5000", 1000, &s, &e) == INST_RANGE_OK);
    CHECK(s == 0 && e == 999);
    /* zero-length file: any concrete range unsatisfiable */
    CHECK(inst_parse_range("bytes=0-0", 0, &s, &e) == INST_RANGE_UNSAT);
    CHECK(inst_parse_range("bytes=-1", 0, &s, &e) == INST_RANGE_UNSAT);
    /* no bytes= / not a range value -> NONE (serve full 200) */
    CHECK(inst_parse_range("", 1000, &s, &e) == INST_RANGE_NONE);
    CHECK(inst_parse_range("items=0-9", 1000, &s, &e) == INST_RANGE_NONE);
    /* multi-range -> NONE (caller serves the full 200) */
    CHECK(inst_parse_range("bytes=0-9,20-29", 1000, &s, &e) == INST_RANGE_NONE);
    /* malformed -> NONE, never a crash */
    CHECK(inst_parse_range("bytes=abc", 1000, &s, &e) == INST_RANGE_NONE);
}

static void test_head(void) {
    inst_http_req_t h;
    const char *g =
        "GET /1758800000-1/Game%20Name.pkg?product=x&serverIpAddr=y HTTP/1.1\r\n"
        "Host: 127.0.0.1\r\n"
        "Connection: keep-alive\r\n"
        "Range: bytes=0-99\r\n\r\n";
    CHECK(inst_http_parse_head(g, strlen(g), &h) == 0);
    CHECK(h.is_get == 1 && h.is_head == 0);
    CHECK(strcmp(h.method, "GET") == 0);
    /* query stripped, path percent-decoded */
    CHECK(strcmp(h.target, "/1758800000-1/Game Name.pkg") == 0);
    CHECK(h.keep_alive == 1);

    /* HTTP/1.1 defaults to keep-alive with no Connection header */
    const char *g2 = "GET /a.pkg HTTP/1.1\r\nHost: h\r\n\r\n";
    CHECK(inst_http_parse_head(g2, strlen(g2), &h) == 0);
    CHECK(h.keep_alive == 1);

    /* explicit close */
    const char *g3 = "HEAD /a.pkg HTTP/1.1\r\nConnection: close\r\n\r\n";
    CHECK(inst_http_parse_head(g3, strlen(g3), &h) == 0);
    CHECK(h.is_head == 1 && h.is_get == 0 && h.keep_alive == 0);

    /* HTTP/1.0 defaults to close */
    const char *g4 = "GET /a.pkg HTTP/1.0\r\n\r\n";
    CHECK(inst_http_parse_head(g4, strlen(g4), &h) == 0);
    CHECK(h.keep_alive == 0);

    /* header extraction, case-insensitive */
    char v[64];
    CHECK(inst_http_header(g, "range", v, sizeof(v)) == 1);
    CHECK(strcmp(v, "bytes=0-99") == 0);
    CHECK(inst_http_header(g, "X-Absent", v, sizeof(v)) == 0);

    /* malformed request line -> -1 */
    CHECK(inst_http_parse_head("garbage-no-spaces\r\n\r\n", 20, &h) == -1);
}

static void test_date(void) {
    char d[64];
    CHECK(inst_http_imf_date(LM_EPOCH, d, sizeof(d)) == 0);
    CHECK(strcmp(d, "Wed, 01 Jan 2025 00:00:00 GMT") == 0);
    CHECK(inst_http_imf_date(DATE_EPOCH, d, sizeof(d)) == 0);
    CHECK(strcmp(d, "Wed, 01 Jan 2025 00:01:01 GMT") == 0);
}

static void test_headers(void) {
    char b[1024];

    CHECK(inst_http_hdr_206(b, sizeof(b), 0, 99, 1000, DATE_EPOCH, LM_EPOCH, 1) > 0);
    CHECK(strcmp(b,
        "HTTP/1.1 206 Partial Content\r\n"
        "Date: Wed, 01 Jan 2025 00:01:01 GMT\r\n"
        "Last-Modified: Wed, 01 Jan 2025 00:00:00 GMT\r\n"
        "Content-Type: application/octet-stream\r\n"
        "Accept-Ranges: bytes\r\n"
        "Content-Length: 100\r\n"
        "Content-Range: bytes 0-99/1000\r\n"
        "Connection: keep-alive\r\n\r\n") == 0);

    CHECK(inst_http_hdr_200(b, sizeof(b), 1000, DATE_EPOCH, LM_EPOCH, 0) > 0);
    CHECK(strcmp(b,
        "HTTP/1.1 200 OK\r\n"
        "Date: Wed, 01 Jan 2025 00:01:01 GMT\r\n"
        "Last-Modified: Wed, 01 Jan 2025 00:00:00 GMT\r\n"
        "Content-Type: application/octet-stream\r\n"
        "Accept-Ranges: bytes\r\n"
        "Content-Length: 1000\r\n"
        "Connection: close\r\n\r\n") == 0);

    CHECK(inst_http_hdr_416(b, sizeof(b), 1000, 1) > 0);
    CHECK(strcmp(b,
        "HTTP/1.1 416 Range Not Satisfiable\r\n"
        "Content-Range: bytes */1000\r\n"
        "Accept-Ranges: bytes\r\n"
        "Content-Length: 0\r\n"
        "Connection: keep-alive\r\n\r\n") == 0);

    CHECK(inst_http_hdr_404(b, sizeof(b), 0) > 0);
    CHECK(strcmp(b,
        "HTTP/1.1 404 Not Found\r\n"
        "Content-Length: 0\r\n"
        "Connection: close\r\n\r\n") == 0);
}

static void test_coverage(void) {
    inst_coverage_t c;
    inst_coverage_reset(&c);
    /* adjacent merge */
    inst_coverage_add(&c, 0, 99);
    inst_coverage_add(&c, 100, 199);
    CHECK(inst_coverage_bytes(&c) == 200);
    /* overlap does not inflate */
    inst_coverage_add(&c, 50, 149);
    CHECK(inst_coverage_bytes(&c) == 200);
    /* exact re-read does not inflate */
    inst_coverage_add(&c, 0, 99);
    CHECK(inst_coverage_bytes(&c) == 200);
    /* out-of-order fill to complete */
    inst_coverage_reset(&c);
    inst_coverage_add(&c, 200, 299);
    inst_coverage_add(&c, 0, 99);
    CHECK(inst_coverage_complete(&c, 300) == 0);
    inst_coverage_add(&c, 100, 199);
    CHECK(inst_coverage_complete(&c, 300) == 1);
    CHECK(inst_coverage_bytes(&c) == 300);
    /* bytes=0-0 single byte */
    inst_coverage_reset(&c);
    inst_coverage_add(&c, 0, 0);
    CHECK(inst_coverage_bytes(&c) == 1);
}

static void test_finish(void) {
    CHECK(inst_job_should_finish(1, 600.0) == 1);
    CHECK(inst_job_should_finish(1, 599.9) == 0);
    CHECK(inst_job_should_finish(0, 100000.0) == 0);
}

static void test_ring(void) {
    inst_job_ring_t r;
    inst_ring_reset(&r);
    char id[32];
    for (int i = 0; i < 20; i++) {
        snprintf(id, sizeof(id), "job-%02d", i);
        inst_ring_put(&r, id, INST_JOB_ACCEPTED, 0, (uint64_t)i, 100);
    }
    inst_job_phase_t ph; uint32_t code; uint64_t bs, tot;
    /* first 4 evicted */
    CHECK(inst_ring_get(&r, "job-00", &ph, &code, &bs, &tot) == 0);
    CHECK(inst_ring_get(&r, "job-03", &ph, &code, &bs, &tot) == 0);
    /* last 16 present */
    CHECK(inst_ring_get(&r, "job-04", &ph, &code, &bs, &tot) == 1);
    CHECK(bs == 4);
    CHECK(inst_ring_get(&r, "job-19", &ph, &code, &bs, &tot) == 1);
    CHECK(bs == 19);
    /* update in place: same id does not consume a new slot */
    inst_ring_put(&r, "job-19", INST_JOB_DONE, 0, 100, 100);
    CHECK(inst_ring_get(&r, "job-19", &ph, &code, &bs, &tot) == 1);
    CHECK(ph == INST_JOB_DONE && bs == 100);
    CHECK(inst_ring_get(&r, "job-04", &ph, &code, &bs, &tot) == 1); /* still there */
}

int main(void) {
    test_range();
    test_head();
    test_date();
    test_headers();
    test_coverage();
    test_finish();
    test_ring();
    printf("installer_http_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
