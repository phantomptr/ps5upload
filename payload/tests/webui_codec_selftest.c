/* Host-side tests for the console web UI's JSON <-> AVA1 adapters (webui_codec.c):
 * Unicode / quote / backslash paths round-trip, surrogate pairs, absent vs empty keys,
 * fs.list results to JSON (exact text, and paging when the reply window is full), the job
 * list, and the small parsers the routes use. Build: see the root Makefile test-payload. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "ava1_app_gen.h"
#include "ava1_wire.h"
#include "webui_codec.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

/* ---- a strict JSON validator (strings must be valid UTF-8, no raw control chars) ---- */

static const char *jp;
static int jv_value(void);

static void jv_ws(void) {
    while (*jp == ' ' || *jp == '\n' || *jp == '\t' || *jp == '\r') jp++;
}

static int utf8_len(const unsigned char *s) {
    unsigned c = s[0], cp;
    int n, i;
    if (c < 0x80) return 1;
    if (c >= 0xc2 && c <= 0xdf) n = 2, cp = c & 0x1f;
    else if (c >= 0xe0 && c <= 0xef) n = 3, cp = c & 0x0f;
    else if (c >= 0xf0 && c <= 0xf4) n = 4, cp = c & 0x07;
    else return 0;
    for (i = 1; i < n; i++) {
        if ((s[i] & 0xc0) != 0x80) return 0;
        cp = (cp << 6) | (s[i] & 0x3f);
    }
    if ((n == 3 && cp < 0x800) || (n == 4 && (cp < 0x10000 || cp > 0x10ffff)) || (cp >= 0xd800 && cp < 0xe000))
        return 0;
    return n;
}

static int jv_string(void) {
    if (*jp != '"') return 0;
    jp++;
    while (*jp && *jp != '"') {
        unsigned char c = (unsigned char)*jp;
        if (c < 0x20) return 0;
        if (c == '\\') {
            jp++;
            if (!*jp || !strchr("\"\\/bfnrtu", *jp)) return 0;
            if (*jp == 'u') {
                int k;
                for (k = 1; k <= 4; k++)
                    if (!strchr("0123456789abcdefABCDEF", jp[k]) || !jp[k]) return 0;
                jp += 4;
            }
            jp++;
        } else {
            int n = utf8_len((const unsigned char *)jp);
            if (!n) return 0;
            jp += n;
        }
    }
    if (*jp != '"') return 0;
    jp++;
    return 1;
}

static int jv_value(void) {
    jv_ws();
    if (*jp == '{') {
        jp++;
        jv_ws();
        if (*jp == '}') {
            jp++;
            return 1;
        }
        for (;;) {
            jv_ws();
            if (!jv_string()) return 0;
            jv_ws();
            if (*jp++ != ':') return 0;
            if (!jv_value()) return 0;
            jv_ws();
            if (*jp == ',') {
                jp++;
                continue;
            }
            if (*jp == '}') {
                jp++;
                return 1;
            }
            return 0;
        }
    }
    if (*jp == '[') {
        jp++;
        jv_ws();
        if (*jp == ']') {
            jp++;
            return 1;
        }
        for (;;) {
            if (!jv_value()) return 0;
            jv_ws();
            if (*jp == ',') {
                jp++;
                continue;
            }
            if (*jp == ']') {
                jp++;
                return 1;
            }
            return 0;
        }
    }
    if (*jp == '"') return jv_string();
    if (!strncmp(jp, "true", 4)) return jp += 4, 1;
    if (!strncmp(jp, "false", 5)) return jp += 5, 1;
    if (!strncmp(jp, "null", 4)) return jp += 4, 1;
    if (*jp == '-' || (*jp >= '0' && *jp <= '9')) {
        if (*jp == '-') jp++;
        if (!(*jp >= '0' && *jp <= '9')) return 0;
        while (*jp >= '0' && *jp <= '9') jp++;
        if (*jp == '.') { /* a fraction needs a digit after the point */
            jp++;
            if (!(*jp >= '0' && *jp <= '9')) return 0;
            while (*jp >= '0' && *jp <= '9') jp++;
        }
        return 1;
    }
    return 0;
}

static int json_valid(const char *s) {
    jp = s;
    if (!jv_value()) return 0;
    jv_ws();
    return *jp == '\0';
}

/* ---- helpers ---- */

/* {"<key>":"<escaped s>"} in a heap buffer. */
static char *obj_with(const char *key, const char *s, size_t n) {
    size_t need = webui_json_escape(NULL, 0, s, n);
    size_t cap = need + strlen(key) + 16;
    char *buf = malloc(cap);
    int at;
    if (!buf) return NULL;
    at = snprintf(buf, cap, "{\"%s\":\"", key);
    webui_json_escape(buf + at, cap - (size_t)at, s, n);
    strcat(buf, "\"}");
    return buf;
}

/* An encoded FsListResult of `count` entries named by `name_of`. */
static size_t encode_list(uint8_t *out, size_t cap, const ava1_fs_entry_t *e, size_t count, uint32_t scanned,
                          int more) {
    uint8_t *blob = malloc(1 << 20);
    ava1_w_t bw, w;
    ava1_fs_list_result_t r;
    size_t i;
    ava1_w_init(&bw, blob, 1 << 20);
    for (i = 0; i < count; i++) ava1_fs_entry_append(&bw, &e[i]);
    memset(&r, 0, sizeof r);
    r.entries = blob;
    r.entries_len = (uint32_t)bw.len;
    r.total_scanned = scanned;
    r.more = (uint8_t)more;
    ava1_w_init(&w, out, cap);
    ava1_fs_list_result_encode(&r, &w);
    free(blob);
    return w.err ? 0 : w.len;
}

/* ---- the brief's tests ---- */

static void unicode_path_round_trips(void) {
    static const char path[] = "/data/homebrew/\xe5\xae\x8c\xe8\x9b\x8b\xef\xbc\x81\xe6\x88\x91\xe8\xa2\xab\xe7\xbe\x8e"
                               "\xe5\xa5\xb3\xe5\x8c\x85\xe5\x9b\xb4\xe4\xba\x86\xef\xbc\x81/a \"b\"\\c.txt";
    char out[1024];
    char *obj = obj_with("path", path, sizeof path - 1);
    CHECK(obj != NULL);
    if (!obj) return;
    CHECK(json_valid(obj));
    CHECK(webui_json_str(obj, strlen(obj), "path", out, sizeof out) == 1);
    CHECK(strlen(out) == sizeof path - 1 && memcmp(out, path, sizeof path - 1) == 0);
    /* the escaped form is what the PC engine's serde would send too */
    CHECK(strstr(obj, "a \\\"b\\\"\\\\c.txt") != NULL);
    free(obj);
    /* also with spaces and every control character (each one escapes and comes back) */
    {
        char raw[40], back[64];
        int i;
        for (i = 0; i < 31; i++) raw[i] = (char)(i + 1);
        memcpy(raw + 31, " x/", 3);
        obj = obj_with("p", raw, 34);
        CHECK(obj && json_valid(obj));
        CHECK(obj && webui_json_str(obj, strlen(obj), "p", back, sizeof back) == 1);
        CHECK(obj && memcmp(back, raw, 34) == 0 && back[34] == '\0');
        free(obj);
    }
}

static void surrogate_pair_decodes(void) {
    const char *j = "{\"p\":\"\\ud83d\\ude00\"}";
    char out[16];
    CHECK(webui_json_str(j, strlen(j), "p", out, sizeof out) == 1);
    CHECK(strcmp(out, "\xf0\x9f\x98\x80") == 0);
    /* BMP escapes, upper-case hex, and \/ */
    j = "{\"p\":\"\\u5B8C\\u00e9\\/x\"}";
    CHECK(webui_json_str(j, strlen(j), "p", out, sizeof out) == 1);
    CHECK(strcmp(out, "\xe5\xae\x8c\xc3\xa9/x") == 0);
    /* a lone high or low surrogate, a reversed pair and \u0000 are refused */
    j = "{\"p\":\"\\ud83d\"}";
    CHECK(webui_json_str(j, strlen(j), "p", out, sizeof out) == -1);
    j = "{\"p\":\"\\ude00x\"}";
    CHECK(webui_json_str(j, strlen(j), "p", out, sizeof out) == -1);
    j = "{\"p\":\"\\ude00\\ud83d\"}";
    CHECK(webui_json_str(j, strlen(j), "p", out, sizeof out) == -1);
    j = "{\"p\":\"a\\u0000b\"}";
    CHECK(webui_json_str(j, strlen(j), "p", out, sizeof out) == -1);
    j = "{\"p\":\"\\x\"}";
    CHECK(webui_json_str(j, strlen(j), "p", out, sizeof out) == -1);
}

static void missing_key_is_absent_not_empty(void) {
    const char *j = "{\"a\":\"\",\"n\":{\"path\":\"in\"},\"s\":\"\\\"path\\\":\\\"x\\\"\",\"num\":12,\"t\":true}";
    char out[16];
    uint64_t v = 7;
    int b = 7;
    out[0] = 'x';
    CHECK(webui_json_str(j, strlen(j), "a", out, sizeof out) == 1 && out[0] == '\0');
    CHECK(webui_json_str(j, strlen(j), "path", out, sizeof out) == 0); /* only nested / inside a string */
    CHECK(webui_json_str(j, strlen(j), "b", out, sizeof out) == 0);
    CHECK(webui_json_str(j, strlen(j), "num", out, sizeof out) == -1); /* present, not a string */
    CHECK(webui_json_u64(j, strlen(j), "num", &v) == 1 && v == 12);
    CHECK(webui_json_u64(j, strlen(j), "nope", &v) == 0);
    CHECK(webui_json_u64(j, strlen(j), "a", &v) == -1);
    CHECK(webui_json_bool(j, strlen(j), "t", &b) == 1 && b == 1);
    CHECK(webui_json_bool(j, strlen(j), "num", &b) == -1);
    CHECK(webui_json_bool(j, strlen(j), "f", &b) == 0);
    /* an empty body, a non-object and a truncated object find nothing */
    CHECK(webui_json_str("", 0, "a", out, sizeof out) == 0);
    CHECK(webui_json_str("[\"a\"]", 5, "a", out, sizeof out) == 0);
    CHECK(webui_json_str("{\"a\":\"xy", 8, "a", out, sizeof out) == -1);
    /* the body is not NUL-terminated: n bounds the scan */
    CHECK(webui_json_u64("{\"o\":12}{\"o\":99}", 8, "o", &v) == 1 && v == 12);
    /* integers: negative, fractional, exponent, overflow */
    CHECK(webui_json_u64("{\"o\":-1}", 8, "o", &v) == -1);
    CHECK(webui_json_u64("{\"o\":1.5}", 9, "o", &v) == -1);
    CHECK(webui_json_u64("{\"o\":1e3}", 9, "o", &v) == -1);
    CHECK(webui_json_u64("{\"o\":18446744073709551616}", 26, "o", &v) == -1);
    CHECK(webui_json_u64("{\"o\":18446744073709551615}", 26, "o", &v) == 1 && v == UINT64_MAX);
    /* a value too long for the buffer is refused, not cut */
    CHECK(webui_json_str("{\"a\":\"0123456789abcdef\"}", 24, "a", out, sizeof out) == -1);
}

static void list_result_to_json(void) {
    ava1_fs_entry_t e[3];
    uint8_t enc[4096];
    char out[4096];
    size_t n;
    memset(e, 0, sizeof e);
    e[0].name = (const uint8_t *)"a.txt";
    e[0].name_len = 5;
    e[0].kind = AVA1_ENTRY_FILE;
    e[0].size = 5;
    e[0].has_mtime = 1;
    e[0].mtime = 100;
    e[1].name = (const uint8_t *)"\xe5\xad\x90\xe7\x9b\xae\xe5\xbd\x95"; /* 子目录 */
    e[1].name_len = 9;
    e[1].kind = AVA1_ENTRY_DIR;
    e[1].has_mtime = 1;
    e[1].mtime = 200;
    e[2].name = (const uint8_t *)"l\"q\\";
    e[2].name_len = 4;
    e[2].kind = AVA1_ENTRY_LINK;
    n = encode_list(enc, sizeof enc, e, 3, 13, 1); /* offset 10: scanned to 13 */
    CHECK(n > 0);
    CHECK(webui_list_to_json(enc, n, out, sizeof out) == 3);
    CHECK(strcmp(out,
                 "{\"entries\":[{\"name\":\"a.txt\",\"kind\":\"file\",\"size\":5,\"mtime\":100},"
                 "{\"name\":\"\xe5\xad\x90\xe7\x9b\xae\xe5\xbd\x95\",\"kind\":\"dir\",\"size\":0,\"mtime\":200},"
                 "{\"name\":\"l\\\"q\\\\\",\"kind\":\"link\",\"size\":0,\"mtime\":null}],"
                 "\"more\":true,\"next\":13}") == 0);
    CHECK(json_valid(out));
    /* an empty directory */
    n = encode_list(enc, sizeof enc, e, 0, 0, 0);
    CHECK(webui_list_to_json(enc, n, out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"entries\":[],\"more\":false,\"next\":0}") == 0);
    /* garbage does not decode */
    CHECK(webui_list_to_json((const uint8_t *)"\x01\x02", 2, out, sizeof out) == -1);
    /* other / unknown kinds read "other" */
    e[0].kind = AVA1_ENTRY_OTHER;
    e[1].kind = AVA1_ENTRY_UNKNOWN;
    n = encode_list(enc, sizeof enc, e, 2, 2, 0);
    CHECK(webui_list_to_json(enc, n, out, sizeof out) == 2);
    CHECK(strstr(out, "\"kind\":\"other\",\"size\":5") != NULL);
    CHECK(strstr(out, "\"kind\":\"other\",\"size\":0,\"mtime\":200") != NULL);
}

static void list_json_truncates_at_cap_with_more(void) {
    enum { N = 256 };
    ava1_fs_entry_t *e = calloc(N, sizeof *e);
    char (*names)[201] = calloc(N, sizeof *names);
    uint8_t *enc = malloc(1 << 20);
    char *out = malloc(16384);
    char tail[64];
    size_t n, i, len;
    int got;
    for (i = 0; i < N; i++) {
        memset(names[i], 'n', 200);
        names[i][0] = (char)('a' + i % 26);
        names[i][1] = '"'; /* escaping makes it longer */
        e[i].name = (const uint8_t *)names[i];
        e[i].name_len = 200;
        e[i].kind = AVA1_ENTRY_FILE;
        e[i].size = i;
        e[i].has_mtime = 1;
        e[i].mtime = 1700000000u + i;
    }
    n = encode_list(enc, 1 << 20, e, N, 512 + N, 0); /* offset 512; the helper says no more */
    CHECK(n > 0);
    got = webui_list_to_json(enc, n, out, 16384);
    CHECK(got > 0 && got < N);
    len = strlen(out);
    CHECK(len < 16384);
    CHECK(json_valid(out));
    snprintf(tail, sizeof tail, "],\"more\":true,\"next\":%d}", 512 + got);
    CHECK(len > strlen(tail) && strcmp(out + len - strlen(tail), tail) == 0);
    /* a window too small for even the empty list */
    CHECK(webui_list_to_json(enc, n, out, 16) == -1);
    free(e);
    free(names);
    free(enc);
    free(out);
}

/* ---- the routes' other helpers ---- */

static void escape_makes_valid_json(void) {
    char out[256], buf[300];
    size_t n;
    /* invalid UTF-8: a lone continuation, a truncated sequence, an overlong, a surrogate, > U+10FFFF */
    static const char bad[] = "a\x80" "b\xe5\xae" "c\xc0\xaf" "d\xed\xa0\x80" "e\xf4\x90\x80\x80" "f\xff";
    n = webui_json_escape(out, sizeof out, bad, sizeof bad - 1);
    CHECK(n < sizeof out && n == strlen(out));
    snprintf(buf, sizeof buf, "\"%s\"", out);
    CHECK(json_valid(buf));
    CHECK(strncmp(out, "a\\ufffdb\\ufffd", 14) == 0);
    /* control characters and DEL */
    n = webui_json_escape(out, sizeof out, "\n\t\x01\x1f\x7f", 5);
    CHECK(strcmp(out, "\\n\\t\\u0001\\u001f\x7f") == 0);
    /* snprintf semantics: the full length comes back, the output is cut and terminated */
    n = webui_json_escape(out, 4, "\"\"\"", 3);
    CHECK(n == 6 && strcmp(out, "\\\"") == 0); /* whole escapes only */
    CHECK(webui_json_escape(NULL, 0, "abc", 3) == 3);
    /* a cut never splits an escape or a UTF-8 sequence */
    n = webui_json_escape(out, 3, "a\"", 2);
    CHECK(n == 3 && strcmp(out, "a") == 0);
    n = webui_json_escape(out, 3, "a\xe5\xae\x8c", 4);
    CHECK(n == 4 && strcmp(out, "a") == 0);
}

static int mine_if_first_byte_1(const uint8_t id[16], void *ctx) {
    (void)ctx;
    return id[0] == 1;
}

static void jobs_to_json(void) {
    ava1_job_entry_t e[2];
    uint8_t blob[512], enc[1024];
    ava1_w_t bw, w;
    ava1_job_list_result_t r;
    char out[1024];
    memset(e, 0, sizeof e);
    memset(e[0].job_id, 0xab, 16);
    e[0].job_id[0] = 1;
    e[0].kind = AVA1_JOB_COPY;
    e[0].state = 0;
    e[0].files_done = 1;
    e[0].files_total = 2;
    e[0].bytes_done = 3;
    e[0].bytes_total = 4;
    e[1].kind = 4; /* an operation (AVA1_JOB_OPKIND) */
    e[1].state = 2;
    ava1_w_init(&bw, blob, sizeof blob);
    ava1_job_entry_append(&bw, &e[0]);
    ava1_job_entry_append(&bw, &e[1]);
    memset(&r, 0, sizeof r);
    r.jobs = blob;
    r.jobs_len = (uint32_t)bw.len;
    r.jobs_count = 2;
    ava1_w_init(&w, enc, sizeof enc);
    CHECK(ava1_job_list_result_encode(&r, &w) == 0);
    CHECK(webui_jobs_to_json(enc, w.len, mine_if_first_byte_1, NULL, out, sizeof out) == 2);
    CHECK(strcmp(out,
                 "{\"jobs\":[{\"job\":\"01ababababababababababababababab\",\"kind\":\"copy\",\"state\":\"running\","
                 "\"files_done\":1,\"files_total\":2,\"bytes_done\":3,\"bytes_total\":4,\"mine\":true},"
                 "{\"job\":\"00000000000000000000000000000000\",\"kind\":\"op\",\"state\":\"failed\","
                 "\"files_done\":0,\"files_total\":0,\"bytes_done\":0,\"bytes_total\":0,\"mine\":false}]}") == 0);
    CHECK(json_valid(out));
    CHECK(webui_jobs_to_json(enc, w.len, NULL, NULL, out, 40) == -1);
}

static size_t status_enc(const ava1_status_t *st, uint8_t *enc, size_t cap) {
    ava1_w_t w;
    ava1_w_init(&w, enc, cap);
    CHECK(ava1_status_encode(st, &w) == 0);
    return w.len;
}

static void job_status_to_json(void) {
    ava1_status_t st;
    static uint8_t enc[16384];
    static char out[65536], big[WEBUI_JOB_RESULT_MAX + 2];
    size_t len;
    const char *res = "{\"path\":\"/data/a \\\"b\\\".bin\",\"size\":5,\"hash\":\"ab12\"}";
    char back[256];
    /* running, with the operation's line */
    memset(&st, 0, sizeof st);
    memset(st.job_id, 0x11, 16);
    st.has_state = 1;
    st.state = 0;
    st.bytes_durable = st.bytes_received = 3;
    st.bytes_total = 10;
    st.files_total = 1;
    st.has_current = 1;
    st.current = (const uint8_t *)"unpacking x.exfat";
    st.current_len = 17;
    len = status_enc(&st, enc, sizeof enc);
    CHECK(webui_job_status_to_json(enc, len, out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"job\":\"11111111111111111111111111111111\",\"state\":\"running\",\"files_done\":0,"
                      "\"files_total\":1,\"bytes_done\":3,\"bytes_total\":10,\"message\":\"unpacking x.exfat\","
                      "\"result\":null,\"code\":null}") == 0);
    CHECK(json_valid(out));
    /* done, with a result carried as a string that unescapes back to the operation's JSON */
    st.state = 1;
    st.has_current = 0;
    st.has_result = 1;
    st.result = (const uint8_t *)res;
    st.result_len = (uint32_t)strlen(res);
    len = status_enc(&st, enc, sizeof enc);
    CHECK(webui_job_status_to_json(enc, len, out, sizeof out) == 0);
    CHECK(json_valid(out));
    CHECK(strstr(out, "\"state\":\"done\"") != NULL && strstr(out, "\"message\":null") != NULL);
    CHECK(webui_json_str(out, strlen(out), "result", back, sizeof back) == 1 && strcmp(back, res) == 0);
    /* failed: the cause and the status code */
    memset(&st, 0, sizeof st);
    st.has_state = 1;
    st.state = 2;
    st.has_current = 1;
    st.current = (const uint8_t *)"fs_unpack_destination_exists";
    st.current_len = 28;
    st.has_code = 1;
    st.code = 6;
    len = status_enc(&st, enc, sizeof enc);
    CHECK(webui_job_status_to_json(enc, len, out, sizeof out) == 0);
    CHECK(json_valid(out));
    CHECK(strstr(out, "\"state\":\"failed\"") && strstr(out, "\"message\":\"fs_unpack_destination_exists\"") &&
          strstr(out, "\"code\":6}"));
    /* a result past the cap is left out, never cut */
    memset(big, 'x', sizeof big - 1);
    big[sizeof big - 1] = '\0';
    memset(&st, 0, sizeof st);
    st.has_state = 1;
    st.state = 1;
    st.has_result = 1;
    st.result = (const uint8_t *)big;
    st.result_len = (uint32_t)strlen(big);
    len = status_enc(&st, enc, sizeof enc);
    CHECK(webui_job_status_to_json(enc, len, out, sizeof out) == 0);
    CHECK(strstr(out, "\"result\":null") != NULL);
    /* too small, or not a status */
    CHECK(webui_job_status_to_json(enc, len, out, 40) == -1 && out[0] == '\0');
    CHECK(webui_job_status_to_json((const uint8_t *)"\xff\xff", 2, out, sizeof out) == -1);
}

static void path_job_args(void) {
    char out[512], back[256];
    /* hash / crc32 / unpack / delete: exactly the engine's {"path":...} */
    CHECK(webui_path_job_args(out, sizeof out, "/data/a.bin", NULL, NULL) == 22);
    CHECK(strcmp(out, "{\"path\":\"/data/a.bin\"}") == 0);
    CHECK(webui_path_job_args(out, sizeof out, "/data/a.bin", NULL, "") > 0 && strcmp(out, "{\"path\":\"/data/a.bin\"}") == 0);
    /* chmod's tail */
    CHECK(webui_path_job_args(out, sizeof out, "/data/x", NULL, ",\"mode\":\"0777\"") > 0);
    CHECK(strcmp(out, "{\"path\":\"/data/x\",\"mode\":\"0777\"}") == 0);
    /* unpack's Keep both name; quotes, backslashes and CJK survive */
    CHECK(webui_path_job_args(out, sizeof out, "/mnt/usb0/\xe5\xae\x8c \"q\"\\.ffpfsc", "img (2).exfat", NULL) > 0);
    CHECK(json_valid(out));
    CHECK(webui_json_str(out, strlen(out), "path", back, sizeof back) == 1 &&
          strcmp(back, "/mnt/usb0/\xe5\xae\x8c \"q\"\\.ffpfsc") == 0);
    CHECK(webui_json_str(out, strlen(out), "name", back, sizeof back) == 1 && strcmp(back, "img (2).exfat") == 0);
    /* no path, or no room: nothing */
    CHECK(webui_path_job_args(out, sizeof out, "", NULL, NULL) == -1 && out[0] == '\0');
    CHECK(webui_path_job_args(out, 12, "/data/a.bin", NULL, NULL) == -1 && out[0] == '\0');
    CHECK(webui_path_job_args(out, 23, "/data/a.bin", NULL, NULL) == 22);
    CHECK(webui_path_job_args(out, 22, "/data/a.bin", NULL, NULL) == -1);
}

static void name_checks(void) {
    char long_name[300];
    CHECK(webui_name_ok("a.exfat") && webui_name_ok("img (2).exfat") && webui_name_ok("\xe5\xae\x8c.exfat"));
    CHECK(!webui_name_ok("") && !webui_name_ok(".") && !webui_name_ok("..") && !webui_name_ok("a/b"));
    CHECK(!webui_name_ok("a\nb") && !webui_name_ok(NULL));
    memset(long_name, 'a', 255);
    long_name[255] = '\0';
    CHECK(webui_name_ok(long_name));
    long_name[255] = 'a';
    long_name[256] = '\0';
    CHECK(!webui_name_ok(long_name));
    /* A decompress's output name leaves room for ".part": 250 bytes at most. */
    memset(long_name, 'a', sizeof long_name);
    long_name[WEBUI_UNPACK_NAME_MAX] = '\0';
    CHECK(webui_unpack_name_ok(long_name) && strlen(long_name) + strlen(".part") <= 255);
    long_name[WEBUI_UNPACK_NAME_MAX] = 'a';
    long_name[WEBUI_UNPACK_NAME_MAX + 1] = '\0';
    CHECK(!webui_unpack_name_ok(long_name) && webui_name_ok(long_name));
    CHECK(webui_unpack_name_ok("img (2).exfat") && !webui_unpack_name_ok("..") && !webui_unpack_name_ok("a/b"));
    CHECK(!webui_unpack_name_ok("") && !webui_unpack_name_ok(NULL) && !webui_unpack_name_ok("a\tb"));
}

static void rename_detours(void) {
    char tmp[64];
    /* A case-only rename in one folder: a temporary name beside it. */
    CHECK(webui_rename_detour("/mnt/usb0/Game/a.txt", "/mnt/usb0/Game/A.txt", 0x1234abcdu, tmp, sizeof tmp) == 0);
    CHECK(strcmp(tmp, "/mnt/usb0/Game/.ps5upload-rename-1234abcd") == 0);
    CHECK(webui_rename_detour("/data/Unity.dll", "/data/unity.DLL", 1, tmp, sizeof tmp) == 0 &&
          strcmp(tmp, "/data/.ps5upload-rename-00000001") == 0);
    /* Never across folders (a hard link elsewhere, another mount): refused, tmp empty. */
    CHECK(webui_rename_detour("/mnt/usb0/a/x", "/mnt/usb0/b/X", 1, tmp, sizeof tmp) == -1 && tmp[0] == '\0');
    CHECK(webui_rename_detour("/data/x", "/mnt/usb0/X", 1, tmp, sizeof tmp) == -1);
    CHECK(webui_rename_detour("/data/ab/x", "/data/a/x", 1, tmp, sizeof tmp) == -1);
    /* The same spelling, no name, dot names, no folder. */
    CHECK(webui_rename_detour("/data/x", "/data/x", 1, tmp, sizeof tmp) == -1);
    CHECK(webui_rename_detour("/data/x/", "/data/X/", 1, tmp, sizeof tmp) == -1);
    CHECK(webui_rename_detour("/data/.", "/data/..", 1, tmp, sizeof tmp) == -1);
    CHECK(webui_rename_detour("x", "X", 1, tmp, sizeof tmp) == -1);
    CHECK(webui_rename_detour(NULL, "/data/X", 1, tmp, sizeof tmp) == -1);
    /* Does not fit: refused, never cut short. */
    CHECK(webui_rename_detour("/data/a", "/data/A", 1, tmp, 20) == -1 && tmp[0] == '\0');
    CHECK(webui_rename_detour("/data/a", "/data/A", 1, tmp, 0) == -1);
}

static void hex_ids(void) {
    uint8_t id[16], back[16];
    char hex[33];
    int i;
    for (i = 0; i < 16; i++) id[i] = (uint8_t)(i * 17);
    webui_hex16(id, hex);
    CHECK(strcmp(hex, "00112233445566778899aabbccddeeff") == 0);
    CHECK(webui_parse_hex16(hex, back) == 0 && memcmp(id, back, 16) == 0);
    CHECK(webui_parse_hex16("00112233445566778899AABBCCDDEEFF", back) == 0 && memcmp(id, back, 16) == 0);
    CHECK(webui_parse_hex16("00112233445566778899aabbccddeef", back) == -1);
    CHECK(webui_parse_hex16("00112233445566778899aabbccddeeff0", back) == -1);
    CHECK(webui_parse_hex16("0011223344556677889zaabbccddeeff", back) == -1);
}

static void status_mapping(void) {
    CHECK(webui_http_status_for(AVA1_ERR_PROTOCOL, "bad") == 400);
    CHECK(webui_http_status_for(AVA1_ERR_PATH, "fs_mkdir_path_not_allowed") == 403);
    CHECK(webui_http_status_for(AVA1_ERR_UNKNOWN_JOB, "") == 404);
    CHECK(webui_http_status_for(AVA1_ERR_IO, "fs_list_dir_opendir_errno_2") == 404);
    CHECK(webui_http_status_for(AVA1_ERR_IO, "fs_stat_failed_errno_2") == 404);
    CHECK(webui_http_status_for(AVA1_ERR_IO, "fs_read_stat_failed") == 404);
    CHECK(webui_http_status_for(AVA1_ERR_IO, "fs_list_dir_opendir_errno_20") == 500);
    CHECK(webui_http_status_for(AVA1_ERR_IO, "fs_move_failed") == 500);
    CHECK(webui_http_status_for(AVA1_ERR_EXISTS, "fs_move_exists") == 409);
    CHECK(webui_http_status_for(AVA1_ERR_BUSY, "") == 409);
    CHECK(webui_http_status_for(AVA1_ERR_CROSS_DEVICE, "fs_move_cross_mount") == 409);
    CHECK(webui_http_status_for(AVA1_ERR_INTERNAL, "x") == 500);
}

static void mime_types(void) {
    CHECK(strcmp(webui_mime_for("/a/icon0.png"), "image/png") == 0);
    CHECK(strcmp(webui_mime_for("/a/B.JPG"), "image/jpeg") == 0);
    CHECK(strcmp(webui_mime_for("/a/b.jpeg"), "image/jpeg") == 0);
    CHECK(strcmp(webui_mime_for("/a/b.txt"), "text/plain; charset=utf-8") == 0);
    CHECK(strcmp(webui_mime_for("/a/b.log"), "text/plain; charset=utf-8") == 0);
    CHECK(strcmp(webui_mime_for("/a/config.ini"), "text/plain; charset=utf-8") == 0);
    CHECK(strcmp(webui_mime_for("/a/param.json"), "application/json") == 0);
    /* active-document types are never served as themselves */
    CHECK(strcmp(webui_mime_for("/a/x.xml"), "application/octet-stream") == 0);
    CHECK(strcmp(webui_mime_for("/a/x.XML"), "application/octet-stream") == 0);
    CHECK(strcmp(webui_mime_for("/a/x.html"), "application/octet-stream") == 0);
    CHECK(strcmp(webui_mime_for("/a/x.htm"), "application/octet-stream") == 0);
    CHECK(strcmp(webui_mime_for("/a/x.xhtml"), "application/octet-stream") == 0);
    CHECK(strcmp(webui_mime_for("/a/x.svg"), "application/octet-stream") == 0);
    CHECK(strcmp(webui_mime_for("/a.png/noext"), "application/octet-stream") == 0);
    CHECK(strcmp(webui_mime_for("png"), "application/octet-stream") == 0);
}

static void kv_lines(void) {
    const char *t = "cpu_temp=51\nsoc_temp=0\nmodel=CFI-1015A\nneg=-3\nbad=1x\n";
    char s[32];
    long v = 0;
    CHECK(webui_kv_int(t, strlen(t), "cpu_temp", &v) == 1 && v == 51);
    CHECK(webui_kv_int(t, strlen(t), "soc_temp", &v) == 1 && v == 0);
    CHECK(webui_kv_int(t, strlen(t), "temp", &v) == 0); /* not a suffix match */
    CHECK(webui_kv_int(t, strlen(t), "neg", &v) == 1 && v == -3);
    CHECK(webui_kv_int(t, strlen(t), "bad", &v) == -1);
    CHECK(webui_kv_str(t, strlen(t), "model", s, sizeof s) == 1 && strcmp(s, "CFI-1015A") == 0);
    CHECK(webui_kv_str(t, strlen(t), "serial", s, sizeof s) == 0);
}

static void fan_status(void) {
    char out[512];
    const char *t = "cpu_temp=51\nsoc_temp=0\nfan_duty_pct=19\nproduct_shape=-1\nfan_pinned_c=75\nfan_target_c=75\n"
                    "fan_stock_c=91\nfan_min_c=45\nfan_max_c=85\nfan_restores=2\nfan_last_restore_s=12\n"
                    "fan_guard=ok\nfan_filtered_c=47.3\nfan_servo=00000000005b\n";
    int n = webui_fan_status_json(t, strlen(t), out, sizeof out);
    CHECK(n > 0 && json_valid(out));
    CHECK(strcmp(out, "{\"cpu_c\":51,\"soc_c\":null,\"duty_pct\":19,\"target_c\":75,\"stock_c\":91,"
                      "\"last_restore_s\":12,\"pinned_c\":75,\"min_c\":45,\"max_c\":85,\"restores\":2,"
                      "\"guard\":\"ok\",\"filtered_c\":47.3}") == 0);
    /* missing readings are null, a strange guard is off, a malformed decimal is dropped */
    t = "cpu_temp=0\nfan_duty_pct=-1\nfan_target_c=-1\nfan_stock_c=0\nfan_last_restore_s=-1\nfan_guard=x\n"
        "fan_filtered_c=4.7.\n";
    n = webui_fan_status_json(t, strlen(t), out, sizeof out);
    CHECK(n > 0 && json_valid(out));
    CHECK(strstr(out, "\"cpu_c\":null") && strstr(out, "\"duty_pct\":null") && strstr(out, "\"target_c\":null") &&
          strstr(out, "\"stock_c\":null") && strstr(out, "\"last_restore_s\":null") && strstr(out, "\"guard\":\"off\"") &&
          strstr(out, "\"filtered_c\":null"));
    t = "fan_filtered_c=-1\n";
    CHECK(webui_fan_status_json(t, strlen(t), out, sizeof out) > 0 && strstr(out, "\"filtered_c\":null"));
    t = "fan_filtered_c=1e9\n";
    CHECK(webui_fan_status_json(t, strlen(t), out, sizeof out) > 0 && strstr(out, "\"filtered_c\":null"));
    CHECK(webui_fan_status_json(t, strlen(t), out, 40) == -1);
}

static void focus_running(void) {
    char tid[16];
    uint32_t app = 0;
    const char *j = "{\"ok\":true,\"apis\":{\"x\":true},\"apps\":[{\"pid\":5,\"title_id\":\"NPXS40087\",\"app_id\":7},"
                    "{\"pid\":9,\"title_id\":\"CUSA07842\",\"app_id\":24600},{\"pid\":10,\"title_id\":\"PPSA01234\","
                    "\"app_id\":24601}],\"focus_app_id\":24601,\"focus_rc\":0,\"focus_available\":true,"
                    "\"big_app\":true,\"mini_app\":true,\"big_app_id\":0,\"mini_app_id\":0,\"monotonic_ms\":1}";
    CHECK(webui_focus_running(j, strlen(j), tid, sizeof tid, &app) == 1);
    CHECK(strcmp(tid, "PPSA01234") == 0 && app == 24601);
    /* the dashboard owns the screen: the first game still runs */
    j = "{\"apps\":[{\"title_id\":\"\",\"app_id\":3},{\"title_id\":\"CUSA07842\",\"app_id\":24600}],\"focus_app_id\":7}";
    CHECK(webui_focus_running(j, strlen(j), tid, sizeof tid, &app) == 1);
    CHECK(strcmp(tid, "CUSA07842") == 0 && app == 24600);
    /* no focus flag: big_app_id decides */
    j = "{\"apps\":[{\"title_id\":\"CUSA00001\",\"app_id\":1},{\"title_id\":\"CUSA00002\",\"app_id\":2}],"
        "\"focus_app_id\":-1,\"big_app_id\":2}";
    CHECK(webui_focus_running(j, strlen(j), tid, sizeof tid, &app) == 1);
    CHECK(strcmp(tid, "CUSA00002") == 0 && app == 2);
    j = "{\"apps\":[{\"title_id\":\"NPXS40087\",\"app_id\":7}],\"focus_app_id\":7}";
    CHECK(webui_focus_running(j, strlen(j), tid, sizeof tid, &app) == 0);
    j = "{\"apps\":[],\"focus_app_id\":0}";
    CHECK(webui_focus_running(j, strlen(j), tid, sizeof tid, &app) == 0);
    CHECK(webui_focus_running("nope", 4, tid, sizeof tid, &app) == -1);
}

/* The fallback (nothing game-like on screen) names only a game: never the page's own tile, the
 * browser or a homebrew app, which Close would kill. The focused app never names the tile either. */
static void focus_running_fallback_games_only(void) {
    char tid[16];
    uint32_t app = 0;
    const char *j = "{\"apps\":[{\"title_id\":\"PSUP00001\",\"app_id\":3},{\"title_id\":\"FAKE10101\",\"app_id\":4},"
                    "{\"title_id\":\"PLDM00001\",\"app_id\":5},{\"title_id\":\"WKAL00001\",\"app_id\":6},"
                    "{\"title_id\":\"PPSA01650\",\"app_id\":7}],\"focus_app_id\":-1,\"big_app_id\":0}";
    CHECK(webui_focus_running(j, strlen(j), tid, sizeof tid, &app) == 1);
    CHECK(strcmp(tid, "PPSA01650") == 0 && app == 7);
    /* only non-games run: nothing to close */
    j = "{\"apps\":[{\"title_id\":\"PSUP00001\",\"app_id\":3},{\"title_id\":\"FAKE10101\",\"app_id\":4}],"
        "\"focus_app_id\":-1}";
    CHECK(webui_focus_running(j, strlen(j), tid, sizeof tid, &app) == 0);
    /* the tile focused (the browser it opened owns the screen): not "running", the game behind it is */
    j = "{\"apps\":[{\"title_id\":\"PSUP00001\",\"app_id\":3},{\"title_id\":\"CUSA07842\",\"app_id\":9}],"
        "\"focus_app_id\":3}";
    CHECK(webui_focus_running(j, strlen(j), tid, sizeof tid, &app) == 1);
    CHECK(strcmp(tid, "CUSA07842") == 0 && app == 9);
    /* a focused homebrew app is still what is on screen (the user sees it; pass 0 keeps it) */
    j = "{\"apps\":[{\"title_id\":\"FAKE10101\",\"app_id\":4},{\"title_id\":\"PPSA01650\",\"app_id\":7}],"
        "\"focus_app_id\":4}";
    CHECK(webui_focus_running(j, strlen(j), tid, sizeof tid, &app) == 1);
    CHECK(strcmp(tid, "FAKE10101") == 0 && app == 4);
    CHECK(webui_is_game_title_id("PPSA01650") && webui_is_game_title_id("CUSA07842") &&
          webui_is_game_title_id("PLJS00001") && webui_is_game_title_id("PCSH00001"));
    CHECK(!webui_is_game_title_id("PSUP00001") && !webui_is_game_title_id("NPXS40087") &&
          !webui_is_game_title_id("FAKE10101") && !webui_is_game_title_id("PPSA0165") &&
          !webui_is_game_title_id("PPSA0165X") && !webui_is_game_title_id("PPSA016500") &&
          !webui_is_game_title_id(""));
}

static void array_span(void) {
    const char *j = "{\"n\":{\"events\":[1]},\"events\":[\"a]\",[2],\"b\"],\"x\":3}";
    const char *v = NULL;
    size_t len = 0;
    CHECK(webui_json_array(j, strlen(j), "events", &v, &len) == 1);
    CHECK(v && len == strlen("[\"a]\",[2],\"b\"]") && memcmp(v, "[\"a]\",[2],\"b\"]", len) == 0);
    CHECK(webui_json_array(j, strlen(j), "x", &v, &len) == -1);   /* not an array */
    CHECK(webui_json_array(j, strlen(j), "nope", &v, &len) == 0);
    CHECK(webui_json_array("{\"events\":[\"a\"", 13, "events", &v, &len) == -1); /* cut short */
}

static void unmount_refusal(void) {
    static const char *sys[] = { "/", "/data", "/data/x", "/user", "/user/app", "/system", "/system_ex", "/preinst",
                                 "/dev/lvd0", "/mnt/usb0", "/mnt/ext1" };
    size_t i;
    for (i = 0; i < sizeof sys / sizeof sys[0]; i++) {
        CHECK(webui_unmount_refusal(sys[i], 0) && strcmp(webui_unmount_refusal(sys[i], 0), "system_volume") == 0);
        CHECK(strcmp(webui_unmount_refusal(sys[i], 1), "system_volume") == 0);
    }
    CHECK(webui_unmount_refusal("/mnt/ps5upload/game.exfat", 0) == NULL);
    CHECK(strcmp(webui_unmount_refusal("/mnt/ps5upload/game.exfat", 1), "game_running") == 0);
    CHECK(webui_unmount_refusal("/mnt/shadowmnt/x", 0) == NULL); /* the helper's own consent rule decides */
    CHECK(strcmp(webui_unmount_refusal(NULL, 0), "system_volume") == 0);
}

/* The Games and Processes sections' guards. */
static void app_refusals(void) {
    CHECK(webui_app_refusal("PPSA01650", 0) == NULL);
    CHECK(webui_app_refusal("CUSA07842", 1) == NULL);
    CHECK(strcmp(webui_app_refusal("PSUP00001", 0), "own_tile") == 0);
    CHECK(strcmp(webui_app_refusal("psup00001", 1), "own_tile") == 0);
    CHECK(webui_app_refusal("NPXS40001", 0) == NULL); /* launching a system app is fine */
    CHECK(strcmp(webui_app_refusal("NPXS40001", 1), "system_title") == 0);
    CHECK(strcmp(webui_app_refusal("npxs40001", 1), "system_title") == 0);
    CHECK(strcmp(webui_app_refusal("", 0), "bad_title_id") == 0);
    CHECK(strcmp(webui_app_refusal("PPSA\"0165", 0), "bad_title_id") == 0);
    CHECK(strcmp(webui_app_refusal("PPSA01650/../x", 0), "bad_title_id") == 0);
    CHECK(strcmp(webui_app_refusal(NULL, 0), "bad_title_id") == 0);
}

static void proc_guards(void) {
    const char *j = "{\"ok\":true,\"procs\":["
                    "{\"pid\":1,\"name\":\"init\",\"comm\":\"init\",\"title_id\":\"\",\"app_id\":0,\"memory_mib\":1.0,"
                    "\"threads\":1,\"kind\":\"system\",\"is_self\":false},"
                    "{\"pid\":88,\"name\":\"SceShellUI\",\"comm\":\"SceShellUI\",\"title_id\":\"NPXS40087\",\"app_id\":5,"
                    "\"memory_mib\":300.0,\"threads\":90,\"kind\":\"system\",\"is_self\":false},"
                    "{\"pid\":120,\"name\":\"eboot.bin\",\"comm\":\"eboot.bin\",\"title_id\":\"PPSA01650\",\"app_id\":77,"
                    "\"memory_mib\":5000.5,\"threads\":40,\"kind\":\"app\",\"is_self\":false},"
                    "{\"pid\":130,\"name\":\"kstuff.elf\",\"comm\":\"kstuff.elf\",\"title_id\":\"\",\"app_id\":0,"
                    "\"memory_mib\":2.0,\"threads\":2,\"kind\":\"payload\",\"is_self\":false},"
                    "{\"pid\":140,\"name\":\"ps5upload.elf\",\"comm\":\"ps5upload.elf\",\"title_id\":\"\",\"app_id\":0,"
                    "\"memory_mib\":20.0,\"threads\":30,\"kind\":\"payload\",\"is_self\":true},"
                    "{\"truncated\":true}]}";
    size_t n = strlen(j);
    CHECK(webui_proc_kill_refusal(j, n, 120) == NULL);
    CHECK(webui_proc_kill_refusal(j, n, 130) == NULL);
    CHECK(strcmp(webui_proc_kill_refusal(j, n, 140), "helper") == 0);
    CHECK(strcmp(webui_proc_kill_refusal(j, n, 88), "system") == 0);
    CHECK(strcmp(webui_proc_kill_refusal(j, n, 1), "system") == 0);
    CHECK(strcmp(webui_proc_kill_refusal(j, n, 0), "system") == 0);
    CHECK(strcmp(webui_proc_kill_refusal(j, n, 999), "not_running") == 0);
    CHECK(strcmp(webui_proc_kill_refusal("{\"procs\":[{\"pid\":120", strlen("{\"procs\":[{\"pid\":120"), 120), "not_running") == 0);
    CHECK(strcmp(webui_proc_kill_refusal("nope", 4, 120), "not_running") == 0);

    CHECK(webui_proc_game_running(j, n, "PPSA01650", 0) == 1);
    CHECK(webui_proc_game_running(j, n, "PPSA01650", 1) == 0); /* no OTHER game */
    CHECK(webui_proc_game_running(j, n, "CUSA07842", 0) == 0);
    CHECK(webui_proc_game_running(j, n, "CUSA07842", 1) == 1);
    CHECK(webui_proc_game_running(j, n, "NPXS40087", 0) == 0); /* system apps are not games */
    CHECK(webui_proc_game_running("{\"procs\":[]}", strlen("{\"procs\":[]}"), "X", 1) == 0);
    CHECK(webui_proc_game_running("{\"x\":1}", 7, "X", 1) == -1);
}

static void array_strings(void) {
    const char *j = "{\"a\":[ \"/x\" , \"\\u5b8c \\\"q\\\"\" ],\"e\":[ ],\"m\":[\"ok\",3],\"z\":null,\"s\":\"null\"}";
    const char *v = NULL;
    size_t len = 0, pos = 0;
    char out[64];
    CHECK(webui_json_array(j, strlen(j), "a", &v, &len) == 1);
    CHECK(webui_json_array_str(v, len, &pos, out, sizeof out) == 1 && strcmp(out, "/x") == 0);
    CHECK(webui_json_array_str(v, len, &pos, out, sizeof out) == 1 && strcmp(out, "\xe5\xae\x8c \"q\"") == 0);
    CHECK(webui_json_array_str(v, len, &pos, out, sizeof out) == 0);
    CHECK(webui_json_array_str(v, len, &pos, out, sizeof out) == 0); /* stays ended */
    pos = 0;
    CHECK(webui_json_array(j, strlen(j), "e", &v, &len) == 1 && webui_json_array_str(v, len, &pos, out, sizeof out) == 0);
    pos = 0;
    CHECK(webui_json_array(j, strlen(j), "m", &v, &len) == 1);
    CHECK(webui_json_array_str(v, len, &pos, out, sizeof out) == 1 && strcmp(out, "ok") == 0);
    CHECK(webui_json_array_str(v, len, &pos, out, sizeof out) == -1 && out[0] == '\0'); /* not a string */
    pos = 0;
    CHECK(webui_json_array(j, strlen(j), "a", &v, &len) == 1);
    CHECK(webui_json_array_str(v, len, &pos, out, 2) == -1); /* "/x" needs 3 bytes with the NUL */
    CHECK(webui_json_null(j, strlen(j), "z") == 1 && webui_json_null(j, strlen(j), "s") == 0 &&
          webui_json_null(j, strlen(j), "nope") == 0 && webui_json_null(j, strlen(j), "a") == 0);
}

/* ---- cheats ---- */

static void cheats_status(void) {
    char out[256];
    CHECK(webui_cheats_title_ok("PPSA01650") && webui_cheats_title_ok("CUSA07842") && webui_cheats_title_ok("FAKE1"));
    CHECK(!webui_cheats_title_ok("") && !webui_cheats_title_ok("ABC") && !webui_cheats_title_ok("PPSA0165\"") &&
          !webui_cheats_title_ok("PPSA/0165") && !webui_cheats_title_ok("ABCDEFGHIJKLMNOP") && !webui_cheats_title_ok(NULL));
    const char *j = "{\"enabled\":true,\"patches_last\":2,\"patches_total\":5,\"game_running\":true,"
                    "\"game_title_id\":\"PPSA01650\",\"game_pid\":77}";
    CHECK(webui_cheats_status_to_json(j, strlen(j), out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"engine\":true,\"game\":{\"title_id\":\"PPSA01650\"}}") == 0);
    j = "{\"enabled\":false,\"patches_last\":0,\"patches_total\":0,\"game_running\":false,\"game_title_id\":\"\","
        "\"game_pid\":0}";
    CHECK(webui_cheats_status_to_json(j, strlen(j), out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"engine\":false,\"game\":null}") == 0);
    /* running but no usable title id, or the page's own tile: no game */
    j = "{\"enabled\":false,\"game_running\":true,\"game_title_id\":\"\"}";
    CHECK(webui_cheats_status_to_json(j, strlen(j), out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"engine\":false,\"game\":null}") == 0);
    j = "{\"enabled\":false,\"game_running\":true,\"game_title_id\":\"PSUP00001\"}";
    CHECK(webui_cheats_status_to_json(j, strlen(j), out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"engine\":false,\"game\":null}") == 0);
    /* a title id that is not plain letters and digits is never echoed */
    j = "{\"enabled\":true,\"game_running\":true,\"game_title_id\":\"AB\\\"<x>\"}";
    CHECK(webui_cheats_status_to_json(j, strlen(j), out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"engine\":true,\"game\":null}") == 0);
    /* a paused game (#550) says so; a helper before 9.5.0 never sends "held" */
    j = "{\"enabled\":true,\"game_running\":true,\"game_title_id\":\"CUSA04402\",\"game_pid\":9,\"held\":true}";
    CHECK(webui_cheats_status_to_json(j, strlen(j), out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"engine\":true,\"game\":{\"title_id\":\"CUSA04402\",\"held\":true}}") == 0);
    j = "{\"enabled\":true,\"game_running\":true,\"game_title_id\":\"CUSA04402\",\"held\":false}";
    CHECK(webui_cheats_status_to_json(j, strlen(j), out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"engine\":true,\"game\":{\"title_id\":\"CUSA04402\"}}") == 0);
    CHECK(webui_cheats_status_to_json("nope", 4, out, sizeof out) == -1);
    CHECK(webui_cheats_status_to_json("{\"x\":1}", 7, out, sizeof out) == -1); /* no enabled */
    CHECK(webui_cheats_status_to_json(j, strlen(j), out, 8) == -1);           /* does not fit */
}

static void cheats_mods(void) {
    static char out[4096];
    char err[64];
    const char *j =
        "{\"title_id\":\"PPSA01650\",\"mods\":["
        "{\"index\":0,\"name\":\"Infinite \\\"HP\\\"\",\"desc\":\"\",\"type\":\"checkbox\",\"on\":true},"
        "{\"index\":1,\"name\":\"\xe7\x84\xa1\xe9\x99\x90\",\"desc\":\"UTF-8 kept\",\"type\":\"button\",\"on\":false},"
        "{\"index\":2,\"desc\":\"no name: left out\",\"type\":\"checkbox\",\"on\":false},"
        "{\"index\":3,\"name\":\"Max money\",\"desc\":\"a\\nb\",\"type\":\"checkbox\",\"on\":false}]}";
    CHECK(webui_cheats_mods_to_json(j, strlen(j), out, sizeof out, err, sizeof err) == 3);
    CHECK(json_valid(out));
    CHECK(strcmp(out, "{\"mods\":["
                      "{\"index\":0,\"name\":\"Infinite \\\"HP\\\"\",\"desc\":\"\",\"button\":false,\"on\":true},"
                      "{\"index\":1,\"name\":\"\xe7\x84\xa1\xe9\x99\x90\",\"desc\":\"UTF-8 kept\",\"button\":true,\"on\":false},"
                      "{\"index\":3,\"name\":\"Max money\",\"desc\":\"a\\nb\",\"button\":false,\"on\":false}],"
                      "\"truncated\":false}") == 0);
    /* an XML game patch for another game version keeps its mark (#527) */
    j = "{\"title_id\":\"PPSA01339\",\"mods\":[{\"index\":0,\"name\":\"Debug Menu (01.000.000)\",\"desc\":\"by illusion\","
        "\"type\":\"checkbox\",\"on\":false,\"format\":\"xml\",\"other_version\":true}]}";
    CHECK(webui_cheats_mods_to_json(j, strlen(j), out, sizeof out, err, sizeof err) == 1);
    CHECK(json_valid(out));
    CHECK(strstr(out, "\"button\":false,\"other_version\":true,\"on\":false}") != NULL);
    /* what happened to it in the running game is passed on (#627); an unknown word is not */
    j = "{\"title_id\":\"CUSA04402\",\"mods\":[{\"index\":8,\"name\":\"60 FPS (01.18)\",\"desc\":\"\","
        "\"type\":\"checkbox\",\"on\":true,\"format\":\"xml\",\"applied\":\"failed\","
        "\"applied_reason\":\"this patch is for game version 01.17; the game is \\\"01.18\\\"\"},"
        "{\"index\":9,\"name\":\"B\",\"desc\":\"\",\"type\":\"checkbox\",\"on\":true,\"applied\":\"waiting\"},"
        "{\"index\":10,\"name\":\"C\",\"desc\":\"\",\"type\":\"checkbox\",\"on\":true,\"applied\":\"bogus\","
        "\"applied_reason\":\"x\"}]}";
    CHECK(webui_cheats_mods_to_json(j, strlen(j), out, sizeof out, err, sizeof err) == 3);
    CHECK(json_valid(out));
    CHECK(strstr(out, "\"applied\":\"failed\",\"applied_reason\":\"this patch is for game version 01.17; the game is \\\"01.18\\\"\",\"on\":true}") != NULL);
    CHECK(strstr(out, "\"name\":\"B\",\"desc\":\"\",\"button\":false,\"applied\":\"waiting\",\"on\":true}") != NULL);
    CHECK(strstr(out, "bogus") == NULL && strstr(out, "\"name\":\"C\",\"desc\":\"\",\"button\":false,\"on\":true}") != NULL);
    /* the helper's "no cheat files" answer: an empty list, not an error */
    j = "{\"mods\":[],\"error\":\"no cheat files found\"}";
    CHECK(webui_cheats_mods_to_json(j, strlen(j), out, sizeof out, err, sizeof err) == 0);
    CHECK(strcmp(out, "{\"mods\":[],\"truncated\":false}") == 0);
    /* any other error is one */
    j = "{\"mods\":[],\"error\":\"oom\"}";
    CHECK(webui_cheats_mods_to_json(j, strlen(j), out, sizeof out, err, sizeof err) == -1);
    CHECK(strcmp(err, "oom") == 0);
    CHECK(webui_cheats_mods_to_json("{\"x\":1}", 7, out, sizeof out, err, sizeof err) == -1);
    /* the helper's 64 KiB reply cut mid-entry: the whole entries before the cut, truncated set */
    j = "{\"title_id\":\"PPSA01650\",\"mods\":[{\"index\":0,\"name\":\"A\",\"desc\":\"\",\"type\":\"checkbox\",\"on\":false},"
        "{\"index\":1,\"name\":\"B";
    CHECK(webui_cheats_mods_to_json(j, strlen(j), out, sizeof out, err, sizeof err) == 1);
    CHECK(json_valid(out));
    CHECK(strstr(out, "\"truncated\":true}") != NULL);
    /* an output too small for every entry keeps the ones that fit and says truncated */
    j = "{\"mods\":[{\"index\":0,\"name\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\",\"desc\":\"\",\"type\":\"checkbox\",\"on\":false},"
        "{\"index\":1,\"name\":\"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB\",\"desc\":\"\",\"type\":\"checkbox\",\"on\":false}]}";
    CHECK(webui_cheats_mods_to_json(j, strlen(j), out, 140, err, sizeof err) == 1);
    CHECK(json_valid(out));
    CHECK(strstr(out, "AAAA") && !strstr(out, "BBBB") && strstr(out, "\"truncated\":true}"));
    CHECK(webui_cheats_mods_to_json(j, strlen(j), out, 10, err, sizeof err) == -1);
}

static void cheats_toggle_errors(void) {
    const char *c = "{\"ok\":false,\"err\":\"no game is currently running\"}";
    CHECK(strcmp(webui_cheats_toggle_error(c, strlen(c)), "game_not_running") == 0);
    c = "{\"ok\":false,\"err\":\"running game (PPSA00001) does not match cheat target (PPSA01650)\"}";
    CHECK(strcmp(webui_cheats_toggle_error(c, strlen(c)), "game_not_running") == 0);
    c = "{\"ok\":false,\"err\":\"no cheat file for PPSA01650. Drop a .json/.shn/.mc4 into /data/etaHEN/cheats\"}";
    CHECK(strcmp(webui_cheats_toggle_error(c, strlen(c)), "no_cheats") == 0);
    c = "{\"ok\":false,\"err\":\"cheat index out of range\"}";
    CHECK(strcmp(webui_cheats_toggle_error(c, strlen(c)), "bad_index") == 0);
    c = "{\"ok\":false,\"err\":\"module \\\"x.prx\\\" is not loaded in the target process\"}";
    CHECK(strcmp(webui_cheats_toggle_error(c, strlen(c)), "module_missing") == 0);
    c = "{\"ok\":false,\"err\":\"pt_attach failed: another debugger or cheat tool is attached to the game (errno=16)\"}";
    CHECK(strcmp(webui_cheats_toggle_error(c, strlen(c)), "game_busy") == 0);
    c = "{\"ok\":false,\"err\":\"pt_attach failed (errno=1)\"}";
    CHECK(strcmp(webui_cheats_toggle_error(c, strlen(c)), "toggle_failed") == 0);
    CHECK(strcmp(webui_cheats_toggle_error(NULL, 0), "toggle_failed") == 0);
    c = "{\"ok\":false,\"err\":\"this patch is for game version 01.000.000; the game is 01.004.000\"}";
    CHECK(strcmp(webui_cheats_toggle_error(c, strlen(c)), "wrong_version") == 0);
    c = "{\"ok\":false,\"err\":\"line 3: its pattern is not in the game (another game version?); nothing was written\"}";
    CHECK(strcmp(webui_cheats_toggle_error(c, strlen(c)), "wrong_version") == 0);
    CHECK(strcmp(webui_cheats_toggle_error("busy", 4), "toggle_failed") == 0);
}

static int hide_secret(const char *path, void *ctx) {
    (void)ctx;
    return strncmp(path, "/data/secret", 12) == 0;
}

static void index_codec(void) {
    static char out[4096];
    const char *roots[] = { "/data", "/mnt/ext0", "/mnt/usb0" };
    const char *bad[] = { "/data", "/a\"b" };
    char long_root[300];
    const char *j;
    /* roots and queries: nothing the handlers' quote-to-quote scan would misread */
    CHECK(webui_index_root_ok("/data"));
    CHECK(webui_index_root_ok("/mnt/usb0/Spiele \xc3\xa4"));
    CHECK(!webui_index_root_ok("data"));
    CHECK(!webui_index_root_ok(""));
    CHECK(!webui_index_root_ok("/a\"b"));
    CHECK(!webui_index_root_ok("/a\\b"));
    CHECK(!webui_index_root_ok("/a\nb"));
    memset(long_root, 'a', sizeof long_root);
    long_root[0] = '/';
    long_root[256] = '\0';
    CHECK(!webui_index_root_ok(long_root)); /* 256 bytes: the handler keeps < 256 */
    long_root[255] = '\0';
    CHECK(webui_index_root_ok(long_root));
    CHECK(webui_index_query_ok("*.pkg"));
    CHECK(webui_index_query_ok("eboot"));
    CHECK(!webui_index_query_ok(""));
    CHECK(!webui_index_query_ok("a\"b"));
    CHECK(!webui_index_query_ok("a\\b"));
    CHECK(!webui_index_query_ok("a\tb"));
    /* runs of '*' collapse to one */
    {
        char q[32];
        strcpy(q, "**a***b*?**");
        webui_index_collapse_stars(q);
        CHECK(strcmp(q, "*a*b*?*") == 0);
        strcpy(q, "abc");
        webui_index_collapse_stars(q);
        CHECK(strcmp(q, "abc") == 0);
    }
    /* INDEX_START's body */
    CHECK(webui_index_start_body(out, sizeof out, roots, 3) > 0);
    CHECK(strcmp(out, "{\"roots\":[\"/data\",\"/mnt/ext0\",\"/mnt/usb0\"]}") == 0);
    CHECK(json_valid(out));
    CHECK(webui_index_start_body(out, sizeof out, bad, 2) == -1 && out[0] == '\0');
    CHECK(webui_index_start_body(out, sizeof out, roots, 0) == -1);
    CHECK(webui_index_start_body(out, sizeof out, roots, 9) == -1);
    CHECK(webui_index_start_body(out, 20, roots, 3) == -1 && out[0] == '\0');
    /* status: the helper's reply plus what the page knows */
    j = "{\"phase\":\"building\",\"files\":1234,\"truncated\":false,\"started_at\":1700000000,\"completed_at\":0}";
    CHECK(webui_index_status_to_json(j, strlen(j), roots, 2, 0, out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"phase\":\"building\",\"files\":1234,\"truncated\":false,\"cancelled\":false,"
                      "\"roots\":[\"/data\",\"/mnt/ext0\"]}") == 0);
    j = "{\"phase\":\"ready\",\"files\":200000,\"truncated\":true,\"started_at\":1,\"completed_at\":2}";
    CHECK(webui_index_status_to_json(j, strlen(j), NULL, 0, 1, out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"phase\":\"ready\",\"files\":200000,\"truncated\":true,\"cancelled\":true,\"roots\":null}") == 0);
    j = "{\"phase\":\"weird\",\"files\":1}";
    CHECK(webui_index_status_to_json(j, strlen(j), NULL, 0, 0, out, sizeof out) == -1);
    CHECK(webui_index_status_to_json("nope", 4, NULL, 0, 0, out, sizeof out) == -1);
    j = "{\"phase\":\"idle\",\"files\":0,\"truncated\":false,\"started_at\":0,\"completed_at\":0}";
    CHECK(webui_index_status_to_json(j, strlen(j), roots, 3, 0, out, 30) == -1 && out[0] == '\0');
    /* search results: escaped paths, scope, hidden paths, the helper's limit */
    j = "{\"results\":[{\"path\":\"/data/games/A \\\"x\\\".pkg\",\"size\":10},"
        "{\"path\":\"/data/secret/key.pkg\",\"size\":2},"
        "{\"path\":\"/data/gamesX/b.pkg\",\"size\":3},"
        "{\"path\":\"/mnt/usb0/c.pkg\",\"size\":4}]}";
    CHECK(webui_index_results_to_json(j, strlen(j), NULL, 100, 200, hide_secret, NULL, out, sizeof out) == 3);
    CHECK(json_valid(out));
    CHECK(strcmp(out, "{\"results\":[{\"path\":\"/data/games/A \\\"x\\\".pkg\",\"size\":10},"
                      "{\"path\":\"/data/gamesX/b.pkg\",\"size\":3},{\"path\":\"/mnt/usb0/c.pkg\",\"size\":4}],"
                      "\"truncated\":false}") == 0);
    /* under a folder: "/data/games" is not "/data/gamesX" */
    CHECK(webui_index_results_to_json(j, strlen(j), "/data/games/", 100, 200, hide_secret, NULL, out, sizeof out) == 1);
    CHECK(strstr(out, "A \\\"x\\\".pkg") && !strstr(out, "gamesX"));
    CHECK(webui_index_results_to_json(j, strlen(j), "/", 100, 200, NULL, NULL, out, sizeof out) == 4);
    /* the page's own maximum, and the helper's limit reached: both say there may be more */
    CHECK(webui_index_results_to_json(j, strlen(j), NULL, 2, 200, NULL, NULL, out, sizeof out) == 2);
    CHECK(strstr(out, "\"truncated\":true}") != NULL);
    CHECK(webui_index_results_to_json(j, strlen(j), NULL, 100, 4, NULL, NULL, out, sizeof out) == 4);
    CHECK(strstr(out, "\"truncated\":true}") != NULL);
    /* the helper's own truncated flag */
    j = "{\"results\":[{\"path\":\"/data/a\",\"size\":1}],\"truncated\":true}";
    CHECK(webui_index_results_to_json(j, strlen(j), NULL, 100, 200, NULL, NULL, out, sizeof out) == 1);
    CHECK(strstr(out, "\"truncated\":true}") != NULL);
    /* a cut reply keeps the whole entries */
    j = "{\"results\":[{\"path\":\"/data/a\",\"size\":1},{\"path\":\"/data/b";
    CHECK(webui_index_results_to_json(j, strlen(j), NULL, 100, 200, NULL, NULL, out, sizeof out) == 1);
    CHECK(json_valid(out) && strstr(out, "\"truncated\":true}"));
    /* an entry without a usable path or size is left out; an empty list is fine */
    j = "{\"results\":[{\"path\":\"rel\",\"size\":1},{\"path\":\"/x\"},{\"size\":3}]}";
    CHECK(webui_index_results_to_json(j, strlen(j), NULL, 100, 200, NULL, NULL, out, sizeof out) == 0);
    CHECK(strcmp(out, "{\"results\":[],\"truncated\":false}") == 0);
    CHECK(webui_index_results_to_json("{\"err\":\"oom\"}", 13, NULL, 100, 200, NULL, NULL, out, sizeof out) == -1);
    /* a small output keeps what fits and says so */
    j = "{\"results\":[{\"path\":\"/data/aaaaaaaaaaaaaaaaaaaa\",\"size\":1},{\"path\":\"/data/bbbbbbbbbbbbbbbbbbbb\",\"size\":2}]}";
    CHECK(webui_index_results_to_json(j, strlen(j), NULL, 100, 200, NULL, NULL, out, 90) == 1);
    CHECK(json_valid(out) && strstr(out, "aaaa") && !strstr(out, "bbbb") && strstr(out, "\"truncated\":true}"));
    CHECK(webui_index_results_to_json(j, strlen(j), NULL, 100, 200, NULL, NULL, out, 20) == -1);
}

int main(void) {
    unicode_path_round_trips();
    array_span();
    array_strings();
    unmount_refusal();
    app_refusals();
    proc_guards();
    surrogate_pair_decodes();
    missing_key_is_absent_not_empty();
    list_result_to_json();
    list_json_truncates_at_cap_with_more();
    escape_makes_valid_json();
    jobs_to_json();
    job_status_to_json();
    path_job_args();
    name_checks();
    rename_detours();
    hex_ids();
    status_mapping();
    mime_types();
    kv_lines();
    fan_status();
    focus_running();
    focus_running_fallback_games_only();
    cheats_status();
    cheats_mods();
    cheats_toggle_errors();
    index_codec();
    if (failures) {
        fprintf(stderr, "webui_codec_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("webui_codec_selftest: all passed\n");
    return 0;
}
