#ifndef PS5UPLOAD_APP_INFO_H
#define PS5UPLOAD_APP_INFO_H

/* sceKernelGetAppInfo(pid, &info) — app id and title id for a process.
 *
 * ── Why this header exists ──────────────────────────────────────────
 *
 * The struct was once copy-pasted into four files with two different
 * layouts; activity.c used `uint8_t unk[0x40]` and read title_id at
 * offset 64. This is now the single definition.
 *
 * ── title_id is at offset 16, measured on hardware. ─────────────────
 *
 * 5.4.5 moved title_id to offset 20, because etaHEN, one onionHEN copy and
 * the SDK's `ps` sample declare a `uint32_t app_type` before it. That change
 * was justified by "82 processes, zero title ids" — measured against
 * PROC_LIST (74), which returns only pid and name and never carried a title
 * id at all. Wrong endpoint, confident wrong conclusion.
 *
 * Measured properly against PROCESS_LIST (162) on FW 5.10 and FW 9.60,
 * offset 20 returned "40087" for "NPXS40087" and "19534" for "PPSA19534" —
 * the last five characters, exactly a four-byte overshoot. f1ffeb65 reverted
 * to 16 and the _Static_assert below pins it. onionHEN (four of its five
 * copies) and kstuff-lite also use offset 16.
 *
 * Whichever layout, the struct is 0x60 bytes — the same size every one of
 * those references passes to sceKernelGetAppInfo — so the kernel's write
 * cannot overflow it. It is not a stack-overflow risk.
 *
 * A returned value of non-zero means the pid is not a registered app
 * (a daemon or system process); the struct contents are meaningless in
 * that case and callers must not read them.
 */

#include <stdint.h>
#include <stddef.h>
#include <sys/types.h>

typedef struct app_info {
    uint32_t app_id;
    uint64_t unknown1;
    /* Offset 16. Measured, not assumed -- see the note above.
     * The field is wider than the value it holds, so do not
     * assume the kernel terminated it. */
    char     title_id[14];
    char     unknown2[0x3c];
} app_info_t;

/* The whole bug was a wrong offset, so pin it. If someone reorders or
 * drops a field again, this fails at compile time rather than silently
 * returning empty title ids on a console. */
_Static_assert(offsetof(app_info_t, app_id) == 0,
               "app_id must be first");
_Static_assert(offsetof(app_info_t, title_id) == 16,
               "title_id is at offset 16 on the firmwares this was "
               "measured on; offset 20 returns only the last five "
               "characters of the real value");

extern int sceKernelGetAppInfo(pid_t pid, app_info_t *info);

/* True when `s` looks like a real title id: four upper-case letters
 * then five digits (CUSA12345, PPSA01234, NPXS40000).
 *
 * Checked before any title id is used, so that if this struct is ever
 * wrong again the result is "no title" rather than garbage flowing into
 * cheat lookups and the UI. */
static inline int app_info_title_id_valid(const char *s) {
    if (!s) return 0;
    for (int i = 0; i < 4; i++)
        if (s[i] < 'A' || s[i] > 'Z') return 0;
    for (int i = 4; i < 9; i++)
        if (s[i] < '0' || s[i] > '9') return 0;
    return s[9] == '\0';
}

/* Copy the title id out as a NUL-terminated string. Returns 1 and fills
 * `out` when the process is an app with a plausible title id, else 0
 * and leaves `out` an empty string. */
static inline int app_info_title_id(const app_info_t *info,
                                    char *out, size_t out_sz) {
    if (!out || out_sz < 10) return 0;
    out[0] = '\0';
    if (!info) return 0;
    char tid[10];
    for (int i = 0; i < 9; i++) tid[i] = info->title_id[i];
    tid[9] = '\0';
    if (!app_info_title_id_valid(tid)) return 0;
    for (int i = 0; i < 10; i++) out[i] = tid[i];
    return 1;
}

#endif /* PS5UPLOAD_APP_INFO_H */
