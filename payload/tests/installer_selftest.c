/* Host self-test for the PS5Upload installer daemon's pure logic:
 * JSON request parsing, reply builders, path safety, path rewrite,
 * loopback URL building, network-class error test, boot-wait decision,
 * and install admission. Compiled with host cc; no Sony calls. */
#include <stdio.h>
#include <string.h>
#include <stdint.h>

#include "../installer/protocol.h"
#include "../installer/pathsafe.h"
#include "../installer/jobs.h"

static int failures = 0;

#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

static void test_parse(void) {
    inst_request_t r;

    /* hello */
    inst_parse_request("{\"op\":\"hello\"}", 14, &r);
    CHECK(r.error == NULL);
    CHECK(r.op == INST_OP_HELLO);

    /* install by url */
    const char *iu = "{\"op\":\"install\",\"url\":\"http://10.0.0.2:9113/a.pkg\",\"name_hint\":\"CUSA1 (Base)\"}";
    inst_parse_request(iu, strlen(iu), &r);
    CHECK(r.error == NULL);
    CHECK(r.op == INST_OP_INSTALL);
    CHECK(r.src == INST_SRC_URL);
    CHECK(strcmp(r.url, "http://10.0.0.2:9113/a.pkg") == 0);
    CHECK(strcmp(r.name_hint, "CUSA1 (Base)") == 0);

    /* install by path */
    const char *ip = "{\"op\":\"install\",\"path\":\"/data/x.pkg\"}";
    inst_parse_request(ip, strlen(ip), &r);
    CHECK(r.error == NULL);
    CHECK(r.op == INST_OP_INSTALL);
    CHECK(r.src == INST_SRC_PATH);
    CHECK(strcmp(r.path, "/data/x.pkg") == 0);

    /* install with BOTH url and path -> bad_request (must be exactly one) */
    const char *both = "{\"op\":\"install\",\"url\":\"http://h/a.pkg\",\"path\":\"/data/x.pkg\"}";
    inst_parse_request(both, strlen(both), &r);
    CHECK(r.error != NULL && strcmp(r.error, "bad_request") == 0);

    /* install with NEITHER -> bad_request */
    inst_parse_request("{\"op\":\"install\"}", 17, &r);
    CHECK(r.error != NULL && strcmp(r.error, "bad_request") == 0);

    /* job */
    const char *jb = "{\"op\":\"job\",\"job\":\"1758800000-3\"}";
    inst_parse_request(jb, strlen(jb), &r);
    CHECK(r.error == NULL);
    CHECK(r.op == INST_OP_JOB);
    CHECK(strcmp(r.job, "1758800000-3") == 0);

    /* stop */
    inst_parse_request("{\"op\":\"stop\"}", 13, &r);
    CHECK(r.error == NULL && r.op == INST_OP_STOP);

    /* unknown op -> bad_request */
    inst_parse_request("{\"op\":\"frobnicate\"}", 19, &r);
    CHECK(r.error != NULL && strcmp(r.error, "bad_request") == 0);

    /* missing op -> bad_request */
    inst_parse_request("{\"url\":\"http://h/a.pkg\"}", 24, &r);
    CHECK(r.error != NULL && strcmp(r.error, "bad_request") == 0);

    /* not JSON -> bad_request, never a crash */
    inst_parse_request("garbage", 7, &r);
    CHECK(r.error != NULL && strcmp(r.error, "bad_request") == 0);

    /* oversize (> INST_REQ_MAX) -> bad_request, no read past len */
    static char big[INST_REQ_MAX + 100];
    memset(big, 'x', sizeof(big));
    inst_parse_request(big, sizeof(big), &r);
    CHECK(r.error != NULL && strcmp(r.error, "bad_request") == 0);

    /* escaped quote in a value decodes */
    const char *esc = "{\"op\":\"install\",\"path\":\"/data/a\\\"b.pkg\"}";
    inst_parse_request(esc, strlen(esc), &r);
    CHECK(r.error == NULL);
    CHECK(strcmp(r.path, "/data/a\"b.pkg") == 0);
}

static void test_replies(void) {
    char b[512];

    CHECK(inst_reply_hello(b, sizeof(b), "1.0.0", "9.60", "ready", 0, 1) > 0);
    CHECK(strcmp(b, "{\"ok\":true,\"version\":\"1.0.0\",\"fw\":\"9.60\",\"state\":\"ready\",\"init_rc\":0,\"escalated\":true}") == 0);

    CHECK(inst_reply_install_ok(b, sizeof(b), "1758800000-1", "url") > 0);
    CHECK(strcmp(b, "{\"ok\":true,\"job\":\"1758800000-1\",\"via\":\"url\"}") == 0);

    CHECK(inst_reply_job(b, sizeof(b), "serving", 1024, 4096, 0) > 0);
    CHECK(strcmp(b, "{\"ok\":true,\"phase\":\"serving\",\"bytes_served\":1024,\"total\":4096,\"code\":0}") == 0);

    CHECK(inst_reply_ok(b, sizeof(b)) > 0);
    CHECK(strcmp(b, "{\"ok\":true}") == 0);

    CHECK(inst_reply_err_busy(b, sizeof(b), "1758800000-2") > 0);
    CHECK(strcmp(b, "{\"ok\":false,\"error\":\"busy\",\"job\":\"1758800000-2\"}") == 0);

    CHECK(inst_reply_err_not_ready(b, sizeof(b), 0x80020002u) > 0);
    CHECK(strcmp(b, "{\"ok\":false,\"error\":\"not_ready\",\"init_rc\":2147614722}") == 0);

    CHECK(inst_reply_err_str(b, sizeof(b), "bad_path") > 0);
    CHECK(strcmp(b, "{\"ok\":false,\"error\":\"bad_path\"}") == 0);

    CHECK(inst_reply_err_sony(b, sizeof(b), 0x80A30004u, "install the base game first") > 0);
    CHECK(strcmp(b, "{\"ok\":false,\"code\":2158166020,\"hint\":\"install the base game first\"}") == 0);

    /* a tiny buffer must be refused, never overflowed */
    char small[8];
    CHECK(inst_reply_ok(small, sizeof(small)) == -1);
}

static void test_pathsafe(void) {
    /* accepted roots */
    CHECK(inst_path_is_safe("/data/x.pkg") == 1);
    CHECK(inst_path_is_safe("/user/data/x.pkg") == 1);
    CHECK(inst_path_is_safe("/mnt/usb0/x.pkg") == 1);
    CHECK(inst_path_is_safe("/mnt/usb7/dir/x.pkg") == 1);
    CHECK(inst_path_is_safe("/mnt/ext0/x.pkg") == 1);
    /* case-insensitive extension */
    CHECK(inst_path_is_safe("/data/X.PKG") == 1);
    /* embedded dots in a component are fine */
    CHECK(inst_path_is_safe("/data/a..b.pkg") == 1);
    /* a "." component and an empty "//" component are benign (resolve to the
     * same file) and are accepted — only a real ".." component is rejected */
    CHECK(inst_path_is_safe("/data/./x.pkg") == 1);
    CHECK(inst_path_is_safe("/data//x.pkg") == 1);

    /* rejected: not absolute */
    CHECK(inst_path_is_safe("data/x.pkg") == 0);
    /* rejected: wrong extension */
    CHECK(inst_path_is_safe("/data/x.txt") == 0);
    CHECK(inst_path_is_safe("/data/x") == 0);
    /* rejected: a real ".." path component */
    CHECK(inst_path_is_safe("/data/../etc/x.pkg") == 0);
    CHECK(inst_path_is_safe("/data/..") == 0);
    /* rejected: outside allowed roots */
    CHECK(inst_path_is_safe("/etc/x.pkg") == 0);
    CHECK(inst_path_is_safe("/mnt/usbx/x.pkg") == 0);
    /* rejected: trailing slash (not a regular file name) */
    CHECK(inst_path_is_safe("/data/x.pkg/") == 0);
    /* rejected: NULL / empty */
    CHECK(inst_path_is_safe(NULL) == 0);
    CHECK(inst_path_is_safe("") == 0);
}

static void test_rewrite(void) {
    char out[64];
    CHECK(inst_rewrite_path("/data/x.pkg", out, sizeof(out)) == 0);
    CHECK(strcmp(out, "/user/data/x.pkg") == 0);
    /* non-/data left verbatim */
    CHECK(inst_rewrite_path("/mnt/usb0/x.pkg", out, sizeof(out)) == 0);
    CHECK(strcmp(out, "/mnt/usb0/x.pkg") == 0);
    /* overflow refused */
    char tiny[4];
    CHECK(inst_rewrite_path("/data/x.pkg", tiny, sizeof(tiny)) == -1);
}

static void test_url_build(void) {
    char enc[128];
    CHECK(inst_percent_encode("Game Name (v1.02).pkg", enc, sizeof(enc)) == 0);
    CHECK(strcmp(enc, "Game%20Name%20%28v1.02%29.pkg") == 0);
    /* unreserved chars pass through */
    CHECK(inst_percent_encode("a-b_c.d~e.pkg", enc, sizeof(enc)) == 0);
    CHECK(strcmp(enc, "a-b_c.d~e.pkg") == 0);

    CHECK(strcmp(inst_basename("/data/dir/x.pkg"), "x.pkg") == 0);
    CHECK(strcmp(inst_basename("x.pkg"), "x.pkg") == 0);

    char url[256];
    CHECK(inst_build_loopback_url(url, sizeof(url), 40123, "1758800000-1",
                                  "Game Name.pkg") > 0);
    CHECK(strcmp(url, "http://127.0.0.1:40123/1758800000-1/Game%20Name.pkg") == 0);
}

static void test_network_class(void) {
    CHECK(inst_is_network_class(0x80431068u) == 1);
    CHECK(inst_is_network_class(0x80431084u) == 1);
    CHECK(inst_is_network_class(0x8043FFFFu) == 1);
    /* not network-class */
    CHECK(inst_is_network_class(0x80A30004u) == 0);
    CHECK(inst_is_network_class(0x80B2150Fu) == 0);
    CHECK(inst_is_network_class(0u) == 0);
}

static void test_boot_wait(void) {
    CHECK(inst_should_boot_wait(0.0) == 1);
    CHECK(inst_should_boot_wait(120.0) == 1);   /* uptime <= 120s -> wait */
    CHECK(inst_should_boot_wait(120.001) == 0); /* over 120s -> skip */
    CHECK(inst_should_boot_wait(3600.0) == 0);
}

static void test_admission(void) {
    /* Only a SERVING loopback job that Sony has NOT finished reading blocks a
     * new install. A fully-read one used to block for its whole 10-minute idle
     * retire window, so on hardware a patch right after its base (and even an
     * unrelated stream install) was refused as busy. */
    CHECK(inst_admit_install(INST_JOB_SERVING, 0) == 0);
    CHECK(inst_admit_install(INST_JOB_SERVING, 1) == 1);
    CHECK(inst_admit_install(INST_JOB_ACCEPTED, 0) == 1); /* url job never blocks */
    CHECK(inst_admit_install(INST_JOB_NONE, 0) == 1);
    CHECK(inst_admit_install(INST_JOB_DONE, 0) == 1);
    CHECK(inst_admit_install(INST_JOB_FAILED, 0) == 1);
    CHECK(strcmp(inst_job_phase_str(INST_JOB_SERVING), "serving") == 0);
    CHECK(strcmp(inst_job_phase_str(INST_JOB_ACCEPTED), "accepted") == 0);
    CHECK(strcmp(inst_job_phase_str(INST_JOB_DONE), "done") == 0);
    CHECK(strcmp(inst_job_phase_str(INST_JOB_FAILED), "failed") == 0);
}

int main(void) {
    test_parse();
    test_replies();
    test_pathsafe();
    test_rewrite();
    test_url_build();
    test_network_class();
    test_boot_wait();
    test_admission();
    printf("installer_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
