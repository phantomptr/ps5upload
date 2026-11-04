#include "sony.h"
#include "hints.h"
#include "pathsafe.h"

#include <string.h>
#include <stdio.h>
#include <unistd.h>
#include <sys/stat.h>

#include <ps5/kernel.h>
#include "authid.h"          /* PS5_SHELLCORE_AUTHID, ps5_detect_firmware_major */
#include "inst_timed_init.h" /* inst_timed_init_wait: monotonic bound */
#include "sceAppInstUtil.h"  /* MetaInfo, sceAppInstUtil* */

static inst_timed_init_state_t g_init_state = INST_TIMED_INIT_STATE_INITIALIZER;

int inst_sony_init(inst_sony_state_t *st) {
    int rc = inst_timed_init_wait(&g_init_state, sceAppInstUtilInitialize, 10000U);
    st->init_rc = (uint32_t)rc;
    st->init_done = (rc == 0);
    return rc;
}

/* One InstallByPackage call with the given URI. content_id is always "".
 * When `swap_shellcore` is set, swap to ShellCore authid around the call.
 *
 * `meta` must be zeroed in full before it is filled: Sony reads past its six pointers (see
 * MetaInfo), and stack leftovers there are what made it refuse packages with 0x80B2116F.
 *
 * The call stays on the connection's own thread. Running it on a thread started for it, to
 * put a time limit on it, was once measured as "refused every time" on FW 13.60; that was
 * before the bytes after `meta` were known to matter, so it may have been the same thing
 * and has not been measured again. A call that never returns is bounded by the watchdog in
 * main.c instead: the engine's deadline on the request only closes its socket, and the daemon
 * would hold the Sony lock for good, refusing `stop` and queueing every later install. */
static uint32_t call_install(const char *uri, const char *name_hint,
                             int swap_shellcore) {
    fprintf(stderr, "[installer] InstallByPackage uri=%s swap=%d\n", uri, swap_shellcore);
    MetaInfo meta;
    SceAppInstallPkgInfo pkg_info;
    PlayGoInfo playgo;
    memset(&meta, 0, sizeof(meta));
    memset(&pkg_info, 0, sizeof(pkg_info));
    memset(&playgo, 0, sizeof(playgo));
    meta.uri                = uri;
    meta.ex_uri             = "";
    meta.playgo_scenario_id = "";
    meta.content_id         = "";                 /* always empty */
    meta.content_name       = name_hint ? name_hint : "";
    meta.icon_url           = "";

    uint64_t saved = 0;
    if (swap_shellcore)
        saved = ps5_authid_acquire("installer", PS5_SHELLCORE_AUTHID);
    int rc = sceAppInstUtilInstallByPackage(&meta, &pkg_info, &playgo);
    if (swap_shellcore)
        ps5_authid_release(saved, "installer");
    fprintf(stderr, "[installer]   -> rc=0x%08x\n", (unsigned)rc);
    return (uint32_t)rc;
}

/* The plain-path last resort (`"route":"path"`): the forms a console-local package can be
 * offered in that are NOT a URL on the console itself: its path, then a file:// URI. The first
 * Sony accepts wins; the last refusal is the one reported. Only reached for a base game that
 * is not installed (the engine's guard), so a refusal has nothing to undo.
 *
 * sceAppInstUtilAppInstallPkg is deliberately NOT tried, though it is the one form FW 13.60
 * accepts here (path: 0x80B2116F, file://: 0x80B21106, measured on a CFI-1115A). It returned
 * 0 and left a tile with no content: /user/appmeta/<id> written, /user/app/<id> never created,
 * launch 0x80B21401, where a stream install of the same package wrote app.pkg. An install
 * that reports success and cannot start is worse than a refusal. */
static inst_install_result_t install_plain_path(const inst_request_t *req,
                                                inst_sony_state_t *st) {
    inst_install_result_t r = { 0, 0x80020002u, "path", NULL, NULL };
    char fixed[INST_PATH_MAX];
    if (inst_rewrite_path(req->path, fixed, sizeof(fixed)) != 0) return r;

    int swap = (st->fw_major > 0 && st->fw_major < 11);
    r.code = call_install(fixed, req->name_hint, swap);
    r.via = "path";
    if (r.code == 0) { r.accepted = 1; return r; }

    char file_uri[INST_PATH_MAX + 8];
    int n = snprintf(file_uri, sizeof(file_uri), "file://%s", fixed);
    if (n > 0 && (size_t)n < sizeof(file_uri)) {
        uint32_t code = call_install(file_uri, req->name_hint, swap);
        if (code == 0) { r.accepted = 1; r.code = 0; r.via = "file"; return r; }
        r.code = code;
        r.via = "file";
    }

    r.hint = inst_install_hint(r.code);
    return r;
}

inst_install_result_t inst_sony_install(const inst_request_t *req,
                                        inst_sony_state_t *st,
                                        inst_loopback_t **lb_out,
                                        const char *attempt) {
    inst_install_result_t r = { 0, 0, "url", NULL, NULL };
    *lb_out = NULL;

    if (req->src == INST_SRC_URL) {
        r.code = call_install(req->url, req->name_hint, 0);
        r.via = "url";
        r.accepted = (r.code == 0);
        if (!r.accepted) r.hint = inst_install_hint(r.code);
        return r;
    }

    /* The plain path was asked for (`"route":"path"`): the caller has already
     * seen the console refuse its own loopback copy, so serving it again
     * would only be refused again. */
    if (req->bare_path) return install_plain_path(req, st);

    /* INST_SRC_PATH: loopback, and nothing else (see sony.h). */
    r.via = "loopback";
    struct stat sb;
    if (stat(req->path, &sb) != 0 || !S_ISREG(sb.st_mode) || access(req->path, R_OK) != 0) {
        r.code = 0x80020002u;
        r.error = "source_unreadable";
        return r;
    }
    const char *base = inst_loopback_serve_name(attempt, inst_basename(req->path));
    inst_loopback_t *lb = NULL;
    uint16_t port = 0;
    char url[INST_URL_MAX];
    if (inst_loopback_start(&lb, req->path, attempt, base, &port) != 0) {
        r.code = 0x80020002u;
        r.error = "loopback_failed";
        return r;
    }
    if (inst_build_loopback_url(url, sizeof(url), port, attempt, base) <= 0) {
        inst_loopback_stop(lb);
        r.code = 0x80020002u;
        r.error = "loopback_failed";
        return r;
    }
    r.code = call_install(url, req->name_hint, 0);
    if (r.code == 0) {
        r.accepted = 1;
        *lb_out = lb;                   /* caller tracks the serving job */
        return r;
    }
    inst_loopback_stop(lb);
    r.hint = inst_install_hint(r.code);
    return r;
}
