/*
 * proc_list.c — walk the FreeBSD allproc list and emit a JSON process
 * summary. Reads kernel memory via the SDK's `kernel_copyout` primitive
 * (which relies on the payload loader having handed us kernel R/W via
 * the payload_args_t struct at startup). Writes are never performed.
 *
 * Design notes:
 *
 *  - Kernel struct offsets come from the SDK (<ps5/kernel.h> exposes
 *    `KERNEL_OFFSET_PROC_P_PID`, etc.). Those are firmware-resolved
 *    at payload startup, so the same binary targets PS5 9.x–12.x
 *    without source edits when built against SDK v0.38+.
 *
 *  - `p_comm` (process name) is NOT in the SDK's exported table, so we
 *    maintain our own per-firmware table via `ps5upload_p_comm_offset()`.
 *    On an unsupported firmware the function returns 0 and we emit
 *    placeholder `"<pid:N>"` names rather than reading a wrong offset
 *    (which would return garbage or page-fault).
 *
 *  - The walk is capped at `MAX_PROCS_SCANNED` as a defensive cap:
 *    if the head pointer is corrupt or we follow a cycle, we bail
 *    rather than looping forever. Real PS5 process count is in the
 *    60–120 range; 1024 is plenty.
 *
 *  - We never malloc. The output buffer is supplied by the caller
 *    (runtime.c owns the mgmt-response staging area) and we bound
 *    each entry's JSON expansion so we don't overflow. If we run
 *    out of room we truncate with a trailing "...(truncated)" note
 *    rather than failing the whole call, since a truncated list is
 *    still useful.
 */

#include "proc_list.h"
#include "kstruct.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <ps5/kernel.h>

/* Safety cap on list traversal depth. See design notes above. */
#define MAX_PROCS_SCANNED   1024

/* Reserve a little room at the end of the buffer for the closing
 * `]}` + a possible truncation marker so we never emit malformed
 * JSON even if we hit the cap mid-entry. */
#define TAIL_RESERVE        64u

/*
 * Per-firmware `p_comm` (process-name) offset table.
 *
 * The SDK's `kernel_get_fw_version()` returns the firmware as a BCD
 * word like `0x09600000` for 9.60, `0x11000000` for 11.00, etc. — we
 * care only about the major nibble (high byte), which we get via
 * `fw >> 24`. See the `test_privileges` sample in the SDK.
 *
 * Extend this table when a new firmware ships. An entry of 0 means
 * "we don't know the offset for this firmware" — the caller emits
 * placeholder names instead of reading.
 */
static off_t p_comm_offset_for_major(uint8_t major) {
    switch (major) {
        case 9:
        case 10:
        case 11:
            /* Validated on hardware through PS5 11.x. The offset has
             * been stable across this range because Sony kept the
             * FreeBSD-11-derived `struct proc` layout. */
            return 0x59C;
        case 12:
            /* TBD: Sony bumped `struct proc` layout somewhere in the
             * 12.x branch. Return 0 until the correct offset is
             * confirmed on real hardware. Rest of PROC_LIST (pid,
             * ucred pointer) still works on 12.x because those
             * offsets come from the SDK's firmware-resolved
             * KERNEL_OFFSET_* symbols. */
            return 0;
        default:
            return 0;
    }
}

off_t ps5upload_p_comm_offset(void) {
    uint32_t fw = kernel_get_fw_version();
    uint8_t major = (uint8_t)((fw >> 24) & 0xFF);
    return p_comm_offset_for_major(major);
}

/* Read `len` bytes from kernel addr `kaddr` into `out`. Returns 0 on
 * success, non-zero on failure. Wraps `kernel_copyout` so the caller
 * doesn't need to remember which direction the copy goes. */
static int k_read(intptr_t kaddr, void *out, size_t len) {
    if (kaddr == 0) return -1;
    return kernel_copyout(kaddr, out, len) == 0 ? 0 : -1;
}

/* JSON-escape a process name into `dst`. PS5 process names are plain
 * ASCII in practice (FreeBSD p_comm is strictly ASCII-safe), but we
 * still quote " and \ for safety and replace anything below 0x20 with
 * '?' rather than emitting a raw control character. Writes at most
 * `dst_cap-1` bytes and always NUL-terminates; returns the number of
 * bytes written (excluding the NUL). */
static size_t json_escape_name(const char *name, size_t name_len,
                               char *dst, size_t dst_cap) {
    size_t i = 0;
    size_t w = 0;
    if (dst_cap == 0) return 0;
    for (i = 0; i < name_len && w + 2 < dst_cap; ++i) {
        unsigned char c = (unsigned char)name[i];
        if (c == 0) break;  /* truncate at first NUL — p_comm is NUL-padded */
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

/* Render a fallback name when we don't have a `p_comm` offset for the
 * running firmware. Shape: "<pid:N>" — distinctive enough that a
 * reader can tell "name is a placeholder" at a glance, short enough
 * that it fits in the same JSON entry budget as a real name. */
static size_t render_fallback_name(int pid, char *dst, size_t dst_cap) {
    int n = snprintf(dst, dst_cap, "<pid:%d>", pid);
    if (n < 0) return 0;
    if ((size_t)n >= dst_cap) return dst_cap - 1;
    return (size_t)n;
}

int proc_list_get_json(char *buf, size_t cap, size_t *written_out,
                       const char **err_out) {
    intptr_t cur = 0;
    size_t w = 0;
    int count = 0;
    int emitted = 0;
    int truncated = 0;
    off_t comm_offset = 0;

    if (!buf || cap < 64 || !written_out) {
        if (err_out) *err_out = "proc_list_bad_args";
        return -1;
    }
    if (KERNEL_ADDRESS_ALLPROC == 0) {
        /* SDK didn't populate the allproc address — this happens when
         * the payload wasn't loaded by a loader that provides kernel
         * R/W. Surface that directly rather than silently returning
         * an empty list. */
        if (err_out) *err_out = "kernel_rw_unavailable";
        return -1;
    }

    /* p_comm offset is zero on unsupported firmware (e.g., 12.x today).
     * That's not a hard error: we still emit PROC_LIST entries with
     * pid + a placeholder name. A caller that needs real names can
     * pair PID with Sony's own `ps`-equivalent off the shell, or we
     * bring up support once the offset is confirmed. */
    comm_offset = ps5upload_p_comm_offset();

    /* allproc is a LIST_HEAD; its lh_first field is the first struct
     * proc *. That pointer is what we walk. On the SDK build, the
     * exported symbol IS the pointer, so we read one pointer out of
     * the global and start walking from there. */
    if (k_read(KERNEL_ADDRESS_ALLPROC, &cur, sizeof(cur)) != 0) {
        if (err_out) *err_out = "allproc_read_failed";
        return -1;
    }

    /* Opening JSON. We emit `{"ok":true,"procs":[` first; as we walk
     * the list we append one object per proc; then close with `]}`.
     * On truncation we substitute a ",{\"truncated\":true}]}" tail. */
    {
        const char *head = "{\"ok\":true,\"procs\":[";
        size_t hl = strlen(head);
        if (w + hl >= cap) {
            if (err_out) *err_out = "proc_list_buf_too_small";
            return -1;
        }
        memcpy(buf + w, head, hl);
        w += hl;
    }

    while (cur != 0 && count < MAX_PROCS_SCANNED) {
        int pid = 0;
        char comm[PROC_COMM_LEN];
        intptr_t next = 0;
        char esc[PROC_COMM_LEN * 2 + 1];
        size_t esc_len = 0;
        /* Per-entry JSON fits in ~64 bytes worst case; budget 128 to
         * be generous. If we don't have that much room left, trigger
         * truncation path below. */
        char entry[128];
        int entry_len = 0;

        if (k_read(cur + KERNEL_OFFSET_PROC_P_PID, &pid, sizeof(pid)) != 0 ||
            k_read(cur + PROC_FIELD_NEXT, &next, sizeof(next)) != 0) {
            /* One entry in the middle went sideways. Stop here rather
             * than keep reading — a bad next pointer could loop us
             * forever or page-fault. */
            truncated = 1;
            break;
        }

        if (comm_offset != 0) {
            /* Firmware supported: read the actual process name. */
            if (k_read(cur + comm_offset, comm, sizeof(comm)) != 0) {
                /* Shouldn't normally happen if pid read succeeded, but
                 * guard anyway — one bad read shouldn't kill the scan. */
                memset(comm, 0, sizeof(comm));
            }
            esc_len = json_escape_name(comm, sizeof(comm), esc, sizeof(esc));
        } else {
            /* Firmware p_comm offset unknown: emit "<pid:N>" placeholder. */
            esc_len = render_fallback_name(pid, esc, sizeof(esc));
        }

        /* snprintf returns would-have-been length; negative on error,
         * which we treat as "skip this entry". */
        entry_len = snprintf(entry, sizeof(entry),
                             "%s{\"pid\":%d,\"name\":\"%.*s\"}",
                             emitted ? "," : "",
                             pid,
                             (int)esc_len, esc);
        if (entry_len <= 0 || (size_t)entry_len >= sizeof(entry)) {
            /* Unlikely — name is at most 62 bytes after escaping.
             * Skip rather than corrupt the stream. */
            cur = next;
            count += 1;
            continue;
        }

        if (w + (size_t)entry_len + TAIL_RESERVE >= cap) {
            truncated = 1;
            break;
        }
        memcpy(buf + w, entry, (size_t)entry_len);
        w += (size_t)entry_len;
        emitted += 1;

        cur = next;
        count += 1;
    }

    if (count >= MAX_PROCS_SCANNED) {
        truncated = 1;
    }

    /* Close the JSON. When truncated, add a sentinel object so the
     * UI can badge "list was cut short" without counting elements.
     * When names are placeholders (unknown-firmware path), add a
     * `names_unavailable:true` hint so the UI can explain why every
     * entry is a "<pid:N>" instead of a real command name. */
    {
        char tail_buf[96];
        int tl = 0;
        int want_hint = (comm_offset == 0);
        if (truncated && want_hint) {
            tl = snprintf(tail_buf, sizeof(tail_buf),
                          "%s{\"truncated\":true,\"names_unavailable\":true}]}",
                          emitted ? "," : "");
        } else if (truncated) {
            tl = snprintf(tail_buf, sizeof(tail_buf),
                          "%s{\"truncated\":true}]}",
                          emitted ? "," : "");
        } else if (want_hint) {
            tl = snprintf(tail_buf, sizeof(tail_buf),
                          "%s{\"names_unavailable\":true}]}",
                          emitted ? "," : "");
        } else {
            tl = snprintf(tail_buf, sizeof(tail_buf), "]}");
        }
        if (tl <= 0 || (size_t)tl >= sizeof(tail_buf)) {
            if (err_out) *err_out = "proc_list_close_overflow";
            return -1;
        }
        if (w + (size_t)tl >= cap) {
            /* Fall back to an unterminated error — this would be a
             * programming mistake (TAIL_RESERVE should prevent it),
             * so surface it loudly. */
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
