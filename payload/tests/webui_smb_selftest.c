/* Host self-test: the console page's network-share routes (src/webui_smb.c) and saved-share storage
 * (src/smb_shares.c), against a fake SMB daemon. Checks that a password never comes back to the page
 * or sits in the file as text, that an edit without a password keeps the saved one, and that copies
 * are held to the helper's own read / write policy. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "ava1_data.h"
#include "path_policy.h"
#include "smb_client.h"
#include "smb_shares.h"
#include "smbd_json.h"
#include "webui_smb.h"

static int g_fail;
#define CHECK(c)                                                         \
    do {                                                                 \
        if (!(c)) {                                                      \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); \
            g_fail++;                                                    \
        }                                                                \
    } while (0)

/* ---- stubs ---- */

static int g_status;
static char g_body[65536];

void webui_send(int fd, int status, const char *ctype, const void *body, size_t len) {
    (void)fd;
    (void)ctype;
    g_status = status;
    if (len >= sizeof g_body) len = sizeof g_body - 1;
    memcpy(g_body, body, len);
    g_body[len] = '\0';
}

void webui_send_json(int fd, int status, const char *json) { webui_send(fd, status, "application/json", json, strlen(json)); }

static char g_req[65536];
static const char *g_next_reply; /* NULL: a canned answer by op */
static int g_daemon_up = 1;

unsigned char smbd_elf[1];
unsigned int smbd_elf_len = 0;

int smbc_request(const char *fields, size_t len, int timeout_ms, int may_start, char **reply, char *err, size_t err_cap) {
    char op[16] = "";
    char wrapped[65600];
    const char *r;
    (void)timeout_ms;
    if (len >= sizeof g_req) len = sizeof g_req - 1;
    memcpy(g_req, fields, len);
    g_req[len] = '\0';
    if (!g_daemon_up && !may_start) {
        snprintf(err, err_cap, "smb_daemon_not_running");
        return -1;
    }
    snprintf(wrapped, sizeof wrapped, "{%s}", g_req);
    sj_str(wrapped, strlen(wrapped), "op", op, sizeof op);
    if (g_next_reply) r = g_next_reply;
    else if (!strcmp(op, "probe")) r = "{\"ok\":true,\"dialect\":\"3.1.1\",\"free\":100}";
    else if (!strcmp(op, "list")) r = "{\"entries\":[{\"name\":\"游戏\",\"kind\":\"dir\",\"size\":0,\"mtime\":1}],\"more\":false,\"next\":1,\"total\":1}";
    else if (!strcmp(op, "stat")) r = "{\"error\":\"not_found\"}";
    else if (!strcmp(op, "copy") || !strcmp(op, "remove")) r = "{\"job\":\"0123456789abcdef0123456789abcdef\"}";
    else if (!strcmp(op, "shares")) r = "{\"shares\":[{\"name\":\"Media\",\"remark\":\"\"}]}";
    else r = "{\"ok\":true}";
    *reply = strdup(r);
    return 0;
}

static int allow_write(const char *p) { return strncmp(p, "/data/", 6) == 0 || strncmp(p, "/mnt/usb0/", 10) == 0; }
static int allow_read(const char *p, int unsafe) {
    (void)unsafe;
    return allow_write(p) || strncmp(p, "/system/", 8) == 0;
}
static ava1_data_cfg_t g_cfg;
const ava1_data_cfg_t *ava1_data_cfg(void) { return &g_cfg; }
int path_in_protected(const char *p) { return strncmp(p, "/data/ps5upload/ava", 19) == 0; }
int path_tree_op_refused(const char *p) { return path_in_protected(p) || !strcmp(p, "/data") || !strcmp(p, "/"); }

/* ---- helpers ---- */

static char g_dir[256], g_file[300], g_key[300];

static void call(void (*fn)(const char *, size_t, uint8_t *, int), const char *body) {
    g_status = 0;
    g_body[0] = '\0';
    fn(body, strlen(body), NULL, 1);
}

static int file_has(const char *needle) {
    char buf[65536];
    FILE *f = fopen(g_file, "rb");
    size_t n;
    if (!f) return 0;
    n = fread(buf, 1, sizeof buf - 1, f);
    fclose(f);
    buf[n] = '\0';
    return strstr(buf, needle) != NULL;
}

static void id_of(const char *json, char id[16]) {
    const char *p = strstr(json, "\"id\":\"");
    id[0] = '\0';
    if (p) {
        memcpy(id, p + 6, 8);
        id[8] = '\0';
    }
}

/* ---- tests ---- */

static void t_save_list_remove(void) {
    char id[16], body[512];
    call(webui_smb_save, "{\"server\":\"smb://192.168.1.10/Media/Games\",\"share\":\"\",\"user\":\"bob\","
                         "\"password\":\"hunter2-秘密\",\"domain\":\"\",\"name\":\"\"}");
    CHECK(g_status == 200);
    CHECK(strstr(g_body, "\"has_password\":true") != NULL);
    CHECK(strstr(g_body, "hunter2") == NULL);
    CHECK(strstr(g_body, "\"share\":\"Media\"") != NULL);
    CHECK(strstr(g_body, "\"start_path\":\"Games\"") != NULL);
    CHECK(strstr(g_body, "\"dialect\":\"3.1.1\"") != NULL);
    CHECK(strstr(g_req, "\"password\":\"hunter2-秘密\"") != NULL); /* the probe signed in with it */
    CHECK(strstr(g_req, "\"path\":\"Games\"") != NULL);
    CHECK(!file_has("hunter2"));
    CHECK(file_has("\"secret\":\""));
    {
        struct stat st;
        CHECK(stat(g_file, &st) == 0 && (st.st_mode & 0077) == 0);
        CHECK(stat(g_key, &st) == 0 && (st.st_mode & 0077) == 0);
    }
    id_of(g_body, id);
    CHECK(strlen(id) == 8);

    call(webui_smb_shares, "{}");
    CHECK(g_status == 200 && strstr(g_body, id) && !strstr(g_body, "hunter2"));
    CHECK(strstr(g_body, "\"path\":\"smb://") != NULL);

    /* edit without a password: the saved one is used for the sign-in and kept */
    snprintf(body, sizeof body,
             "{\"id\":\"%s\",\"server\":\"192.168.1.10\",\"share\":\"Media\",\"user\":\"bob\",\"name\":\"NAS\"}", id);
    call(webui_smb_save, body);
    CHECK(g_status == 200 && strstr(g_body, "\"name\":\"NAS\"") && strstr(g_body, "\"has_password\":true"));
    CHECK(strstr(g_req, "\"password\":\"hunter2-秘密\"") != NULL);
    /* another user: the old password is not carried over */
    snprintf(body, sizeof body, "{\"id\":\"%s\",\"server\":\"192.168.1.10\",\"share\":\"Media\",\"user\":\"eve\"}", id);
    call(webui_smb_save, body);
    CHECK(g_status == 200 && strstr(g_body, "\"has_password\":false"));
    CHECK(strstr(g_req, "\"password\":\"\"") != NULL);

    /* a refused sign-in: 403 (never 401), nothing saved */
    g_next_reply = "{\"error\":\"smb_auth\",\"detail\":\"STATUS_LOGON_FAILURE\"}";
    call(webui_smb_save, "{\"server\":\"10.0.0.9\",\"share\":\"x\",\"user\":\"a\",\"password\":\"b\"}");
    g_next_reply = NULL;
    CHECK(g_status == 403 && strstr(g_body, "smb_auth"));
    call(webui_smb_shares, "{}");
    CHECK(strstr(g_body, "10.0.0.9") == NULL);

    /* refused fields */
    call(webui_smb_save, "{\"server\":\"smb://u:p@nas/x\",\"share\":\"x\"}");
    CHECK(g_status == 400 && strstr(g_body, "bad_host"));
    call(webui_smb_save, "{\"server\":\"nas\",\"share\":\"IPC$\"}");
    CHECK(g_status == 400 && strstr(g_body, "bad_share"));
    call(webui_smb_save, "{\"server\":\"nas\",\"share\":\"x\",\"start_path\":\"../etc\"}");
    CHECK(g_status == 400 && strstr(g_body, "bad_path"));
    call(webui_smb_save, "{\"id\":\"deadbeef\",\"server\":\"nas\",\"share\":\"x\"}");
    CHECK(g_status == 404);

    snprintf(body, sizeof body, "{\"id\":\"%s\"}", id);
    call(webui_smb_remove, body);
    CHECK(g_status == 200);
    call(webui_smb_remove, body);
    CHECK(g_status == 404);
    call(webui_smb_shares, "{}");
    CHECK(strcmp(g_body, "{\"shares\":[]}") == 0);
}

/* The PC app's smb.shares_* and the Library's scan route (#498). */
static void t_pc_and_library(void) {
    char *out = malloc(65536), id[16], body[512];
    int n;
    n = webui_smb_pc_save("{\"server\":\"nas\",\"share\":\"Games\",\"user\":\"amy\",\"password\":\"pw-秘\",\"start_path\":\"PS5\"}",
                          strlen("{\"server\":\"nas\",\"share\":\"Games\",\"user\":\"amy\",\"password\":\"pw-秘\",\"start_path\":\"PS5\"}"), out,
                          65536);
    CHECK(n > 0 && strncmp(out, "{\"ok\":true,\"share\":{", 20) == 0 && strstr(out, "\"dialect\":\"3.1.1\""));
    CHECK(strstr(out, "pw-") == NULL && strstr(out, "\"has_password\":true"));
    id_of(out, id);
    CHECK(strlen(id) == 8);
    n = webui_smb_pc_list(out, 65536);
    CHECK(n > 0 && strstr(out, "{\"ok\":true,\"shares\":[") == out && strstr(out, id) && !strstr(out, "pw-") &&
          strstr(out, "\"max\":16"));
    /* a refused sign-in: the daemon's token (and detail) as the method's refusal */
    g_next_reply = "{\"error\":\"smb_auth\",\"detail\":\"STATUS_LOGON_FAILURE\"}";
    n = webui_smb_pc_save("{\"server\":\"nas\",\"share\":\"x\"}", 29, out, 65536);
    g_next_reply = NULL;
    CHECK(n > 0 && strstr(out, "\"ok\":false") && strstr(out, "\"err\":\"smb_auth\"") && strstr(out, "LOGON_FAILURE"));
    n = webui_smb_pc_save("{\"server\":\"u:p@nas\",\"share\":\"x\"}", 32, out, 65536);
    CHECK(n > 0 && strstr(out, "\"err\":\"bad_host\""));

    /* the library scan: a share walks its start folder with its sign-in; the console its drives */
    snprintf(body, sizeof body, "{\"source\":\"%s\"}", id);
    g_next_reply = "{\"job\":\"0123456789abcdef0123456789abcdef\"}";
    call(webui_library_scan, body);
    CHECK(g_status == 200 && strstr(g_body, "\"job\""));
    CHECK(strstr(g_req, "\"op\":\"scan\"") && strstr(g_req, "\"path\":\"PS5\"") && strstr(g_req, "\"password\":\"pw-秘\"") &&
          strstr(g_req, "\"share\":\"Games\""));
    call(webui_library_scan, "{\"source\":\"console\"}");
    CHECK(g_status == 200 && strstr(g_req, "\"local\":true") && !strstr(g_req, "password"));
    g_next_reply = NULL;
    call(webui_library_scan, "{\"source\":\"deadbeef\"}");
    CHECK(g_status == 404 && strstr(g_body, "unknown_share"));
    call(webui_library_scan, "{\"source\":\"../etc\"}");
    CHECK(g_status == 404);
    call(webui_library_scan, "{}");
    CHECK(g_status == 400 && strstr(g_body, "bad_source"));

    snprintf(body, sizeof body, "{\"id\":\"%s\"}", id);
    n = webui_smb_pc_remove(body, strlen(body), out, 65536);
    CHECK(n > 0 && strcmp(out, "{\"ok\":true}") == 0);
    n = webui_smb_pc_remove(body, strlen(body), out, 65536);
    CHECK(n > 0 && strstr(out, "\"err\":\"unknown_share\""));
    free(out);
}

static void t_paths_and_copy(void) {
    char id[16], body[512];
    call(webui_smb_save, "{\"server\":\"nas\",\"share\":\"Media\",\"user\":\"\",\"password\":\"\"}");
    CHECK(g_status == 200);
    id_of(g_body, id);

    snprintf(body, sizeof body, "{\"path\":\"smb://%s/游戏\",\"offset\":0,\"limit\":50}", id);
    call(webui_smb_list, body);
    CHECK(g_status == 200 && strstr(g_body, "游戏"));
    CHECK(strstr(g_req, "\"op\":\"list\"") && strstr(g_req, "\"path\":\"游戏\"") && strstr(g_req, "\"limit\":50"));
    call(webui_smb_list, "{\"path\":\"smb://00000000/\"}");
    CHECK(g_status == 404 && strstr(g_body, "unknown_share"));
    call(webui_smb_list, "{\"path\":\"/data\"}");
    CHECK(g_status == 400);
    snprintf(body, sizeof body, "{\"path\":\"smb://%s/a/../b\"}", id);
    call(webui_smb_list, body);
    CHECK(g_status == 400);

    snprintf(body, sizeof body, "{\"path\":\"smb://%s/x.bin\",\"missing_ok\":true}", id);
    call(webui_smb_stat, body);
    CHECK(g_status == 200 && strcmp(g_body, "{\"missing\":true}") == 0);
    snprintf(body, sizeof body, "{\"path\":\"smb://%s/x.bin\"}", id);
    call(webui_smb_stat, body);
    CHECK(g_status == 404);

    /* down: to an allowed console folder */
    snprintf(body, sizeof body, "{\"src\":\"smb://%s/Games/中文\",\"dest\":\"/data/ps5upload-smb-test/中文\"}", id);
    call(webui_smb_copy, body);
    CHECK(g_status == 200 && strstr(g_body, "\"job\""));
    CHECK(strstr(g_req, "\"dir\":\"down\"") && strstr(g_req, "\"src\":\"Games/中文\"") &&
          strstr(g_req, "\"dest\":\"/data/ps5upload-smb-test/中文\"") && strstr(g_req, "\"overwrite\":false"));
    /* up: from a readable console path */
    snprintf(body, sizeof body, "{\"src\":\"/system/x\",\"dest\":\"smb://%s/x\",\"overwrite\":true}", id);
    call(webui_smb_copy, body);
    CHECK(g_status == 200 && strstr(g_req, "\"dir\":\"up\"") && strstr(g_req, "\"overwrite\":true"));
    /* refused */
    snprintf(body, sizeof body, "{\"src\":\"smb://%s/x\",\"dest\":\"/system/x\"}", id);
    call(webui_smb_copy, body);
    CHECK(g_status == 403 && strstr(g_body, "path_not_allowed"));
    snprintf(body, sizeof body, "{\"src\":\"smb://%s/x\",\"dest\":\"/data/ps5upload/ava/x\"}", id);
    call(webui_smb_copy, body);
    CHECK(g_status == 403);
    snprintf(body, sizeof body, "{\"src\":\"smb://%s/x\",\"dest\":\"/data/../system/x\"}", id);
    call(webui_smb_copy, body);
    CHECK(g_status == 403);
    snprintf(body, sizeof body, "{\"src\":\"smb://%s/x\",\"dest\":\"smb://%s/y\"}", id, id);
    call(webui_smb_copy, body);
    CHECK(g_status == 400);
    call(webui_smb_copy, "{\"src\":\"/data/a\",\"dest\":\"/data/b\"}");
    CHECK(g_status == 400);
    snprintf(body, sizeof body, "{\"src\":\"smb://%s/\",\"dest\":\"/data/all\"}", id);
    call(webui_smb_copy, body);
    CHECK(g_status == 400);

    snprintf(body, sizeof body, "{\"path\":\"smb://%s/old\"}", id);
    call(webui_smb_delete, body);
    CHECK(g_status == 200 && strstr(g_req, "\"op\":\"remove\""));
    snprintf(body, sizeof body, "{\"path\":\"smb://%s/\"}", id);
    call(webui_smb_delete, body);
    CHECK(g_status == 400);

    /* a Replace's swap: inside one folder of one share */
    snprintf(body, sizeof body, "{\"from\":\"smb://%s/Games/G\",\"to\":\"smb://%s/Games/G.ps5u-old\"}", id, id);
    call(webui_smb_rename, body);
    CHECK(g_status == 200 && strstr(g_req, "\"op\":\"rename\"") && strstr(g_req, "\"path\":\"Games/G\"") &&
          strstr(g_req, "\"to\":\"Games/G.ps5u-old\""));
    snprintf(body, sizeof body, "{\"from\":\"smb://%s/G\",\"to\":\"smb://%s/G.ps5u-new\"}", id, id);
    call(webui_smb_rename, body);
    CHECK(g_status == 200 && strstr(g_req, "\"path\":\"G\"") && strstr(g_req, "\"to\":\"G.ps5u-new\""));
    snprintf(body, sizeof body, "{\"from\":\"smb://%s/Games/G\",\"to\":\"smb://%s/Other/G\"}", id, id);
    call(webui_smb_rename, body);
    CHECK(g_status == 400 && strstr(g_body, "bad_path")); /* another folder */
    snprintf(body, sizeof body, "{\"from\":\"smb://%s/\",\"to\":\"smb://%s/G\"}", id, id);
    call(webui_smb_rename, body);
    CHECK(g_status == 400); /* the share's top */
    snprintf(body, sizeof body, "{\"from\":\"smb://%s/G\",\"to\":\"smb://ffffffff/G\"}", id);
    call(webui_smb_rename, body);
    CHECK(g_status == 400 && strstr(g_body, "bad_to")); /* another share */
    snprintf(body, sizeof body, "{\"from\":\"smb://%s/G\",\"to\":\"/data/G\"}", id);
    call(webui_smb_rename, body);
    CHECK(g_status == 400 && strstr(g_body, "bad_to")); /* off the share */
    call(webui_smb_rename, "{\"from\":\"/data/a\",\"to\":\"/data/b\"}");
    CHECK(g_status == 400 && strstr(g_body, "bad_from"));
    g_next_reply = "{\"error\":\"exists\"}";
    snprintf(body, sizeof body, "{\"from\":\"smb://%s/G\",\"to\":\"smb://%s/H\"}", id, id);
    call(webui_smb_rename, body);
    CHECK(g_status == 409 && strstr(g_body, "exists")); /* never over something */
    g_next_reply = NULL;

    /* jobs never start the daemon */
    g_daemon_up = 0;
    call(webui_smb_job, "{\"job\":\"0123456789abcdef0123456789abcdef\"}");
    CHECK(g_status == 404 && strstr(g_body, "unknown_job"));
    call(webui_smb_jobs, "{}");
    CHECK(g_status == 200 && strcmp(g_body, "{\"jobs\":[]}") == 0);
    g_daemon_up = 1;
    call(webui_smb_job, "{\"job\":\"nothex\"}");
    CHECK(g_status == 400);

    snprintf(body, sizeof body, "{\"id\":\"%s\"}", id);
    call(webui_smb_remove, body);
}

static void t_status(void) {
    CHECK(webui_smb_http_status("smb_auth") == 403);
    CHECK(webui_smb_http_status("smb_unreachable") == 502);
    CHECK(webui_smb_http_status("smb_no_share") == 404);
    CHECK(webui_smb_http_status("exists") == 409);
    CHECK(webui_smb_http_status("no_space") == 507);
    CHECK(webui_smb_http_status("smb_loader_unavailable") == 503);
    CHECK(webui_smb_http_status("bad_host") == 400);
    CHECK(webui_smb_http_status("weird") == 500);
}

static void t_lines(void) {
    smb_share_t a, b;
    uint8_t key[32], other[32];
    char line[4096];
    int k;
    memset(&a, 0, sizeof a);
    memset(key, 7, sizeof key);
    memset(other, 8, sizeof other);
    strcpy(a.id, "0a1b2c3d");
    strcpy(a.host, "nas");
    strcpy(a.share, "共享");
    strcpy(a.user, "bob");
    strcpy(a.password, "p\"w\\d");
    strcpy(a.start_path, "a/b");
    CHECK(smb_share_check(&a, NULL) == 0 && a.port == 445 && strcmp(a.name, "nas/共享") == 0);
    k = smb_share_to_line(&a, key, line, sizeof line);
    CHECK(k > 0 && !strstr(line, "p\\\"w"));
    CHECK(smb_share_from_line(line, (size_t)k, key, &b) == 0 && strcmp(b.password, "p\"w\\d") == 0 && b.has_password);
    CHECK(strcmp(b.share, "共享") == 0 && strcmp(b.start_path, "a/b") == 0);
    /* the wrong key: the share stays, its password does not */
    CHECK(smb_share_from_line(line, (size_t)k, other, &b) == 0 && !b.has_password && b.password[0] == '\0');
    CHECK(smb_share_from_line("{\"id\":\"zz\"}", 11, key, &b) != 0);
    CHECK(smb_share_to_line(&a, key, line, 20) == -1);
    {
        char id[16], rel[256];
        CHECK(smb_path_parse("smb://0a1b2c3d/x/y/", id, rel, sizeof rel) == 0 && !strcmp(id, "0a1b2c3d") && !strcmp(rel, "x/y"));
        CHECK(smb_path_parse("smb://0a1b2c3d", id, rel, sizeof rel) == 0 && rel[0] == '\0');
        CHECK(smb_path_parse("smb://0A1B2C3D/x", id, rel, sizeof rel) != 0);
        CHECK(smb_path_parse("smb://0a1b2c3/x", id, rel, sizeof rel) != 0);
        CHECK(smb_path_parse("smb://0a1b2c3d/../x", id, rel, sizeof rel) != 0);
        CHECK(smb_path_parse("/data/x", id, rel, sizeof rel) != 0);
        CHECK(smb_path_make("0a1b2c3d", "x", rel, sizeof rel) == 0 && !strcmp(rel, "smb://0a1b2c3d/x"));
    }
}

/* A job's status is the job's outcome: a failed or cancelled job's "error" is relayed as 200 with
 * the status, never turned into the request's own failure (507 / 409 / 502, which the page read as
 * the daemon gone). Only a daemon refusal (no "state") maps to a status. */
static void t_job_status(void) {
    static const char failed[] =
        "{\"job\":\"0123456789abcdef0123456789abcdef\",\"kind\":\"download\",\"state\":\"failed\",\"bytes_done\":5,"
        "\"bytes_total\":9,\"files_done\":0,\"files_total\":1,\"scanning\":false,\"reconnects\":0,\"dialect\":785,"
        "\"started\":1,\"current\":\"a.pkg\",\"src\":\"a.pkg\",\"dest\":\"/data/a.pkg\",\"error\":\"no_space\","
        "\"failed\":\"a.pkg\",\"detail\":\"\"}";
    static const char cancelled[] =
        "{\"job\":\"0123456789abcdef0123456789abcdef\",\"kind\":\"upload\",\"state\":\"cancelled\",\"error\":\"cancelled\"}";
    static const char running[] =
        "{\"job\":\"0123456789abcdef0123456789abcdef\",\"kind\":\"scan\",\"state\":\"running\",\"error\":null}";
    const char *q = "{\"job\":\"0123456789abcdef0123456789abcdef\"}";
    g_next_reply = failed;
    call(webui_smb_job, q);
    CHECK(g_status == 200 && strcmp(g_body, failed) == 0);
    g_next_reply = cancelled;
    call(webui_smb_job, q);
    CHECK(g_status == 200 && strcmp(g_body, cancelled) == 0);
    g_next_reply = running;
    call(webui_smb_job, q);
    CHECK(g_status == 200 && strcmp(g_body, running) == 0);
    /* the daemon's own refusals */
    g_next_reply = "{\"error\":\"not_found\"}";
    call(webui_smb_job, q);
    CHECK(g_status == 404 && strstr(g_body, "unknown_job"));
    g_next_reply = "{\"error\":\"busy\"}";
    call(webui_smb_job, q);
    CHECK(g_status == 503 && strstr(g_body, "busy"));
    g_next_reply = NULL;
    call(webui_smb_job, "{\"job\":\"nope\"}");
    CHECK(g_status == 400 && strstr(g_body, "bad_job"));
}

/* The per-console key is replaced only when there is none: a key file that cannot be read is an
 * error, not a new key (which would leave every saved password undecryptable). */
static void t_key_file(void) {
    char before[80], after[80];
    FILE *f;
    size_t n;
    call(webui_smb_save, "{\"server\":\"nas\",\"share\":\"k\",\"user\":\"u\",\"password\":\"p\"}");
    CHECK(g_status == 200);
    f = fopen(g_key, "rb");
    n = f ? fread(before, 1, sizeof before - 1, f) : 0;
    if (f) fclose(f);
    before[n] = '\0';
    CHECK(n == 64);
    if (geteuid() != 0) { /* root reads a 000 file; the check needs the refusal */
        CHECK(chmod(g_key, 0) == 0);
        call(webui_smb_shares, "{}");
        CHECK(g_status == 500 && strstr(g_body, "io_error"));
        CHECK(chmod(g_key, 0600) == 0);
        f = fopen(g_key, "rb");
        n = f ? fread(after, 1, sizeof after - 1, f) : 0;
        if (f) fclose(f);
        after[n] = '\0';
        CHECK(n == 64 && strcmp(before, after) == 0);
        call(webui_smb_shares, "{}");
        CHECK(g_status == 200 && strstr(g_body, "\"has_password\":true"));
    }
    /* an empty (never finished) key file is made again */
    unlink(g_file);
    f = fopen(g_key, "wb");
    if (f) fclose(f);
    call(webui_smb_shares, "{}");
    CHECK(g_status == 200);
    f = fopen(g_key, "rb");
    n = f ? fread(after, 1, sizeof after - 1, f) : 0;
    if (f) fclose(f);
    after[n] = '\0';
    CHECK(n == 64 && strcmp(before, after) != 0);
}

int main(void) {
    snprintf(g_dir, sizeof g_dir, "/tmp/ps5upload-webui-smb-%d", (int)getpid());
    snprintf(g_file, sizeof g_file, "%s/shares.jsonl", g_dir);
    snprintf(g_key, sizeof g_key, "%s/shares.key", g_dir);
    mkdir(g_dir, 0700);
    smb_shares_set_paths(g_file, g_key);
    g_cfg.may_write = allow_write;
    g_cfg.may_read = allow_read;
    t_lines();
    t_save_list_remove();
    t_pc_and_library();
    t_paths_and_copy();
    t_status();
    t_job_status();
    t_key_file();
    unlink(g_file);
    unlink(g_key);
    rmdir(g_dir);
    if (g_fail) {
        fprintf(stderr, "%d check(s) failed\n", g_fail);
        return 1;
    }
    printf("webui smb self-test: ok\n");
    return 0;
}
