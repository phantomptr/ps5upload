#include "sony.h"
#include "hints.h"
#include "pathsafe.h"

#include <string.h>
#include <stdio.h>
#include <unistd.h>

#include <ps5/kernel.h>
#include "authid.h"          /* PS5_SHELLCORE_AUTHID, ps5_detect_firmware_major */
#include "timed_init.h"      /* ps5_timed_init_wait, PS5_TIMED_INIT_TIMEOUT */
#include "sceAppInstUtil.h"  /* MetaInfo, sceAppInstUtil* */

static ps5_timed_init_state_t g_init_state = PS5_TIMED_INIT_STATE_INITIALIZER;

int inst_sony_init(inst_sony_state_t *st) {
    int rc = ps5_timed_init_wait(&g_init_state, sceAppInstUtilInitialize, 10000U);
    st->init_rc = (uint32_t)rc;
    st->init_done = (rc == 0);
    return rc;
}

/* One InstallByPackage call with the given URI. content_id is always "".
 * When `swap_shellcore` is set, swap to ShellCore authid around the call. */
static uint32_t call_install(const char *uri, const char *name_hint,
                             int swap_shellcore) {
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
    return (uint32_t)rc;
}

inst_install_result_t inst_sony_install(const inst_request_t *req,
                                        inst_sony_state_t *st,
                                        inst_loopback_t **lb_out,
                                        const char *attempt) {
    inst_install_result_t r = { 0, 0, "url", NULL };
    *lb_out = NULL;

    if (req->src == INST_SRC_URL) {
        r.code = call_install(req->url, req->name_hint, 0);
        r.via = "url";
        r.accepted = (r.code == 0);
        if (!r.accepted) r.hint = inst_install_hint(r.code);
        return r;
    }

    /* INST_SRC_PATH: try loopback first. */
    const char *base = inst_basename(req->path);
    inst_loopback_t *lb = NULL;
    uint16_t port = 0;
    if (inst_loopback_start(&lb, req->path, attempt, base, &port) == 0) {
        char url[INST_URL_MAX];
        if (inst_build_loopback_url(url, sizeof(url), port, attempt, base) > 0) {
            uint32_t code = call_install(url, req->name_hint, 0);
            if (code == 0) {
                r.accepted = 1; r.code = 0; r.via = "loopback";
                *lb_out = lb;               /* caller tracks the serving job */
                return r;
            }
            /* one-shot fallback ONLY for a network-class refusal */
            if (inst_is_network_class(code)) {
                inst_loopback_stop(lb);
                lb = NULL;
                char fixed[INST_PATH_MAX];
                if (inst_rewrite_path(req->path, fixed, sizeof(fixed)) == 0) {
                    int swap = (st->fw_major > 0 && st->fw_major < 11);
                    uint32_t code2 = call_install(fixed, req->name_hint, swap);
                    r.via = "path";
                    r.code = code2;
                    r.accepted = (code2 == 0);
                    if (!r.accepted) r.hint = inst_install_hint(code2);
                    return r;
                }
            }
            /* non-network refusal: return it as-is */
            inst_loopback_stop(lb);
            r.via = "loopback"; r.code = code; r.accepted = 0;
            r.hint = inst_install_hint(code);
            return r;
        }
        inst_loopback_stop(lb);
    }

    /* loopback could not start: fall straight to the bare path. */
    {
        char fixed[INST_PATH_MAX];
        if (inst_rewrite_path(req->path, fixed, sizeof(fixed)) != 0) {
            r.via = "path"; r.code = 0x80020002u; r.accepted = 0; return r;
        }
        int swap = (st->fw_major > 0 && st->fw_major < 11);
        r.code = call_install(fixed, req->name_hint, swap);
        r.via = "path";
        r.accepted = (r.code == 0);
        if (!r.accepted) r.hint = inst_install_hint(r.code);
        return r;
    }
}
