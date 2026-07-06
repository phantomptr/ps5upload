/*
 * Standalone DPI (Direct Package Installer) daemon for PS5.
 *
 * Ported to plain C from cy33hc/ps5-ezremote-dpi (source/main.cpp),
 * which is in turn modelled on etaHEN's DPI. Credit: cy33hc.
 * The boot-timing + init-retry pattern is ported from elf-arsenal's
 * payloads-src/dpi/main.c (soniciso/elf-arsenal).
 *
 * Why ps5upload ships this as a SEPARATE payload rather than calling
 * sceAppInstUtilInstallByPackage from the main ps5upload.elf: the main
 * payload runs with heavily-modified credentials (ucred jailbreak,
 * forged ShellCore authid, prior ptrace/Initialize calls). On FW 11.x
 * Sony's installer rejects an install issued from that context
 * (observed 0x8041013d). A freshly elfldr-loaded standalone process —
 * this daemon — calls the same API with the loader's pristine
 * credentials and succeeds. ps5upload's engine auto-sends this ELF to
 * the loader port when :9040 isn't already listening, then hands it an
 * http(s):// URL to install (the engine serves the .pkg over its own
 * /pkg-host/ endpoint).
 *
 * Protocol (matches the reference so it stays drop-in compatible with
 * the wider scene tooling): connect to TCP :9040, send one line — an
 * http(s):// URL, a bare local path, or "stop" to shut the daemon down
 * — terminated by \n or \r. The daemon replies with one of:
 *   "ok"                     — InstallByPackage accepted (rc == 0)
 *   "error:0x%08X"           — InstallByPackage rejected with rc
 *   "error:init:0x%08X"      — sceAppInstUtilInitialize failed with rc
 *   "error:init:timeout"     — sceAppInstUtilInitialize timed out
 *   "error:badpath"          — path rejected by safety check
 * and closes the connection.
 *
 * Boot timing + init retry (the fix for the FW-10.40 "helper dies ~4s
 * after the first install is rejected" symptom, issue #152):
 * kstuff spawns last in the payload chain and applies kernel patches
 * that sceAppInstUtilInitialize depends on. This daemon sleeps 25 s at
 * startup (500 ms steps) to let those patches land, then runs
 * sceAppInstUtilInitialize on a detached thread with a 10 s timeout.
 * If startup init failed (IPMI backend not ready yet, or FW-11+
 * SYSTEM_AUTHID gate), every incoming install request retries init
 * once before attempting the install. Calling InstallByPackage COLD
 * (no init) leaves IPMI state half-wedged and Sony's watchdog kills
 * the helper a few seconds later — exactly the symptom users report.
 */
/* NOTE: no `#undef main` here (the reference had one for a different
 * loader). The PS5 Payload SDK's crt wraps `main` to wire up the payload
 * entry; undef'ing it bypasses that wrapper and the ELF loads but never
 * runs (no :9040 bind) — matches our main payload, which uses plain
 * `int main(void)` with no undef. */

#include <stdio.h>
#include <stdlib.h>
#include <stdarg.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>
#include <stdint.h>
#include <time.h>
#include <pthread.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>

#include "sceAppInstUtil.h"

#define BUF_SIZE      4096
#define PORT          9040
#define INIT_TIMEOUT  10            /* seconds to wait for Initialize */
#define BOOT_WAIT_MS  500           /* per-step boot sleep */
#define BOOT_WAIT_STEPS 50          /* 50 * 500ms = 25s total */

/* Sony toast helper — same shape the reference uses. */
typedef struct notify_request {
    char useless1[45];
    char message[3075];
} notify_request_t;

extern int sceKernelSendNotificationRequest(int, notify_request_t *, size_t, int);

static void notify(const char *fmt, ...) {
    notify_request_t req;
    va_list args;
    bzero(&req, sizeof req);
    va_start(args, fmt);
    vsnprintf(req.message, sizeof req.message, fmt, args);
    va_end(args);
    sceKernelSendNotificationRequest(0, &req, sizeof req, 0);
}

/* ── timed sceAppInstUtilInitialize (ported from elf-arsenal's DPI v1) ── */
static volatile int g_init_done = 0;
static volatile int g_init_rc   = -1;

static void *init_thread(void *arg) {
    (void)arg;
    g_init_rc   = sceAppInstUtilInitialize();
    g_init_done = 1;
    return NULL;
}

/* Runs sceAppInstUtilInitialize on a detached thread with a timeout.
 * Returns the init rc on success, or -0xDEAD on timeout so callers can
 * distinguish "init failed with Sony error X" from "init hung". */
#define INIT_TIMEOUT_SENTINEL (-0xDEAD)
static int timed_init(void) {
    pthread_t tid;
    g_init_done = 0;
    g_init_rc   = -1;
    if (pthread_create(&tid, NULL, init_thread, NULL) != 0)
        return -1;
    pthread_detach(tid);

    for (int i = 0; i < INIT_TIMEOUT * 10; i++) {
        if (g_init_done)
            return g_init_rc;
        struct timespec ts = {0, 100000000};  /* 100 ms */
        nanosleep(&ts, NULL);
    }
    return INIT_TIMEOUT_SENTINEL;
}

/* ── path safety + rewrite ───────────────────────────────────────────── */

/* Reject path traversal and obviously broken inputs before handing them
 * to Sony's installer. Mirrors elf-arsenal's pkg_filename_is_safe
 * (homebrew.c:1497) for the local-path case; URLs are accepted as-is. */
static int path_is_safe(const char *s) {
    if (s == NULL || s[0] == '\0') return 0;
    /* http(s):// URLs are validated by Sony's URI parser; let them through. */
    if (strncmp(s, "http://", 7) == 0 || strncmp(s, "https://", 8) == 0)
        return 1;
    /* Bare local path. */
    if (s[0] != '/') return 0;
    if (strstr(s, "..") != NULL) return 0;
    size_t n = strlen(s);
    if (n >= BUF_SIZE) return 0;
    return 1;
}

/* Sony's install service runs in a sandbox that sees the filesystem
 * rooted at /user, so a file at /data/foo on the payload's view is at
 * /user/data/foo from the installer's view. Rewrite the path so the
 * installer can actually find the staged pkg. URLs pass through
 * unchanged. Ported from elf-arsenal's rewrite_path_for_install
 * (homebrew.c:425). */
static void rewrite_path_for_install(const char *in, char *out, size_t out_size) {
    if (strncmp(in, "/data/", 6) == 0) {
        snprintf(out, out_size, "/user%s", in);   /* /data/foo -> /user/data/foo */
        return;
    }
    strncpy(out, in, out_size - 1);
    out[out_size - 1] = '\0';
}

int main(void) {
    int server_fd, new_socket, ret;
    struct sockaddr_in address;
    socklen_t addrlen = sizeof(address);
    char buffer[BUF_SIZE];
    char fixed[BUF_SIZE];
    char *pos;
    PlayGoInfo playgo_info;
    SceAppInstallPkgInfo pkg_info;
    MetaInfo metainfo;

    /* Boot-timing wait: let kstuff apply kernel patches before we touch
     * AppInstUtil. Without this, on FW where kstuff loads after us,
     * sceAppInstUtilInitialize hits unpatched kernel paths and either
     * hangs or returns an error that leaves IPMI wedged. */
    {
        struct timespec ts = {0, BOOT_WAIT_MS * 1000000L};  /* 500 ms */
        for (int i = 0; i < BOOT_WAIT_STEPS; i++)            /* 25 s total */
            nanosleep(&ts, NULL);
    }
    fprintf(stderr, "[dpi] startup wait done, calling sceAppInstUtilInitialize\n");
    int init_rc = timed_init();
    fprintf(stderr, "[dpi] sceAppInstUtilInitialize rc=0x%x%s\n",
            (unsigned)init_rc,
            init_rc == 0 ? " (ok)" :
            (init_rc == INIT_TIMEOUT_SENTINEL ? " (timeout — fallback mode)" :
             " (fallback mode)"));

    server_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (server_fd < 0) {
        notify("ezRemote DPI create socket failed");
        return -1;
    }

    int reuse = 1;
    setsockopt(server_fd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));

    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = INADDR_ANY;
    address.sin_port = htons(PORT);

    if (bind(server_fd, (const struct sockaddr *)&address, sizeof(address)) < 0) {
        /* Another DPI (ours or the scene's) already owns the port — that's
         * fine, the install path can use it. Exit cleanly. */
        notify("ezRemote DPI: port %d already in use", PORT);
        close(server_fd);
        return 0;
    }

    if (listen(server_fd, 3) < 0) {
        notify("ezRemote DPI listen failed");
        close(server_fd);
        return 0;
    }

    notify("ezRemote DPI listening on port %d", PORT);

    for (;;) {
        new_socket = accept(server_fd, (struct sockaddr *)&address, &addrlen);
        if (new_socket < 0) {
            continue;
        }

        struct timeval tv;
        tv.tv_sec = 1;
        tv.tv_usec = 0;
        setsockopt(new_socket, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
        int yes = 1;
        setsockopt(new_socket, IPPROTO_TCP, TCP_NODELAY, &yes, sizeof(int));

        memset(buffer, 0, sizeof(buffer));
        ret = -1;
        int valread = recv(new_socket, buffer, BUF_SIZE - 1, 0);
        if (valread > 0) {
            buffer[valread] = '\0';
            if ((pos = strchr(buffer, '\r')) != NULL) *pos = '\0';
            if ((pos = strchr(buffer, '\n')) != NULL) *pos = '\0';

            if (strncmp(buffer, "stop", 4) == 0) {
                close(new_socket);
                break;
            }

            /* If startup init failed (or timed out), retry once now —
             * IPMI may have become available since boot. Calling
             * InstallByPackage cold leaves IPMI half-wedged and trips
             * Sony's watchdog a few seconds later. */
            if (init_rc != 0) {
                fprintf(stderr, "[dpi] retrying init (prev rc=0x%x)\n",
                        (unsigned)init_rc);
                init_rc = timed_init();
            }
            if (init_rc != 0) {
                char err[64];
                if (init_rc == INIT_TIMEOUT_SENTINEL)
                    snprintf(err, sizeof(err), "error:init:timeout");
                else
                    snprintf(err, sizeof(err), "error:init:0x%08X",
                             (unsigned)init_rc);
                fprintf(stderr, "[dpi] init failed, sending %s\n", err);
                notify("ezRemote DPI init failed:\n%s", err + 6);
                (void)send(new_socket, err, strlen(err), 0);
                close(new_socket);
                continue;
            }

            if (!path_is_safe(buffer)) {
                fprintf(stderr, "[dpi] rejected unsafe path\n");
                (void)send(new_socket, "error:badpath", 13, 0);
                close(new_socket);
                continue;
            }

            /* Rewrite /data/ -> /user/data/ so the sandboxed installer
             * can see the staged pkg. URLs pass through unchanged. */
            rewrite_path_for_install(buffer, fixed, sizeof(fixed));

            notify("ezRemote DPI received:\n%s", fixed);

            memset(&playgo_info, 0, sizeof(playgo_info));
            memset(&pkg_info, 0, sizeof(pkg_info));

            /* Install via InstallByPackage with the input as the URI — the
             * etaHEN DPI v2 / upstream ezremote-dpi recipe. A bare local
             * absolute path (the staged-pkg case, fixed[0]=='/') and an
             * http(s):// URL are BOTH accepted as the uri.
             *
             * 2.25.2 — CRITICAL: this used to route local paths to
             * sceAppInstUtilAppInstallPkg, which registers the title's
             * METADATA only (the tile shows up) but installs NO launchable
             * CONTENT — so the game failed with "can't start the game or
             * app" when launched. That was the wrong API. InstallByPackage
             * with the bare local path installs the full content and the
             * game launches (exactly what etaHEN DPI v2 and ext-HDD installs
             * do). The 0x80B22404 PlayGo reject that AppInstallPkg was
             * chosen to avoid only fires for real http:// URLs that need the
             * HTTP pre-flight; a bare local path skips it. The caller stages
             * the .pkg on the console's disk first. */
            metainfo.uri                = fixed;
            metainfo.ex_uri             = "";
            metainfo.playgo_scenario_id = "";
            metainfo.content_id         = "";
            metainfo.content_name       = fixed;
            metainfo.icon_url           = "";
            ret = sceAppInstUtilInstallByPackage(&metainfo, &pkg_info, &playgo_info);
            if (ret != 0) {
                fprintf(stderr, "[dpi] InstallByPackage rc=0x%08X\n", (unsigned)ret);
                notify("ezRemote DPI install failed\nError Code: 0x%08X",
                       (unsigned)ret);
            } else {
                fprintf(stderr, "[dpi] InstallByPackage ok\n");
            }
        }

        /* Reply in the reference's ok/error form so the caller can tell
         * accept from reject from init-failure. The engine's
         * dpi_install_handler parses this tri-state. */
        char resp[64];
        int n;
        if (ret == 0) {
            n = snprintf(resp, sizeof(resp), "ok");
        } else if (ret == -1) {
            /* recv failed / no valid input — already sent a specific error
             * above for the init/badpath cases; for a bare recv failure
             * send a generic error so the caller doesn't hang. */
            n = snprintf(resp, sizeof(resp), "error:recv");
        } else {
            n = snprintf(resp, sizeof(resp), "error:0x%08X", (unsigned)ret);
        }
        if (n > 0) {
            (void)send(new_socket, resp, (size_t)n, 0);
        }
        close(new_socket);
    }

    notify("ezRemote DPI stopping");
    close(server_fd);
    return 0;
}
