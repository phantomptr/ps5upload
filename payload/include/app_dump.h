/* Dump an installed, running title to a drive on the console (JOB_OP_APP_DUMP).
 *
 * A port of ps5-app-dumper (github.com/EchoStretch/ps5-app-dumper, EchoStretch and slopmaster33,
 * GPLv3-or-later) and of idlesauce's ps5-self-pager (github.com/idlesauce/ps5-self-pager, The
 * Unlicense) for the SELF decryption. Its web UI, queue, launcher and notifications are not ported:
 * the engine launches the title and shows the progress.
 *
 * JOB_OP_APP_DUMP {"title_id":"PPSA01234","dest":"/mnt/usb0/ps5upload-dumps","decrypt":false,
 *   "fself":false,"dlc":true}
 *   The title must be mounted (running): <sandbox>/<ID>-app0, else "dump_not_running". It is written
 *   to <dest>/<ID>-app (PS5) or <dest>/<ID> (PS4, the app and its patch merged as ps5-app-dumper's
 *   unsplit layout does); an existing folder is "dump_exists", a drive without room "dump_no_space".
 *   PS5: the package's sce_sys entries, the mounted app0, the appmeta (user and system), trophy and
 *   UDS files. PS4: the package's and the patch package's sce_sys entries, the nested PFS images (or
 *   the mounted tree), nptitle/npbind and the trophy. With "dlc", each mounted DLC (<...>-ac) goes
 *   beside it as <dest>/<folder> with its ac.pkg entries and appmeta. With "decrypt", every
 *   .elf/.self/.prx/.sprx/.bin is decrypted in place (one segment window at a time; a file that
 *   fails is counted, not fatal; a firmware with no pager entry skips decryption with
 *   "dump_decrypt_unsupported_fw"); with "fself" too, each decrypted ELF is kept under
 *   <out>/decrypted/ and the dump gets a fake-signed SELF in its place.
 *   Stop: the partial folder stays, with dump-incomplete.txt in it ("dump_cancelled").
 *   Result: {"path","files","bytes","decrypted","decrypt_failed","decrypt_skipped","dlc":[..],"fw"},
 *   also written to <out>/dump-info.json with "title_id" and "finished" (unix seconds).
 *   One dump at a time ("dump_busy").
 */
#ifndef PS5UPLOAD_APP_DUMP_H
#define PS5UPLOAD_APP_DUMP_H

#include <stddef.h>
#include <stdint.h>

#define APP_DUMP_MAX_DLC 32
#define APP_DUMP_PATH_MAX 1024

typedef struct {
    char title_id[16];
    char dest[APP_DUMP_PATH_MAX];
    int decrypt, fself, dlc;
} app_dump_args_t;

typedef struct {
    char path[APP_DUMP_PATH_MAX];
    uint64_t files, bytes, decrypted, decrypt_failed;
    char decrypt_skipped[48];
    char dlc[APP_DUMP_MAX_DLC][96];
    unsigned dlc_count;
    char fw[16];
    char cause[256]; /* a failure's token, with ": <detail>" for an I/O failure */
} app_dump_result_t;

enum {
    APP_DUMP_OK = 0,
    APP_DUMP_E_NOT_RUNNING,
    APP_DUMP_E_EXISTS,
    APP_DUMP_E_NO_SPACE,
    APP_DUMP_E_IO,
    APP_DUMP_E_CANCELLED,
};

/* How the run reports. Every member may be NULL. */
typedef struct {
    void *arg;
    int (*cancelled)(void *arg);
    void (*set_total)(void *arg, uint64_t files, uint64_t bytes);
    void (*add)(void *arg, uint64_t files, uint64_t bytes);
    void (*note)(void *arg, const char *what); /* the step it is on, for the job's message */
} app_dump_hooks_t;

/* What one SELF decryption came to. */
enum {
    APP_DUMP_SELF_OK = 0,
    APP_DUMP_SELF_NOT_SELF,    /* not a SELF (a plain ELF, data): left as copied */
    APP_DUMP_SELF_FAILED,      /* I/O, a malformed header, or a segment the console would not decrypt */
    APP_DUMP_SELF_TOO_BIG,     /* sizes beyond any real SELF: refused, not attempted */
    APP_DUMP_SELF_UNSUPPORTED, /* this firmware's pager table is not known */
    APP_DUMP_SELF_CANCELLED,
};

/* The console-only half, replaceable by tests. The default (app_dump_self.c) uses the self pager
 * on the console and is a stub elsewhere (unsupported, firmware unknown). */
typedef struct {
    uint32_t (*fw_version)(void); /* kernel_get_fw_version(), 0 when unknown */
    int (*decrypt_supported)(void); /* 1 when decryption can run on this firmware */
    /* Decrypts the SELF open as in_fd into out_fd (empty, writable), as an ELF. */
    int (*decrypt_self)(int in_fd, int out_fd, int (*cancelled)(void *), void *arg);
} app_dump_platform_t;
extern app_dump_platform_t app_dump_platform;

/* Prefixed to every console path (/mnt/sandbox, /user, /system_data): "" on the console, a temp
 * tree in the host tests. `dest` is never prefixed. */
extern const char *app_dump_root;

/* The args JSON. NULL, or the refusal token. */
const char *app_dump_parse_args(const char *json, size_t n, app_dump_args_t *a);
/* 1 for a title id of 4 capital letters and 5 digits (PPSA01234, CUSA00001). */
int app_dump_title_ok(const char *id);
/* 1 for a PS4 title (CUSA...). */
int app_dump_is_ps4(const char *id);
/* The dump's folder: <dest>/<ID>-app (PS5) or <dest>/<ID> (PS4). 0, or -1 when it does not fit. */
int app_dump_out_path(const app_dump_args_t *a, char *out, size_t cap);
/* 1 when `name` is one of `title_id`'s DLC mount folders (<region>-<ID>_00-<LABEL>-ac). */
int app_dump_dlc_name_ok(const char *name, const char *title_id);

/* Runs the dump. APP_DUMP_OK, or an APP_DUMP_E_* with r->cause set. */
int app_dump_run(const app_dump_args_t *a, const app_dump_hooks_t *h, app_dump_result_t *r);

/* The result as JSON (no title_id/finished) or, with info, the dump-info.json body. Returns the
 * length, or -1 when it does not fit. */
int app_dump_result_json(const app_dump_result_t *r, const char *title_id, long long finished, int info,
                         char *out, size_t cap);
/* <r->path>/dump-info.json. 0 or -1. */
int app_dump_write_info(const app_dump_args_t *a, const app_dump_result_t *r, long long finished);

/* ---- the ported pieces (app_dump_pkg.c, app_dump_fself.c), for the tests ---- */
/* sce_sys entries of a PS5 package (app.pkg / ac.pkg: its _sc.pkg CNT) into <out_dir>/sce_sys. */
int app_dump_unpkg_ps5(const char *pkg, const char *out_dir, const app_dump_hooks_t *h);
/* sce_sys entries of a PS4 package into <out_dir>/sce_sys. */
int app_dump_unpkg_ps4(const char *pkg, const char *out_dir, const app_dump_hooks_t *h);
/* 1 when `pkg` starts with the PS4 CNT magic. */
int app_dump_is_ps4_pkg(const char *pkg);
/* The files of a PFS image (pfs_image.dat). dry: only counts them into *files / *bytes. Copies
 * report through h->add. 0, -1 an error (errno), -2 cancelled. */
int app_dump_unpfs(const char *image, const char *out_dir, int dry, const app_dump_hooks_t *h, uint64_t *files,
                   uint64_t *bytes);
/* A fake-signed SELF of the ELF at elf_path. 0 or -1. */
int app_dump_elf2fself(const char *elf_path, const char *fself_path);
/* NPWR id (NPWRxxxxx_xx) from an npbind.dat. 0 or -1. */
int app_dump_read_npwr(const char *npbind, char *out, size_t cap);

/* ---- the job ---- */
struct ava1_op_ctx;
int app_dump_op(void *arg, struct ava1_op_ctx *c, const uint8_t *args, size_t n);
/* Registers JOB_OP_APP_DUMP with ava1_op.c (next to archive_extract_register). */
void app_dump_register(void);
/* 1 while a dump runs (the helper is busy: helper_busy.h). */
int app_dump_running(void);

/* From a fatal-signal handler: if a decryption had the kernel's vnode pager swapped for the self
 * pager, put it back (async-signal-safe; no-op otherwise). */
void app_dump_pager_emergency_restore(void);

#endif
