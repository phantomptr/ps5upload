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
 * this daemon — calls the same API with clean credentials and succeeds.
 * ps5upload's engine auto-sends this ELF to the loader port when :9040
 * isn't already listening, then hands it an http(s):// URL to install
 * (the engine serves the .pkg over its own /pkg-host/ endpoint).
 *
 * ── SYSTEM_AUTHID self-escalation (the FW-9.60 stream-install fix) ──
 * BGFT's task-creation path requires the calling process to hold
 * SYSTEM_AUTHID (0x4801000000000013) before it will queue a download
 * for an http:// URL. Without it, InstallByPackage returns 0x80431068
 * (BGFT download error) and fetches zero bytes — the exact failure
 * observed during HW testing on a Pro (FW 9.60). elf-arsenal solves
 * this by spawning dpi.elf as a CHILD of its kernel-RW main process
 * and escalating the child's pid via jb_escalate_pid(child_pid).
 *
 * Our DPI daemon is loaded by the PS5's :9021 elfldr, which is a
 * single-payload loader — there is no parent to escalate us. Instead
 * we self-escalate: the 25 s boot wait guarantees kstuff's kernel
 * patches are in place, so the kernel_set_ucred_* primitives from
 * <ps5/kernel.h> work from our freshly-loaded process. This mirrors
 * elf-arsenal's backup-helper and sdk-changer standalone payloads,
 * which also self-escalate via jb_escalate_pid(getpid()) on entry.
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
 * startup (500 ms steps) to let those patches land, then self-escalates
 * to SYSTEM_AUTHID, then runs sceAppInstUtilInitialize on a detached
 * thread with a 10 s timeout. If startup init failed (IPMI backend not
 * ready yet), every incoming install request retries init once before
 * attempting the install. Calling InstallByPackage COLD (no init)
 * leaves IPMI state half-wedged and Sony's watchdog kills the helper a
 * few seconds later — exactly the symptom users report.
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
#include <sys/types.h>

#include <ps5/kernel.h>

#include "sceAppInstUtil.h"

/* The SYSTEM_AUTHID Sony's BGFT download manager requires before it will
 * initialise (sceAppInstUtilInitialize). This is the same constant
 * elf-arsenal's jb.c uses (JB_AUTHID) and the same one our main payload's
 * bgft.c uses for the FW>=11 install path (PS5_SYSTEM_INSTALL_AUTHID). */
#define DPI_JB_AUTHID 0x4801000000000013ULL

/* ShellCore's authid — required by sceAppInstUtilInstallByPackage on
 * FW < 11. The ucred auth_id must equal ShellCore's identifier
 * (0x3800000000000010) for InstallByPackage's URL pre-flight. Without
 * it, InstallByPackage returns 0x80431068 (BGFT download error) for
 * http:// URLs and 0x80B21106 for file:// URLs. We self-escalate to
 * SYSTEM_AUTHID for init (above), then swap to ShellCore just for the
 * InstallByPackage call. Mirrors bgft.c:authid_acquire_shellcore(). */
#define DPI_SHELLCORE_AUTHID 0x3800000000000010ULL

/* ── jb_escalate_pid (ported from elf-arsenal's jb.c) ──────────────────
 * Rewrites the target pid's ucred to full root + SYSTEM_AUTHID + all
 * capabilities. Uses the kernel R/W primitives from <ps5/kernel.h>,
 * which work from ANY process once kstuff has patched the kernel
 * syscalls (the 25 s boot wait below guarantees kstuff has loaded).
 *
 * This is the exact pattern elf-arsenal's standalone payloads use
 * (backup-helper/main.c:25, sdk-changer/src/main.c:53) — they link
 * -lkernel_sys and self-escalate on entry. Our DPI daemon is in the
 * same situation: loaded by the PS5's :9021 elfldr with no parent
 * process to escalate it, so it must escalate itself. */
static int jb_escalate_pid(pid_t pid) {
    if (pid <= 0) return -1;

    intptr_t proc = kernel_get_proc(pid);
    if (!proc) return -1;

    int rc = 0;

    /* uid → 0 (root) on every cred slot the kernel checks. */
    if (kernel_set_ucred_uid (pid, 0) != 0) rc = -1;
    if (kernel_set_ucred_ruid(pid, 0) != 0) rc = -1;
    if (kernel_set_ucred_svuid(pid, 0) != 0) rc = -1;
    if (kernel_set_ucred_rgid(pid, 0) != 0) rc = -1;
    if (kernel_set_ucred_svgid(pid, 0) != 0) rc = -1;

    /* Sandbox escape: rootdir/jaildir → kernel root vnode. */
    intptr_t rootvnode = kernel_get_root_vnode();
    if (rootvnode) {
        if (kernel_set_proc_rootdir(pid, rootvnode) != 0) rc = -1;
        if (kernel_set_proc_jaildir(pid, rootvnode) != 0) rc = -1;
    }

    /* The load-bearing write: authid → SYSTEM (0x4801...0013). This is
     * what BGFT's task-creation path checks before accepting a download.
     * Without it, InstallByPackage returns 0x80431068 (BGFT download
     * error) and no bytes are fetched. */
    if (kernel_set_ucred_authid(pid, DPI_JB_AUTHID) != 0) rc = -1;

    /* All capability bits set. */
    uint8_t caps[16];
    memset(caps, 0xff, sizeof(caps));
    if (kernel_set_ucred_caps(pid, caps) != 0) rc = -1;

    /* cr_sceAttr high-attr flag. Mirrors elf-arsenal's backup-helper
     * exactly (attrs = 0x80). Our main payload uses 0x80000000 — a
     * different byte in the 8-byte attrs field — but the backup-helper
     * pattern is the proven one for standalone self-escalating ELFs
     * that pass BGFT's auth gate, so we follow it verbatim. */
    if (kernel_set_ucred_attrs(pid, 0x80) != 0) rc = -1;

    return rc;
}

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

    /* Self-escalate to SYSTEM_AUTHID BEFORE calling sceAppInstUtilInitialize.
     * This is the fix for the FW-9.60 "Stream (beta) doesn't install"
     * bug: BGFT's task-creation path rejects InstallByPackage(http_url)
     * calls from processes without SYSTEM_AUTHID (0x4801...0013), with
     * 0x80431068 (BGFT download error) and zero bytes fetched. The 25 s
     * boot wait above guarantees kstuff's kernel patches are in place,
     * so the kernel_set_ucred_* primitives work from this freshly-loaded
     * elfldr process. Mirrors elf-arsenal's backup-helper/sdk-changer
     * standalone payloads (jb_escalate_pid(getpid()) on entry). */
    {
        pid_t me = getpid();
        int esc_rc = jb_escalate_pid(me);
        fprintf(stderr, "[dpi] jb_escalate_pid(self=%d) rc=%d%s\n",
                (int)me, esc_rc,
                esc_rc == 0 ? " (SYSTEM_AUTHID acquired)" :
                " (FAILED — install will likely be rejected by BGFT)");
        if (esc_rc != 0) {
            notify("ezRemote DPI WARNING:\n"
                   "authid escalation failed.\n"
                   "Stream install may not work.\n"
                   "Ensure kstuff is loaded.");
        }
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

            /* Swap to ShellCore authid for the InstallByPackage call.
             * init() runs under SYSTEM_AUTHID (set at boot), but
             * InstallByPackage's URL pre-flight requires ShellCore's
             * authid (0x3800000000000010) on FW < 11. Without the swap,
             * InstallByPackage returns 0x80431068 (BGFT download error).
             * After the call we restore SYSTEM_AUTHID so subsequent
             * inits/calls stay in the privileged context. This mirrors
             * bgft.c:authid_acquire_shellcore() exactly. */
            pid_t me = getpid();
            uint64_t saved_authid = kernel_get_ucred_authid(me);
            if (saved_authid != 0) {
                kernel_set_ucred_authid(me, DPI_SHELLCORE_AUTHID);
            }
            ret = sceAppInstUtilInstallByPackage(&metainfo, &pkg_info, &playgo_info);
            if (saved_authid != 0) {
                kernel_set_ucred_authid(me, saved_authid);
            }
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
