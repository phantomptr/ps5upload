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

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <sys/sysctl.h>
#include <sys/types.h>

/* Layout offsets inside FreeBSD's kinfo_proc as exposed via
 * sysctl(KERN_PROC_PROC). Same offsets shellui_rpc.c uses. */
#define KINFO_PID_OFFSET     72
#define KINFO_TDNAME_OFFSET  447

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

int proc_list_get_json(char *buf, size_t cap, size_t *written_out,
                       const char **err_out) {
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

        char entry[128];
        int entry_len = snprintf(entry, sizeof(entry),
                                 "%s{\"pid\":%d,\"name\":\"%.*s\"}",
                                 emitted ? "," : "",
                                 (int)pid,
                                 (int)esc_len, esc);
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
