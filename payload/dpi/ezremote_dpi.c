/*
 * Standalone DPI (Direct Package Installer) daemon for PS5.
 *
 * Ported to plain C from cy33hc/ps5-ezremote-dpi (source/main.cpp),
 * which is in turn modelled on etaHEN's DPI. Credit: cy33hc.
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
 * http(s):// URL, or "stop" to shut the daemon down — terminated by
 * \n or \r. The daemon replies with the decimal InstallByPackage
 * return code ("0" on accept) and closes the connection.
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
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>

#include "sceAppInstUtil.h"

#define BUF_SIZE 4096
#define PORT     9040

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

int main(void) {
    int server_fd, new_socket, ret;
    struct sockaddr_in address;
    socklen_t addrlen = sizeof(address);
    char buffer[BUF_SIZE];
    char *pos;
    PlayGoInfo playgo_info;
    SceAppInstallPkgInfo pkg_info;
    MetaInfo metainfo;

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

            notify("ezRemote DPI received:\n%s", buffer);

            memset(&playgo_info, 0, sizeof(playgo_info));
            memset(&pkg_info, 0, sizeof(pkg_info));

            if (buffer[0] == '/') {
                /* Local absolute path → AppInstallPkg (elf-arsenal path).
                 * No URI parse, no PlayGo HTTP gate — the path that works
                 * without the 0x80B22404 reject. The caller stages the
                 * .pkg under /user/data first. */
                ret = sceAppInstUtilAppInstallPkg(buffer, &pkg_info);
            } else {
                /* http(s):// URL → InstallByPackage (ezremote-dpi path).
                 * Kept for firmware/setups where the HTTP fetch isn't
                 * gated. */
                metainfo.uri                = buffer;
                metainfo.ex_uri             = "";
                metainfo.playgo_scenario_id = "";
                metainfo.content_id         = "";
                metainfo.content_name       = buffer;
                metainfo.icon_url           = "";
                ret = sceAppInstUtilInstallByPackage(&metainfo, &pkg_info, &playgo_info);
            }
            if (ret != 0) {
                notify("ezRemote DPI install failed\nError Code: 0x%08X", (unsigned)ret);
            }
        }

        /* Reply with the decimal return code so the caller can tell
         * accept (0) from reject. */
        char resp[16];
        int n = snprintf(resp, sizeof(resp), "%d", ret);
        if (n > 0) {
            (void)send(new_socket, resp, (size_t)n, 0);
        }
        close(new_socket);
    }

    notify("ezRemote DPI stopping");
    close(server_fd);
    return 0;
}
