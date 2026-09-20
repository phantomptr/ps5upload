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
#include "proc_identity.h"

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

/* Layout and the title-id helpers live in app_info.h — this struct was
 * duplicated four times and every copy had the offset wrong. */
#include "app_info.h"

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

void proc_log_homebrew_neighbours(void) {
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PROC, 0};
    size_t buf_size = 0;
    if (sysctl(mib, 4, NULL, &buf_size, NULL, 0) != 0 || buf_size == 0) {
        fprintf(stderr, "[payload2] neighbours: sysctl unavailable\n");
        return;
    }

    /* 25% headroom + 1 KiB padding — same growth strategy the rest of this
     * file uses, because the proc list can grow between the sizing call and
     * the fetch. */
    size_t alloc = buf_size + (buf_size / 4) + 1024;
    uint8_t *kbuf = (uint8_t *)malloc(alloc);
    if (!kbuf) return;

    size_t got = alloc;
    if (sysctl(mib, 4, kbuf, &got, NULL, 0) != 0) {
        free(kbuf);
        fprintf(stderr, "[payload2] neighbours: sysctl fetch failed\n");
        return;
    }

    const size_t MIN_KINFO_BYTES = KINFO_TDNAME_OFFSET + 1;
    int count = 0;
    int generic = 0;

    fprintf(stderr, "[payload2] neighbours: homebrew processes on this console\n");
    for (uint8_t *p = kbuf; (size_t)(p - kbuf) + sizeof(int) <= got;) {
        int ki_structsize = *(int *)p;
        if (ki_structsize <= 0 ||
            (size_t)ki_structsize < MIN_KINFO_BYTES ||
            (size_t)(p - kbuf) + (size_t)ki_structsize > got) {
            break;
        }
        pid_t pid = *(pid_t *)&p[KINFO_PID_OFFSET];
        const char *tdname = (const char *)&p[KINFO_TDNAME_OFFSET];
        size_t name_max = (size_t)ki_structsize - KINFO_TDNAME_OFFSET;

        /* Bounded copy: tdname is not guaranteed NUL-terminated within the
         * record. */
        char name[64] = {0};
        size_t i = 0;
        for (; i < name_max && i + 1 < sizeof(name) && tdname[i]; ++i) {
            name[i] = tdname[i];
        }
        name[i] = '\0';

        p += (size_t)ki_structsize;

        size_t len = strlen(name);
        int is_elf = (len > 4 && strcmp(name + len - 4, ".elf") == 0);
        if (!is_elf && !proc_name_is_ours(name)) continue;

        int mine = proc_name_is_ours(name);
        if (strcmp(name, "payload.elf") == 0) generic++;
        fprintf(stderr, "[payload2]   pid=%d name=%s%s\n",
                (int)pid, name, mine ? " (ours)" : "");
        count++;
    }
    free(kbuf);

    fprintf(stderr, "[payload2] neighbours: %d homebrew process(es)\n", count);
    if (generic > 0) {
        fprintf(stderr,
                "[payload2] neighbours: %d process(es) named 'payload.elf' — any payload "
                "running the scene's kill-my-predecessor-by-name sweep will SIGKILL "
                "them all\n",
                generic);
    }
}

/* Find the first process whose thread-name matches `name`, via
 * sysctl(KERN_PROC_PROC). Returns the pid (>0) on success, -1 if no
 * match or sysctl fails. Same kinfo_proc walk pattern used everywhere
 * else in this file — no kernel R/W needed. Used by the Remote Play
 * daemon reset path to locate "SceRemotePlay" before SIGKILL'ing it. */
/* Is a title currently running? Walks the same sysctl(KERN_PROC_PROC)
 * snapshot as proc_find_pid_by_name, but matches on the app's TITLE ID
 * (via sceKernelGetAppInfo) rather than the thread name.
 *
 * Exists so launch_title can tell "that attempt actually started the game"
 * from "that attempt failed". Sony's launch APIs can return non-zero for a
 * launch that is in fact proceeding; without this check the fallback ladder
 * fired a SECOND launch at a title that was already coming up, and the shell
 * responded by bouncing the game straight back to the background — the
 * "it starts then immediately minimises" report.
 *
 * Returns the pid (>0) when a process for `title_id` exists, -1 otherwise. */
/* App id (not pid) of a running title, or 0 when it isn't running.
 *
 * Sony's focus and kill APIs are keyed on the APP id from
 * sceKernelGetAppInfo, which is a different number from the pid. launch_title
 * needs it to hand focus to a game it has just started. */
unsigned int proc_app_id_by_title_id(const char *title_id) {
    if (!title_id || !title_id[0]) return 0;

    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PROC, 0};
    size_t buf_size = 0;
    if (sysctl(mib, 4, NULL, &buf_size, NULL, 0) != 0) return 0;
    if (buf_size == 0) return 0;

    uint8_t *buf = (uint8_t *)malloc(buf_size);
    if (!buf) return 0;
    if (sysctl(mib, 4, buf, &buf_size, NULL, 0) != 0) {
        free(buf);
        return 0;
    }

    unsigned int found = 0;
    for (uint8_t *ptr = buf; ptr < buf + buf_size;) {
        int ki_structsize = *(int *)ptr;
        if (ki_structsize <= 0 ||
            (size_t)(ptr - buf) + (size_t)ki_structsize > buf_size) break;
        if (ki_structsize <= KINFO_PID_OFFSET) break;
        pid_t ki_pid = *(pid_t *)&ptr[KINFO_PID_OFFSET];
        app_info_t info;
        if (sceKernelGetAppInfo(ki_pid, &info) == 0) {
            char tid[sizeof(info.title_id) + 1];
            memcpy(tid, info.title_id, sizeof(info.title_id));
            tid[sizeof(info.title_id)] = '\0';
            if (strcmp(tid, title_id) == 0 && info.app_id != 0) {
                found = info.app_id;
                break;
            }
        }
        ptr += ki_structsize;
    }
    free(buf);
    return found;
}

int proc_find_pid_by_title_id(const char *title_id) {
    if (!title_id || !title_id[0]) return -1;

    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PROC, 0};
    size_t buf_size = 0;
    if (sysctl(mib, 4, NULL, &buf_size, NULL, 0) != 0) return -1;
    if (buf_size == 0) return -1;

    uint8_t *buf = (uint8_t *)malloc(buf_size);
    if (!buf) return -1;
    if (sysctl(mib, 4, buf, &buf_size, NULL, 0) != 0) {
        free(buf);
        return -1;
    }

    int found = -1;
    for (uint8_t *ptr = buf; ptr < buf + buf_size;) {
        int ki_structsize = *(int *)ptr;
        if (ki_structsize <= 0 ||
            (size_t)(ptr - buf) + (size_t)ki_structsize > buf_size) break;
        if (ki_structsize <= KINFO_PID_OFFSET) break;
        pid_t ki_pid = *(pid_t *)&ptr[KINFO_PID_OFFSET];
        app_info_t info;
        if (sceKernelGetAppInfo(ki_pid, &info) == 0) {
            /* title_id is a fixed char[14] with no NUL guarantee. */
            char tid[sizeof(info.title_id) + 1];
            memcpy(tid, info.title_id, sizeof(info.title_id));
            tid[sizeof(info.title_id)] = '\0';
            if (strcmp(tid, title_id) == 0) {
                found = (int)ki_pid;
                break;
            }
        }
        ptr += ki_structsize;
    }
    free(buf);
    return found;
}

int proc_find_pid_by_name(const char *name) {
    if (!name || !name[0]) return -1;

    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PROC, 0};
    size_t buf_size = 0;
    if (sysctl(mib, 4, NULL, &buf_size, NULL, 0) != 0) return -1;
    if (buf_size == 0) return -1;

    uint8_t *buf = (uint8_t *)malloc(buf_size);
    if (!buf) return -1;
    if (sysctl(mib, 4, buf, &buf_size, NULL, 0) != 0) {
        free(buf);
        return -1;
    }

    int found = -1;
    for (uint8_t *ptr = buf; ptr < buf + buf_size;) {
        int ki_structsize = *(int *)ptr;
        if (ki_structsize <= 0 ||
            (size_t)(ptr - buf) + (size_t)ki_structsize > buf_size) break;
        if (ki_structsize <= KINFO_TDNAME_OFFSET) break;
        pid_t ki_pid = *(pid_t *)&ptr[KINFO_PID_OFFSET];
        const char *ki_tdname = (const char *)&ptr[KINFO_TDNAME_OFFSET];
        if (strcmp(ki_tdname, name) == 0) {
            found = (int)ki_pid;
            break;
        }
        ptr += ki_structsize;
    }
    free(buf);
    return found;
}

/* See proc_list.h. Behavioural focus detection: sample this over time and a
 * backgrounded title shows up as a collapse in the runtime_us rate. */
int proc_list_app_states_json(char *buf, size_t cap, size_t *written_out,
                              const char **err_out) {
    int mib[3] = { CTL_KERN, KERN_PROC, KERN_PROC_PROC };
    size_t buf_size = 0;
    uint8_t *procbuf = NULL;
    size_t off = 0;
    int n, emitted = 0;

    if (!buf || cap == 0) {
        if (err_out) *err_out = "app_states_bad_args";
        return -1;
    }
    if (sysctl(mib, 3, NULL, &buf_size, NULL, 0) != 0 || buf_size == 0) {
        if (err_out) *err_out = "app_states_sysctl_size_failed";
        return -1;
    }
    /* Headroom: the process table can grow between the sizing call and the
     * read, same as proc_list_build. */
    buf_size += buf_size / 4;
    procbuf = (uint8_t *)malloc(buf_size);
    if (!procbuf) {
        if (err_out) *err_out = "app_states_alloc_failed";
        return -1;
    }
    if (sysctl(mib, 3, procbuf, &buf_size, NULL, 0) != 0) {
        free(procbuf);
        if (err_out) *err_out = "app_states_sysctl_read_failed";
        return -1;
    }

    n = snprintf(buf, cap, "[");
    if (n < 0 || (size_t)n >= cap) {
        free(procbuf);
        if (err_out) *err_out = "app_states_buf_too_small";
        return -1;
    }
    off = (size_t)n;

    for (uint8_t *ptr = procbuf; ptr < procbuf + buf_size;) {
        int ki_structsize = *(int *)ptr;
        if (ki_structsize <= 0 ||
            (size_t)(ptr - procbuf) + (size_t)ki_structsize > buf_size) break;
        if (ki_structsize <= KINFO_TDNAME_OFFSET) break;

        const struct kinfo_proc *ki = (const struct kinfo_proc *)ptr;
        pid_t pid = *(pid_t *)&ptr[KINFO_PID_OFFSET];

        app_info_t appinfo;
        if (sceKernelGetAppInfo(pid, &appinfo) != 0) {
            memset(&appinfo, 0, sizeof(appinfo));
        }
        /* Only real apps/games — app_id 0 is every daemon on the system and
         * would bury the two rows that matter. */
        if (appinfo.app_id != 0) {
            char tid_esc[32];
            json_escape_name(appinfo.title_id, sizeof(appinfo.title_id),
                             tid_esc, sizeof(tid_esc));
            n = snprintf(buf + off, cap - off,
                         "%s{\"pid\":%d,\"title_id\":\"%s\",\"app_id\":%u,"
                         "\"stat\":%d,\"runtime_us\":%llu,\"pctcpu\":%u,"
                         "\"nthreads\":%d,\"slptime\":%u,\"swtime\":%u}",
                         emitted ? "," : "",
                         (int)pid, tid_esc, (unsigned)appinfo.app_id,
                         (int)ki->ki_stat,
                         (unsigned long long)ki->ki_runtime,
                         (unsigned)ki->ki_pctcpu,
                         (int)ki->ki_numthreads,
                         (unsigned)ki->ki_slptime,
                         (unsigned)ki->ki_swtime);
            if (n < 0 || (size_t)n >= cap - off) {
                free(procbuf);
                if (err_out) *err_out = "app_states_buf_too_small";
                return -1;
            }
            off += (size_t)n;
            emitted++;
        }
        ptr += ki_structsize;
    }
    free(procbuf);

    n = snprintf(buf + off, cap - off, "]");
    if (n < 0 || (size_t)n >= cap - off) {
        if (err_out) *err_out = "app_states_buf_too_small";
        return -1;
    }
    off += (size_t)n;
    if (written_out) *written_out = off;
    return 0;
}
