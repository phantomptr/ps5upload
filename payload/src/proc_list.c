/*
 * proc_list.c — emit a JSON process summary using sysctl(KERN_PROC_PROC).
 *
 * Why sysctl, not a kernel allproc walk:
 *
 *   - kinfo_proc layout is FreeBSD-stable across every PS5 firmware
 *     we've ever shipped against. The pid lives at offset 72 and the
 *     thread-name at offset 447 from 9.x through 12.x, with no per-
 *     firmware offset table to keep current.
 *
 *   - shellui_rpc.c uses the same pattern to locate SceShellUI;
 *     keeping PROC_LIST on the same code path means there's only
 *     one offset table to maintain.
 *
 *   - No kernel R/W needed — sysctl works on plain unprivileged
 *     processes, so PROC_LIST is functional even before kstuff has
 *     handed us kernel-RW. (We still need it for everything else,
 *     just not for the process listing.)
 *
 * The output buffer is supplied by runtime.c and bounded; we never
 * malloc-output, only the sysctl staging buffer (which is freed
 * before we return). Truncation is signaled with a sentinel object
 * `{"truncated":true}` so the UI can badge "list cut short".
 */

#include "proc_list.h"

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <signal.h>
#include <unistd.h>

#include <sys/sysctl.h>
#include <sys/types.h>
#include <sys/user.h>

/* Layout offsets inside FreeBSD's kinfo_proc as exposed via
 * sysctl(KERN_PROC_PROC). Same offsets shellui_rpc.c uses. */
#define KINFO_PID_OFFSET     72
#define KINFO_TDNAME_OFFSET  447

/* The detailed (process-manager) path reads memory + thread count via the
 * SDK's `struct kinfo_proc` directly (cleaner than hand-offsetting every
 * field). This assert ties that struct to the SAME proven layout the raw
 * offsets above rely on: if a future SDK shuffles kinfo_proc, ki_pid moves
 * off 72 and the build fails LOUDLY instead of silently reporting garbage
 * memory. (pid@72 is independently confirmed by shellui_rpc.c.) */
_Static_assert(offsetof(struct kinfo_proc, ki_pid) == KINFO_PID_OFFSET,
               "kinfo_proc layout drift — re-verify ki_rssize/ki_numthreads");

/* sceKernelGetAppInfo(pid) → app id + title id for app/game processes; a
 * non-zero return means the pid isn't a registered app (daemon/system), in
 * which case app_id stays 0 and title_id empty. Same shape the reference
 * (drakmor/ps5-payload-manager) and Sony's own tooling use. */
typedef struct app_info {
    uint32_t app_id;
    uint64_t unknown1;
    char     title_id[14];
    char     unknown2[0x3c];
} app_info_t;
extern int sceKernelGetAppInfo(pid_t pid, app_info_t *info);

/* Classify a process for the manager UI's filter/guard:
 *   "app"     — a USER game/app: app_id != 0 with a real game title id
 *               (PPSA/CUSA/PCSA…). Shown by default, Restart-able.
 *   "payload" — a user ELF homebrew (name ends .elf, not mini-syscore).
 *               Shown by default.
 *   "system"  — everything else: Sce* daemons, kernel helpers, AND Sony's
 *               own NPXS* "apps" (SceShellUI/SceShellCore/etc.). Killing any
 *               of these can freeze/crash the console, so the UI hides them
 *               behind a toggle and adds an extra confirm.
 *
 * The NPXS check is the key safety distinction hardware testing surfaced:
 * SceShellUI has an app_id (it IS a registered app), but it's the home-
 * screen UI — emphatically NOT something to offer a one-tap Kill on. NPXS
 * is Sony's reserved prefix for built-in system applications. */
static const char *proc_kind(const char *comm, uint32_t app_id,
                             const char *title_id) {
    /* A user game/app needs BOTH an app_id AND a real title id. An app_id with
     * an empty title id (some processes report app_id != 0 but no resolvable
     * title) is treated as system, not "app": it can't be Restarted (no title
     * to relaunch) and shouldn't be offered a one-tap kill without the system
     * confirm. */
    if (app_id != 0 && title_id && title_id[0]) {
        /* NPXS* = Sony system app → treat as system, not a user app. */
        if (strncmp(title_id, "NPXS", 4) == 0) return "system";
        return "app";
    }
    if (comm && comm[0]) {
        if (strcmp(comm, "mini-syscore.elf") != 0) {
            const char *ext = strrchr(comm, '.');
            if (ext && strcasecmp(ext, ".elf") == 0) return "payload";
        }
    }
    return "system";
}

/* Reserve space for the JSON closing tail (`{"truncated":true}]}` is
 * 22 bytes; round up generously). */
#define TAIL_RESERVE         64u

/* JSON-escape a NUL-terminated process name into `dst`. PS5 process
 * names are plain ASCII in practice, but we still quote " and \ for
 * safety and replace anything below 0x20 with '?'. */
static size_t json_escape_name(const char *name, size_t name_max,
                               char *dst, size_t dst_cap) {
    size_t w = 0;
    if (dst_cap == 0) return 0;
    for (size_t i = 0; i < name_max && w + 2 < dst_cap; ++i) {
        unsigned char c = (unsigned char)name[i];
        if (c == 0) break;
        if (c == '"' || c == '\\') {
            if (w + 2 >= dst_cap) break;
            dst[w++] = '\\';
            dst[w++] = (char)c;
        } else if (c < 0x20) {
            dst[w++] = '?';
        } else {
            dst[w++] = (char)c;
        }
    }
    dst[w] = '\0';
    return w;
}

static int proc_list_build(char *buf, size_t cap, size_t *written_out,
                           const char **err_out, int detailed) {
    if (!buf || cap < 64 || !written_out) {
        if (err_out) *err_out = "proc_list_bad_args";
        return -1;
    }

    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PROC, 0};
    size_t buf_size = 0;
    if (sysctl(mib, 4, NULL, &buf_size, NULL, 0) != 0) {
        if (err_out) *err_out = "proc_list_sysctl_size_failed";
        return -1;
    }
    if (buf_size == 0) {
        if (err_out) *err_out = "proc_list_sysctl_empty";
        return -1;
    }
    /* The proc list can grow between the size-probe and the read.
     * Pad ~25% headroom upfront and retry the read once with
     * doubled capacity if the kernel still says ENOMEM. */
    size_t alloc = buf_size + (buf_size / 4) + 1024;
    uint8_t *kbuf = (uint8_t *)malloc(alloc);
    if (!kbuf) {
        if (err_out) *err_out = "proc_list_alloc_failed";
        return -1;
    }
    size_t got = alloc;
    if (sysctl(mib, 4, kbuf, &got, NULL, 0) != 0) {
        free(kbuf);
        alloc = alloc * 2;
        kbuf = (uint8_t *)malloc(alloc);
        if (!kbuf) {
            if (err_out) *err_out = "proc_list_alloc_failed";
            return -1;
        }
        got = alloc;
        if (sysctl(mib, 4, kbuf, &got, NULL, 0) != 0) {
            free(kbuf);
            if (err_out) *err_out = "proc_list_sysctl_read_failed";
            return -1;
        }
    }

    size_t w = 0;
    int emitted = 0;
    int truncated = 0;
    {
        const char *head = "{\"ok\":true,\"procs\":[";
        size_t hl = strlen(head);
        if (w + hl >= cap) {
            free(kbuf);
            if (err_out) *err_out = "proc_list_buf_too_small";
            return -1;
        }
        memcpy(buf + w, head, hl);
        w += hl;
    }

    /* Minimum kinfo_proc size needed to read both pid and tdname.
     * tdname (offset 447) is the highest-offset field we touch;
     * any entry shorter than that is malformed and we skip it. */
    const size_t MIN_KINFO_BYTES = KINFO_TDNAME_OFFSET + 1;
    for (uint8_t *p = kbuf; (size_t)(p - kbuf) + sizeof(int) <= got;) {
        int ki_structsize = *(int *)p;
        if (ki_structsize <= 0 ||
            (size_t)ki_structsize < MIN_KINFO_BYTES ||
            (size_t)(p - kbuf) + (size_t)ki_structsize > got) {
            /* Malformed entry — bail rather than walk into garbage. */
            break;
        }

        pid_t pid = *(pid_t *)&p[KINFO_PID_OFFSET];
        const char *tdname = (const char *)&p[KINFO_TDNAME_OFFSET];

        char esc[64];
        size_t esc_len = json_escape_name(tdname,
                                          (size_t)ki_structsize - KINFO_TDNAME_OFFSET,
                                          esc, sizeof(esc));

        /* Larger entry buffer for the detailed path (adds comm, memory,
         * threads, title id, kind). The compact path stays at the original
         * shape so existing diagnostics consumers see byte-identical output. */
        char entry[320];
        int entry_len;
        if (!detailed) {
            entry_len = snprintf(entry, sizeof(entry),
                                 "%s{\"pid\":%d,\"name\":\"%.*s\"}",
                                 emitted ? "," : "",
                                 (int)pid,
                                 (int)esc_len, esc);
        } else {
            /* The SDK struct matches the proven offsets (see _Static_assert),
             * so reading ki_comm/ki_rssize/ki_numthreads by field is safe. */
            const struct kinfo_proc *ki = (const struct kinfo_proc *)p;
            char comm_esc[64];
            size_t comm_len = json_escape_name(ki->ki_comm,
                                               sizeof(ki->ki_comm),
                                               comm_esc, sizeof(comm_esc));
            app_info_t appinfo;
            if (sceKernelGetAppInfo(pid, &appinfo) != 0) {
                memset(&appinfo, 0, sizeof(appinfo));
            }
            /* title_id from the struct is a fixed char[14], not guaranteed
             * NUL-terminated; bound + escape it. */
            char tid_esc[32];
            json_escape_name(appinfo.title_id, sizeof(appinfo.title_id),
                             tid_esc, sizeof(tid_esc));
            /* tid_esc is NUL-terminated and prefix-checkable; pass it so the
             * NPXS* system-app check works without re-reading the raw field. */
            const char *kind =
                proc_kind(ki->ki_comm, appinfo.app_id, tid_esc);
            /* Resident memory: pages → MiB. getpagesize() returns the live
             * value (PS5 is 16 KiB pages, not the amd64-default 4 KiB). */
            double mem_mib =
                ((double)ki->ki_rssize * (double)getpagesize()) / (1024.0 * 1024.0);
            /* Flag the helper's OWN process. proc_kill refuses to kill it
             * (pid == getpid() → EPERM), so without this the UI would offer a
             * Kill button that always fails with a confusing "Operation not
             * permitted" — the user reported exactly that. The UI disables
             * Kill/Restart for is_self and explains why. */
            int is_self = (pid == getpid());
            entry_len = snprintf(entry, sizeof(entry),
                                 "%s{\"pid\":%d,\"name\":\"%.*s\",\"comm\":\"%.*s\","
                                 "\"title_id\":\"%s\",\"app_id\":%u,"
                                 "\"memory_mib\":%.1f,\"threads\":%d,\"kind\":\"%s\","
                                 "\"is_self\":%s}",
                                 emitted ? "," : "",
                                 (int)pid,
                                 (int)esc_len, esc,
                                 (int)comm_len, comm_esc,
                                 tid_esc, appinfo.app_id,
                                 mem_mib, (int)ki->ki_numthreads, kind,
                                 is_self ? "true" : "false");
        }
        if (entry_len <= 0 || (size_t)entry_len >= sizeof(entry)) {
            p += ki_structsize;
            continue;
        }

        if (w + (size_t)entry_len + TAIL_RESERVE >= cap) {
            truncated = 1;
            break;
        }
        memcpy(buf + w, entry, (size_t)entry_len);
        w += (size_t)entry_len;
        emitted += 1;

        p += ki_structsize;
    }

    free(kbuf);

    {
        char tail_buf[64];
        int tl;
        if (truncated) {
            tl = snprintf(tail_buf, sizeof(tail_buf),
                          "%s{\"truncated\":true}]}",
                          emitted ? "," : "");
        } else {
            tl = snprintf(tail_buf, sizeof(tail_buf), "]}");
        }
        if (tl <= 0 || (size_t)tl >= sizeof(tail_buf)) {
            if (err_out) *err_out = "proc_list_close_overflow";
            return -1;
        }
        if (w + (size_t)tl >= cap) {
            if (err_out) *err_out = "proc_list_close_overflow";
            return -1;
        }
        memcpy(buf + w, tail_buf, (size_t)tl);
        w += (size_t)tl;
    }
    buf[w] = '\0';
    *written_out = w;
    return 0;
}

/* Compact list — pid + name only. Byte-identical to the original output so
 * existing diagnostics ("ps") consumers are unaffected. */
int proc_list_get_json(char *buf, size_t cap, size_t *written_out,
                       const char **err_out) {
    return proc_list_build(buf, cap, written_out, err_out, 0);
}

/* Detailed list — adds comm, title id, app id, memory (MiB), thread count,
 * and a kind classification for the in-app process manager. */
int proc_list_get_json_ex(char *buf, size_t cap, size_t *written_out,
                          const char **err_out) {
    return proc_list_build(buf, cap, written_out, err_out, 1);
}

/* SIGKILL a process by pid, with guards against self-destruction and the
 * obviously-fatal low pids. Returns 0 on success, -1 on guard trip or a
 * failed kill (ESRCH/EPERM). The UI is responsible for warning the user
 * before killing a process it classified as "system"; the payload trusts
 * the confirmed request but still refuses to kill itself, the kernel/init
 * (pid 0/1), or a negative/process-group target. */
int proc_kill(int pid) {
    /* Set errno on guard trips too, so the caller (runtime.c) can report a
     * meaningful reason for EVERY failure — guard, ESRCH (already gone), or
     * EPERM (kernel refused) — instead of a bare "kill_failed". */
    if (pid <= 1) {                 /* 0 = kernel idle, 1 = init, <0 = pgrp */
        errno = EPERM;
        return -1;
    }
    if (pid == (int)getpid()) {     /* never kill the helper itself */
        errno = EPERM;
        return -1;
    }
    return kill(pid, SIGKILL) == 0 ? 0 : -1;  /* errno set by kill() on failure */
}

int proc_name_by_pid(int pid, char *out, size_t cap) {
    if (!out || cap == 0) return -1;
    out[0] = '\0';
    if (pid <= 0) return -1;

    /* Query just this pid. One kinfo_proc is ~1.1 KiB on 64-bit FreeBSD;
     * a 2 KiB staging buffer covers it with headroom. */
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, pid};
    uint8_t kbuf[2048];
    size_t got = sizeof(kbuf);
    if (sysctl(mib, 4, kbuf, &got, NULL, 0) != 0) {
        return -1; /* ESRCH (gone) or ENOMEM (shouldn't happen at 2 KiB) */
    }
    if (got < (size_t)(KINFO_TDNAME_OFFSET + 1)) return -1;
    int ki_structsize = *(int *)kbuf;
    if (ki_structsize <= 0 ||
        (size_t)ki_structsize < (size_t)(KINFO_TDNAME_OFFSET + 1) ||
        (size_t)ki_structsize > got) {
        return -1;
    }
    const char *tdname = (const char *)&kbuf[KINFO_TDNAME_OFFSET];
    size_t name_max = (size_t)ki_structsize - KINFO_TDNAME_OFFSET;
    size_t i = 0;
    for (; i < name_max && i + 1 < cap && tdname[i]; ++i) out[i] = tdname[i];
    out[i] = '\0';
    return 0;
}
