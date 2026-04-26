#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/param.h>
#include <sys/mount.h>
#include <sys/sysctl.h>
#include <sys/mdioctl.h>
#include <sys/ioctl.h>
#include <sys/uio.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <pthread.h>
#include "config.h"
#include "runtime.h"
#include "register.h"
#include "hw_info.h"
#include "proc_list.h"
#include "blake3.h"

/* PS5 SDK's `<fcntl.h>` hides `posix_fadvise` and its POSIX_FADV_* constants
 * behind `__POSIX_VISIBLE >= 200112`, but defining `_POSIX_C_SOURCE` to unlock
 * them also strips BSD-only types (`struct timeval`, `u_int`, etc.) that the
 * rest of this translation unit needs. Work around that by declaring the
 * prototype + constants ourselves — the underlying syscalls are wired up in
 * libc either way, so this is a visibility fix, not an ABI change. */
#ifndef POSIX_FADV_SEQUENTIAL
#define POSIX_FADV_NORMAL     0
#define POSIX_FADV_RANDOM     1
#define POSIX_FADV_SEQUENTIAL 2
#define POSIX_FADV_WILLNEED   3
#define POSIX_FADV_DONTNEED   4
#define POSIX_FADV_NOREUSE    5
extern int posix_fadvise(int fd, off_t offset, off_t len, int advice);
#endif

/* `posix_fallocate` is hidden behind the same __POSIX_VISIBLE gate as
 * posix_fadvise above. The libc symbol is present (FreeBSD 11+); we
 * just need a visible prototype. Used by cp_rf to reserve contiguous
 * dst extents up-front, which prevents block-allocation interleaving
 * with the data-write loop on multi-GiB copies. */
extern int posix_fallocate(int fd, off_t offset, off_t len);

#define FTX2_MAGIC 0x32585446u
#define FTX2_VERSION 1u
#define FTX2_FRAME_HELLO 1u
#define FTX2_FRAME_HELLO_ACK 2u
#define FTX2_FRAME_ERROR 3u
#define FTX2_FRAME_BEGIN_TX 10u
#define FTX2_FRAME_BEGIN_TX_ACK 11u
/* BeginTx TxMeta flag bits (see specs/ftx2-protocol.md). Bit 0 = RESUME:
 * host asks the payload to reuse an already-interrupted tx_id instead of
 * allocating a fresh slot. Unknown / non-resumable tx_ids fall through to
 * fresh-BeginTx semantics (flag becomes a no-op). */
#define FTX2_TX_FLAG_RESUME 0x1u
#define FTX2_FRAME_QUERY_TX 12u
#define FTX2_FRAME_QUERY_TX_ACK 13u
#define FTX2_FRAME_COMMIT_TX 14u
#define FTX2_FRAME_COMMIT_TX_ACK 15u
#define FTX2_FRAME_ABORT_TX 16u
#define FTX2_FRAME_ABORT_TX_ACK 17u
#define FTX2_FRAME_TAKEOVER_REQUEST 18u
#define FTX2_FRAME_TAKEOVER_ACK 19u
#define FTX2_FRAME_STATUS 20u
#define FTX2_FRAME_STATUS_ACK 21u
#define FTX2_FRAME_SHUTDOWN 22u
#define FTX2_FRAME_SHUTDOWN_ACK 23u
#define FTX2_FRAME_STREAM_SHARD 30u
#define FTX2_FRAME_SHARD_ACK 31u
#define FTX2_FRAME_CLEANUP 32u
#define FTX2_FRAME_CLEANUP_ACK 33u
#define FTX2_FRAME_FS_LIST_VOLUMES 34u
#define FTX2_FRAME_FS_LIST_VOLUMES_ACK 35u
#define FTX2_FRAME_FS_LIST_DIR 36u
#define FTX2_FRAME_FS_LIST_DIR_ACK 37u
#define FTX2_FRAME_FS_HASH 38u
#define FTX2_FRAME_FS_HASH_ACK 39u
#define FTX2_FRAME_FS_DELETE 40u
#define FTX2_FRAME_FS_DELETE_ACK 41u
#define FTX2_FRAME_FS_MOVE 42u
#define FTX2_FRAME_FS_MOVE_ACK 43u
#define FTX2_FRAME_FS_CHMOD 44u
#define FTX2_FRAME_FS_CHMOD_ACK 45u
#define FTX2_FRAME_FS_MKDIR 46u
#define FTX2_FRAME_FS_MKDIR_ACK 47u
#define FTX2_FRAME_FS_READ 48u
#define FTX2_FRAME_FS_READ_ACK 49u
#define FTX2_FRAME_FS_COPY 50u
#define FTX2_FRAME_FS_COPY_ACK 51u
#define FTX2_FRAME_FS_MOUNT 52u
#define FTX2_FRAME_FS_MOUNT_ACK 53u
#define FTX2_FRAME_FS_UNMOUNT 54u
#define FTX2_FRAME_FS_UNMOUNT_ACK 55u
/* App lifecycle frames — registration + launch. See register.h for
 * the pipeline. Body is JSON:
 *   APP_REGISTER:       {"src_path":"/abs/..."}
 *   APP_UNREGISTER:     {"title_id":"PPSA00xxx"}
 *   APP_LAUNCH:         {"title_id":"PPSA00xxx"}
 *   APP_LIST_REGISTERED: {} (empty)
 * ACK bodies:
 *   APP_REGISTER_ACK:    {"title_id":"...","title_name":"...","used_nullfs":true}
 *   APP_UNREGISTER_ACK:  {} (empty)
 *   APP_LAUNCH_ACK:      {} (empty)
 *   APP_LIST_REGISTERED_ACK: {"apps":[{"title_id":"...","title_name":"...",
 *                                      "src":"/...","image_backed":bool}, ...]}
 */
#define FTX2_FRAME_APP_REGISTER         56u
#define FTX2_FRAME_APP_REGISTER_ACK     57u
#define FTX2_FRAME_APP_UNREGISTER       58u
#define FTX2_FRAME_APP_UNREGISTER_ACK   59u
#define FTX2_FRAME_APP_LAUNCH           60u
#define FTX2_FRAME_APP_LAUNCH_ACK       61u
#define FTX2_FRAME_APP_LIST_REGISTERED     62u
#define FTX2_FRAME_APP_LIST_REGISTERED_ACK 63u
/* Hardware monitoring frames. Bodies are newline-separated "key=value"
 * text (not JSON) — keeps the payload parser trivial. */
#define FTX2_FRAME_HW_INFO          64u
#define FTX2_FRAME_HW_INFO_ACK      65u
#define FTX2_FRAME_HW_TEMPS         66u
#define FTX2_FRAME_HW_TEMPS_ACK     67u
#define FTX2_FRAME_HW_POWER         68u
#define FTX2_FRAME_HW_POWER_ACK     69u
/* Open the PS5 web browser (useful for self-hosted payload-loader
 * pages). Fire-and-forget: the ACK is just a success signal. */
#define FTX2_FRAME_APP_LAUNCH_BROWSER     70u
#define FTX2_FRAME_APP_LAUNCH_BROWSER_ACK 71u
/* Set fan turbo-threshold °C via /dev/icc_fan. Request body is
 * ASCII decimal digits (e.g. "65"); ACK body is empty. The payload
 * clamps to the safe range defined in hw_info.h regardless of input. */
#define FTX2_FRAME_HW_SET_FAN_THRESHOLD     72u
#define FTX2_FRAME_HW_SET_FAN_THRESHOLD_ACK 73u
/* Walk kernel allproc and return {"ok":bool,"procs":[{"pid":N,"name":"..."},...]}.
 * Body is JSON (unlike the hw_* text bodies above) because the UI wants
 * to render a table of entries, which is nicer with structured parsing
 * than a line-based format. */
#define FTX2_FRAME_PROC_LIST      74u
#define FTX2_FRAME_PROC_LIST_ACK  75u
/* Where we place mount points. Scoped under /mnt/ps5upload/ so it
 * never collides with mount paths owned by other utilities. */
#define FS_MOUNT_BASE "/mnt/ps5upload"
#define FS_MOUNT_MD_CTL  "/dev/mdctl"
#define FS_MOUNT_LVD_CTL "/dev/lvdctl"
/* Max wait for /dev/md<N> or /dev/lvd<N> to appear after attach.
 * Device nodes usually show up within a few hundred microseconds,
 * but allow up to 2 s so a slow sandbox doesn't falsely report
 * "attach failed". */
#define FS_MOUNT_DEV_WAIT_RETRIES 200
#define FS_MOUNT_DEV_WAIT_US      10000u  /* 10 ms × 200 = 2 s */

/* ── Sony LVD (Logical Volume Device) ioctl interface ────────────────────
 *
 * Reverse-engineered LVD constants + structs. The PS5 kernel prefers
 * LVD over plain FreeBSD md(4) for file-backed images; MDIOCATTACH
 * often fails on PS5 where LVD succeeds. We try LVD first and fall
 * back to MD if LVD returns an error.
 *
 * Struct layouts are PS5-kernel-specific — DO NOT reorder fields. */

#define FS_MOUNT_LVD_IOC_ATTACH_V0 0xC0286D00ull
#define FS_MOUNT_LVD_IOC_DETACH    0xC0286D01ull

/* LVD attach raw flags → normalized flags (precomputed) for the four
 * cases we support. We hardcode the outputs since we only ever mount
 * RW as user. */
#define FS_MOUNT_LVD_FLAGS_EXFAT_RW 0x14u  /* raw 0x8 (single-image RW) */
#define FS_MOUNT_LVD_FLAGS_UFS_RW   0x16u  /* raw 0xC (dd/lwfs RW)       */
#define FS_MOUNT_LVD_SECONDARY_SINGLE 0x10000u

typedef struct {
    uint16_t source_type;      /* +0x00: 1 = file, 2 = block/char device */
    uint16_t flags;            /* +0x02: bit0 = NO_BITMAP */
    uint32_t reserved0;        /* +0x04: must be zero */
    const char *path;          /* +0x08: backing file path */
    uint64_t offset;           /* +0x10: offset within backing object */
    uint64_t size;             /* +0x18: exposed size in bytes */
    const char *bitmap_path;   /* +0x20: unused when NO_BITMAP set */
    uint64_t bitmap_offset;    /* +0x28: unused */
    uint64_t bitmap_size;      /* +0x30: unused */
} fs_mount_lvd_layer_t;

typedef struct {
    uint32_t io_version;       /* +0x00: 0 = V0/base */
    int32_t  device_id;        /* +0x04: in=-1 for auto, out=unit assigned */
    uint32_t sector_size;      /* +0x08: exposed sector size (512 or 4096) */
    uint32_t secondary_unit;   /* +0x0C: LVD_SECONDARY_SINGLE for exfat */
    uint16_t flags;            /* +0x10: normalized attach flags */
    uint16_t image_type;       /* +0x12: 0=single, 7=ufs_dd, 5=pfs_save */
    uint32_t layer_count;      /* +0x14: 1 for single-layer images */
    uint64_t device_size;      /* +0x18: total exposed virtual size */
    fs_mount_lvd_layer_t *layers_ptr; /* +0x20: array of layer descriptors */
} fs_mount_lvd_attach_t;

typedef struct {
    uint32_t reserved0;        /* +0x00: must be zero */
    int32_t  device_id;        /* +0x04: unit to detach */
    uint8_t  reserved[0x20];   /* +0x08: kernel ABI padding */
} fs_mount_lvd_detach_t;
/* Hard cap on FS_READ response size. 2 MiB is comfortably above the
 * largest icon0.png we've observed (~700 KiB) while keeping a single
 * ACK well under the payload's malloc ceiling and the send buffer
 * size. Clients asking for more get truncated silently — metadata
 * fetches don't need recovery semantics. */
#define FS_READ_MAX_BYTES (2u * 1024u * 1024u)
#define FTX2_HEADER_LEN 28u
#define FTX2_TX_META_LEN 24u
#define FTX2_SHARD_HEADER_LEN 64u
#define FTX2_SHARD_ACK_LEN 48u
#define FTX2_ACK_STATE_SPOOLED 0u
#define FTX2_ACK_STATE_APPLIED 1u
#define FTX2_ACK_STATE_DUPLICATE_IGNORED 2u
#define FTX2_SHARD_DRAIN_BUF 65536u

/* ShardHeader.flags bit definitions. */
#define FTX2_SHARD_FLAG_PACKED 0x1u
#define FTX2_PACKED_RECORD_PREFIX_LEN 8u   /* u32 path_len + u32 data_len */

typedef struct {
    uint32_t magic;
    uint16_t version;
    uint16_t frame_type;
    uint32_t flags;
    uint64_t body_len;
    uint64_t trace_id;
} ftx2_header_t;

typedef struct {
    unsigned char tx_id[16];
    uint32_t kind;
    uint32_t flags;
} ftx2_tx_meta_t;

/* Shard header prefix (64 bytes, LE) — matches ftx2-proto::ShardHeader */
typedef struct {
    unsigned char tx_id[16];        /* offset  0 */
    uint64_t shard_seq;             /* offset 16 */
    unsigned char shard_digest[32]; /* offset 24 — BLAKE3-256 */
    uint32_t record_count;          /* offset 56 */
    uint32_t flags;                 /* offset 60 */
} ftx2_shard_header_t;             /* 64 bytes */

/* SHARD_ACK body (48 bytes, LE) — matches ftx2-proto::ShardAck */
typedef struct {
    unsigned char tx_id[16];    /* offset  0 */
    uint64_t shard_seq;         /* offset 16 */
    uint8_t ack_state;          /* offset 24 */
    uint8_t pad[7];             /* offset 25 */
    uint64_t bytes_committed;   /* offset 32 */
    uint64_t files_committed;   /* offset 40 */
} ftx2_shard_ack_t;            /* 48 bytes */

/* Forward type declaration — full definition appears later in the file. */
typedef struct manifest_file_entry manifest_file_entry_t;
typedef struct manifest_index_entry manifest_index_entry_t;

/* Forward declarations */
static uint32_t read_le32(const unsigned char *p);
/* Forward declarations — runtime_reconcile_mounts uses fs_mount and
 * mount_tracker helpers defined further down in the file. */
static int fs_mount_try_unmount(const char *mount_point);
static int fs_mount_detach_md(int unit_id);
static int fs_mount_detach_lvd(int unit_id);
static int  mount_tracker_read(const char *mount_name, char *out, size_t out_cap);
static void mount_tracker_write(const char *mount_name, const char *src_path);
static void mount_tracker_remove(const char *mount_name);
static int handle_stream_shard(runtime_state_t *state, int client_fd,
                                uint64_t trace_id, uint64_t body_len);
static int runtime_write_manifest(const runtime_tx_entry_t *entry,
                                   const char *manifest_json, size_t manifest_len);
static int runtime_read_manifest(const runtime_tx_entry_t *entry,
                                  char *buf, size_t buf_len);
static int manifest_get_nth_file_path(const char *json, uint64_t n,
                                       manifest_file_entry_t *out);
static void runtime_release_tx_resources(runtime_tx_entry_t *entry);
/* Variant that preserves the `.ps5up2-tmp` file(s) and manifest heap so a
 * resumed BEGIN_TX can pick up where the interrupted one left off. Used
 * whenever the tx is transitioning to "interrupted" (TCP drop, takeover,
 * shutdown) — NOT for terminal transitions (commit/abort), which use
 * the full-cleanup variant above. */
static void runtime_release_tx_resources_ephemeral(runtime_tx_entry_t *entry);

/* Per-connection tracking for the transfer port: records the tx_id of
 * the currently-open transaction on this connection so the accept loop
 * can mark it "interrupted" when the socket closes without COMMIT/ABORT.
 * Mgmt-port handlers pass NULL — they never open a tx. */
typedef struct {
    unsigned char tx_id[16];
    int           has_tx;  /* 1 between BEGIN_TX_ACK and COMMIT/ABORT_ACK */
} conn_tx_ctx_t;

/* Mark a single tx entry "interrupted" iff it's currently "active". Preserves
 * tmp file(s) + manifest heap so a subsequent BEGIN_TX with TX_FLAG_RESUME
 * can adopt the entry and pick up from the last-acked shard. Safe to call
 * with a tx_id that no longer exists (e.g. the tx committed between the
 * frame returning and the connection closing) — becomes a no-op. */
static void runtime_mark_tx_interrupted_by_id(runtime_state_t *state,
                                                const unsigned char *tx_id);

/* ── Directory helper ───────────────────────────────────────────────────────── */

static int ensure_dir(const char *path) {
    if (!path || !*path) return -1;
    if (mkdir(path, 0777) == 0) return 0;
    if (errno == EEXIST) return 0;
    return -1;
}

/* ── TX summary state (active count + last seq) ─────────────────────────────── */

static int runtime_load_tx_state(runtime_state_t *state) {
    FILE *fp = NULL;
    unsigned long long active = 0;
    unsigned long long last_seq = 0;
    if (!state) return -1;
    fp = fopen(state->tx_state_path, "r");
    if (!fp) {
        if (errno == ENOENT) return 0;
        return -1;
    }
    if (fscanf(fp, "active_transactions=%llu\nlast_tx_seq=%llu\n", &active, &last_seq) == 2) {
        state->active_transactions = (uint64_t)active;
        state->last_tx_seq = (uint64_t)last_seq;
    }
    fclose(fp);
    return 0;
}

static int runtime_save_tx_state(const runtime_state_t *state) {
    FILE *fp = NULL;
    if (!state) return -1;
    fp = fopen(state->tx_state_path, "w");
    if (!fp) return -1;
    fprintf(fp,
            "active_transactions=%llu\nlast_tx_seq=%llu\n",
            (unsigned long long)state->active_transactions,
            (unsigned long long)state->last_tx_seq);
    fclose(fp);
    return 0;
}

/* ── TX event log ────────────────────────────────────────────────────────────── */

static int runtime_append_tx_event(const runtime_state_t *state, const char *event_name) {
    FILE *fp = NULL;
    if (!state || !event_name) return -1;
    fp = fopen(state->tx_journal_path, "a");
    if (!fp) return -1;
    fprintf(fp,
            "ts=%llu event=%s active_transactions=%llu last_tx_seq=%llu\n",
            (unsigned long long)time(NULL),
            event_name,
            (unsigned long long)state->active_transactions,
            (unsigned long long)state->last_tx_seq);
    fclose(fp);
    return 0;
}

/* ── Per-TX shard log ────────────────────────────────────────────────────────── */

/*
 * Per-shard log. Kept as an open FILE* on the tx entry so we pay one fopen()
 * per transaction, not one per shard (which cost ~1 ms × N shards on PS5).
 * Casting away const: the cache is a pure-lazy-init optimisation of an
 * otherwise stateless log append, not an observable state change.
 */
static void runtime_append_shard_log(const runtime_tx_entry_t *entry,
                                      uint64_t shard_seq, uint64_t data_len) {
    runtime_tx_entry_t *mut;
    FILE *fp;
    if (!entry) return;
    mut = (runtime_tx_entry_t *)entry;
    fp = (FILE *)mut->shard_log_fp;
    if (!fp) {
        char path[512];
        snprintf(path, sizeof(path), "%s/shards_%s.log",
                 PS5UPLOAD2_TX_DIR, entry->tx_id_hex);
        fp = fopen(path, "a");
        if (!fp) return;
        mut->shard_log_fp = (void *)fp;
    }
    fprintf(fp, "shard_seq=%llu bytes=%llu ts=%llu\n",
            (unsigned long long)shard_seq,
            (unsigned long long)data_len,
            (unsigned long long)time(NULL));
    /* No fflush here — one fflush on terminal transition (release_tx_resources)
     * is enough. Losing the tail of the log on crash is fine; the log is for
     * post-hoc diagnostics, not recovery. */
}

/* ── JSON helpers ─────────────────────────────────────────────────────────────── */

static void extract_json_string_field(const char *json, const char *field,
                                       char *out, size_t out_len) {
    char needle[64];
    const char *pos = NULL;
    const char *start = NULL;
    size_t len = 0;
    if (!json || !field || !out || out_len == 0) return;
    out[0] = '\0';
    snprintf(needle, sizeof(needle), "\"%s\":\"", field);
    pos = strstr(json, needle);
    if (!pos) return;
    start = pos + strlen(needle);
    while (start[len] && start[len] != '"') len += 1;
    if (len == 0 || len + 1 > out_len) return;
    memcpy(out, start, len);
    out[len] = '\0';
}

static uint64_t extract_json_uint64_field(const char *json, const char *field) {
    char needle[64];
    const char *pos = NULL;
    if (!json || !field) return 0;
    snprintf(needle, sizeof(needle), "\"%s\":", field);
    pos = strstr(json, needle);
    if (!pos) return 0;
    pos += strlen(needle);
    return (uint64_t)strtoull(pos, NULL, 10);
}

/* ── TX ID helpers ───────────────────────────────────────────────────────────── */

static void tx_id_bytes_to_hex(const unsigned char *tx_id, char *out, size_t out_len) {
    static const char hex[] = "0123456789abcdef";
    size_t i = 0;
    if (!tx_id || !out || out_len < 33) return;
    for (i = 0; i < 16; i++) {
        out[i * 2]     = hex[(tx_id[i] >> 4) & 0x0f];
        out[i * 2 + 1] = hex[tx_id[i] & 0x0f];
    }
    out[32] = '\0';
}

static int tx_id_bytes_equal(const unsigned char *a, const unsigned char *b) {
    return memcmp(a, b, 16) == 0;
}

static int hex_char_value(char ch) {
    if (ch >= '0' && ch <= '9') return ch - '0';
    if (ch >= 'a' && ch <= 'f') return 10 + (ch - 'a');
    if (ch >= 'A' && ch <= 'F') return 10 + (ch - 'A');
    return -1;
}

static int tx_id_hex_to_bytes(const char *hex, unsigned char *out) {
    int i = 0;
    if (!hex || !out) return -1;
    for (i = 0; i < 16; i++) {
        int hi = hex_char_value(hex[i * 2]);
        int lo = hex_char_value(hex[i * 2 + 1]);
        if (hi < 0 || lo < 0) return -1;
        out[i] = (unsigned char)((hi << 4) | lo);
    }
    return 0;
}

/* ── TX table helpers ─────────────────────────────────────────────────────────── */

static runtime_tx_entry_t *runtime_find_tx_entry(runtime_state_t *state,
                                                   const unsigned char *tx_id) {
    int i = 0;
    if (!state || !tx_id) return NULL;
    for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
        if (state->tx_entries[i].in_use &&
            tx_id_bytes_equal(state->tx_entries[i].tx_id, tx_id)) {
            return &state->tx_entries[i];
        }
    }
    return NULL;
}

/* True if `entry` is in a terminal state and its slot can be recycled. */
static int runtime_tx_state_is_terminal(const runtime_tx_entry_t *entry) {
    if (!entry || !entry->in_use) return 1;
    return strcmp(entry->state, "committed")   == 0 ||
           strcmp(entry->state, "aborted")     == 0 ||
           strcmp(entry->state, "interrupted") == 0;
}

static runtime_tx_entry_t *runtime_alloc_tx_entry(runtime_state_t *state,
                                                    const unsigned char *tx_id,
                                                    uint64_t tx_seq) {
    int i = 0;
    int evict_idx = -1;
    uint64_t oldest_seq = UINT64_MAX;
    runtime_tx_entry_t *entry = NULL;
    if (!state || !tx_id) return NULL;
    entry = runtime_find_tx_entry(state, tx_id);
    if (entry) return entry;
    /* First pass: any free slot wins. */
    for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
        if (!state->tx_entries[i].in_use) {
            evict_idx = i;
            break;
        }
    }
    /* Second pass: evict the oldest terminal tx (committed/aborted/interrupted)
     * so the table never wedges after many completed transfers. */
    if (evict_idx < 0) {
        for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
            if (runtime_tx_state_is_terminal(&state->tx_entries[i]) &&
                state->tx_entries[i].tx_seq < oldest_seq) {
                oldest_seq = state->tx_entries[i].tx_seq;
                evict_idx = i;
            }
        }
    }
    if (evict_idx < 0) return NULL;
    entry = &state->tx_entries[evict_idx];
    /* If we're reusing a terminal slot, its heap state (manifest blob + index)
     * must be freed before we memset — otherwise we leak both allocations. */
    runtime_release_tx_resources(entry);
    memset(entry, 0, sizeof(*entry));
    entry->in_use = 1;
    memcpy(entry->tx_id, tx_id, 16);
    tx_id_bytes_to_hex(tx_id, entry->tx_id_hex, sizeof(entry->tx_id_hex));
    entry->tx_seq = tx_seq;
    snprintf(entry->state, sizeof(entry->state), "active");
    return entry;
}

/* ── TX record persistence ────────────────────────────────────────────────────── */

/*
 * Write (or overwrite) the JSON record for a transaction from in-memory state.
 * Called on BEGIN_TX, COMMIT_TX, ABORT_TX, and when marking interrupted.
 */
static int runtime_flush_tx_record(const runtime_state_t *state,
                                    const runtime_tx_entry_t *entry) {
    char path[512];
    FILE *fp = NULL;
    if (!state || !entry) return -1;
    snprintf(path, sizeof(path), "%s/tx_%s.json", PS5UPLOAD2_TX_DIR, entry->tx_id_hex);
    fp = fopen(path, "w");
    if (!fp) return -1;
    fprintf(fp,
            "{\"tx_id\":\"%s\",\"tx_seq\":%llu,\"state\":\"%s\","
            "\"shards_received\":%llu,\"bytes_received\":%llu,"
            "\"total_shards\":%llu,\"total_bytes\":%llu,"
            "\"file_count\":%llu,\"dest_root\":\"%s\"}\n",
            entry->tx_id_hex,
            (unsigned long long)entry->tx_seq,
            entry->state,
            (unsigned long long)entry->shards_received,
            (unsigned long long)entry->bytes_received,
            (unsigned long long)entry->total_shards,
            (unsigned long long)entry->total_bytes,
            (unsigned long long)entry->file_count,
            entry->dest_root);
    fclose(fp);
    return 0;
}

/* Read the persisted JSON record for a transaction entry into buf. */
static int runtime_read_tx_record(const runtime_tx_entry_t *entry,
                                   char *buf, size_t buf_len) {
    char path[512];
    FILE *fp = NULL;
    size_t got = 0;
    if (!entry || !buf || buf_len == 0) return -1;
    snprintf(path, sizeof(path), "%s/tx_%s.json", PS5UPLOAD2_TX_DIR, entry->tx_id_hex);
    fp = fopen(path, "r");
    if (!fp) return -1;
    got = fread(buf, 1, buf_len - 1, fp);
    buf[got] = '\0';
    fclose(fp);
    return 0;
}

/*
 * Update in-memory state string and flush the record.
 * Used by ABORT_TX, COMMIT_TX, and the interrupted-on-takeover path.
 */
static int runtime_set_tx_state(runtime_state_t *state,
                                  const unsigned char *tx_id,
                                  const char *new_state) {
    runtime_tx_entry_t *entry = runtime_find_tx_entry(state, tx_id);
    if (!entry) return -1;
    snprintf(entry->state, sizeof(entry->state), "%s", new_state);
    return runtime_flush_tx_record(state, entry);
}

/* ── TX table bulk operations ────────────────────────────────────────────────── */

static int runtime_load_tx_entries(runtime_state_t *state) {
    DIR *dir = NULL;
    struct dirent *de = NULL;
    if (!state) return -1;
    dir = opendir(PS5UPLOAD2_TX_DIR);
    if (!dir) {
        if (errno == ENOENT) return 0;
        return -1;
    }
    while ((de = readdir(dir)) != NULL) {
        char tx_id_hex[33];
        unsigned char tx_id[16];
        char path[512];
        char record[512];
        unsigned long long tx_seq = 0;
        runtime_tx_entry_t *entry = NULL;
        const char *name = de->d_name;
        if (strncmp(name, "tx_", 3) != 0) continue;
        if (strlen(name) < 3 + 32 + 5) continue;
        memcpy(tx_id_hex, name + 3, 32);
        tx_id_hex[32] = '\0';
        if (strcmp(name + 35, ".json") != 0) continue;
        if (tx_id_hex_to_bytes(tx_id_hex, tx_id) != 0) continue;
        snprintf(path, sizeof(path), "%s/%s", PS5UPLOAD2_TX_DIR, name);
        {
            FILE *fp = fopen(path, "r");
            size_t got = 0;
            if (!fp) continue;
            got = fread(record, 1, sizeof(record) - 1, fp);
            record[got] = '\0';
            fclose(fp);
        }
        tx_seq = extract_json_uint64_field(record, "tx_seq");
        if (tx_seq == 0) continue;
        entry = runtime_alloc_tx_entry(state, tx_id, tx_seq);
        if (!entry) continue;
        extract_json_string_field(record, "state", entry->state, sizeof(entry->state));
        if (entry->state[0] == '\0') {
            snprintf(entry->state, sizeof(entry->state), "active");
        }
        entry->shards_received = extract_json_uint64_field(record, "shards_received");
        entry->bytes_received  = extract_json_uint64_field(record, "bytes_received");
        entry->total_shards    = extract_json_uint64_field(record, "total_shards");
        entry->total_bytes     = extract_json_uint64_field(record, "total_bytes");
        entry->file_count      = extract_json_uint64_field(record, "file_count");
        extract_json_string_field(record, "dest_root", entry->dest_root, sizeof(entry->dest_root));
    }
    closedir(dir);
    return 0;
}

static void runtime_reconcile_tx_state(runtime_state_t *state) {
    int i = 0;
    uint64_t active = 0;
    uint64_t last_seq = 0;
    if (!state) return;
    for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
        if (!state->tx_entries[i].in_use) continue;
        if (strcmp(state->tx_entries[i].state, "active") == 0) active += 1;
        if (state->tx_entries[i].tx_seq > last_seq) last_seq = state->tx_entries[i].tx_seq;
    }
    state->active_transactions = active;
    if (last_seq > state->last_tx_seq) state->last_tx_seq = last_seq;
}

static void runtime_mark_active_transactions(runtime_state_t *state, const char *new_state) {
    int i = 0;
    int is_interrupt = 0;
    if (!state || !new_state) return;
    is_interrupt = (strcmp(new_state, "interrupted") == 0);
    for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
        if (!state->tx_entries[i].in_use) continue;
        if (strcmp(state->tx_entries[i].state, "active") != 0) continue;
        (void)runtime_set_tx_state(state, state->tx_entries[i].tx_id, new_state);
        /* "interrupted" is a pauseable state — the client may reconnect with
         * TX_FLAG_RESUME and pick up from last_acked_shard. Preserve the
         * tmp file(s) and manifest so that works. For any other transition
         * (currently unused, but keep the behavior defensive), fall through
         * to the full release which unlinks the tmp. */
        if (is_interrupt) {
            runtime_release_tx_resources_ephemeral(&state->tx_entries[i]);
        } else {
            runtime_release_tx_resources(&state->tx_entries[i]);
        }
    }
    runtime_reconcile_tx_state(state);
    (void)runtime_save_tx_state(state);
}

static void runtime_mark_tx_interrupted_by_id(runtime_state_t *state,
                                                const unsigned char *tx_id) {
    /* Snapshot of the entry we need to flush after unlocking. We don't
     * keep a pointer to the slot across the unlock because
     * `runtime_alloc_tx_entry` can evict + memset terminal slots at
     * any time from another thread — holding a raw pointer would
     * TOCTOU into reading a zeroed entry and writing a garbled
     * tx_<hex>.json record. Copy-by-value while we hold the lock. */
    runtime_tx_entry_t snapshot;
    int should_flush = 0;
    if (!state || !tx_id) return;
    pthread_mutex_lock(&state->state_mtx);
    {
        runtime_tx_entry_t *entry = runtime_find_tx_entry(state, tx_id);
        if (entry && strcmp(entry->state, "active") == 0) {
            snprintf(entry->state, sizeof(entry->state), "interrupted");
            runtime_release_tx_resources_ephemeral(entry);
            /* `active_transactions` was bumped at BEGIN_TX and not
             * decremented by commit/abort (which never ran here).
             * Decrement now so the counter reflects reality; the entry
             * survives for resume lookup. */
            if (state->active_transactions > 0) state->active_transactions -= 1;
            snapshot = *entry;  /* copy fields used by the flush below */
            should_flush = 1;
        }
    }
    pthread_mutex_unlock(&state->state_mtx);
    if (should_flush) {
        (void)runtime_flush_tx_record(state, &snapshot);
        (void)runtime_save_tx_state(state);
        (void)runtime_append_tx_event(state, "conn_drop_interrupt");
    }
}

/* ── Public lifecycle ─────────────────────────────────────────────────────────── */

int runtime_ensure_directories(void) {
    if (ensure_dir(PS5UPLOAD2_RUNTIME_ROOT) != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_RUNTIME_DIR)  != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_TX_DIR)       != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_SPOOL_DIR)    != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_DEBUG_DIR)    != 0) return -1;
    if (ensure_dir(PS5UPLOAD2_MOUNTS_DIR)   != 0) return -1;
    return 0;
}

/* Startup reconciliation for `/mnt/ps5upload/` mounts.
 *
 * Walks the kernel mount table. For each of our mounts, validates:
 *   1. The backing dev node (f_mntfromname) still exists.
 *   2. The recorded source image (from the .src tracker) still exists.
 *
 * If either check fails the mount is orphaned — force-unmount +
 * detach + cleanup + remove tracker. Result: every payload startup
 * leaves the Volumes screen showing only valid, usable mounts.
 *
 * Why we need this:
 *   - Mounts survive payload restarts (kernel holds them), so without
 *     reconciliation a "Volumes" screen after a payload re-send shows
 *     old mounts the user didn't do in the current session — confusing.
 *   - If the user deletes the backing .exfat file while it's mounted,
 *     the mount silently breaks. Reconciliation at next payload start
 *     cleans up the dead mount instead of leaving it to surface
 *     misleading errors.
 *
 * Intentionally tolerant: any single failure logs + continues. A
 * reconciliation error never prevents the payload from coming up.
 * At worst, one stale mount stays visible for one more session. */
void runtime_reconcile_mounts(void) {
    struct statfs *mnts = NULL;
    int nmnts = getmntinfo(&mnts, MNT_NOWAIT);
    if (nmnts <= 0 || mnts == NULL) return;

    int cleaned = 0;
    int kept    = 0;
    for (int i = 0; i < nmnts; i++) {
        const char *mnt_on   = mnts[i].f_mntonname;
        const char *mnt_from = mnts[i].f_mntfromname;
        if (strncmp(mnt_on, "/mnt/ps5upload/", 15) != 0) continue;
        if (mnt_on[15] == '\0') continue;  /* skip the parent dir */

        const char *name = mnt_on + 15;
        int orphaned = 0;
        const char *reason = "unknown";

        /* Check the dev node. If it's a /dev/md* or /dev/lvd* that
         * no longer stats, the MDIOCATTACH/LVD entry is gone and
         * the mount can't do anything useful. */
        struct stat dev_st;
        if (stat(mnt_from, &dev_st) != 0) {
            orphaned = 1;
            reason = "dev_node_gone";
        }

        /* Check the source image file. If it was deleted/moved since
         * the mount was created, keep the mount — users may have
         * intentionally moved the file and we don't own cleanup of
         * that. Log only; don't clean up. */
        char src[256];
        int have_src = mount_tracker_read(name, src, sizeof(src));
        if (have_src) {
            struct stat src_st;
            if (stat(src, &src_st) != 0) {
                /* Source file gone — flag for info, but DON'T unmount.
                 * Filesystem on /dev/lvd* is self-contained; the
                 * source file being missing is a diagnostic, not a
                 * correctness problem. Leaving this mount alive lets
                 * the user finish whatever they were doing. */
                fprintf(stderr,
                    "[payload2] mount %s: source %s missing (keeping mount)\n",
                    mnt_on, src);
            }
        }

        if (orphaned) {
            fprintf(stderr,
                "[payload2] reconcile: unmounting orphan %s (%s)\n",
                mnt_on, reason);
            /* Extract the unit number so we can release the attachment. */
            int lvd_unit = -1, md_unit = -1;
            if (strncmp(mnt_from, "/dev/lvd", 8) == 0 &&
                mnt_from[8] >= '0' && mnt_from[8] <= '9') {
                lvd_unit = atoi(mnt_from + 8);
            } else if (strncmp(mnt_from, "/dev/md", 7) == 0 &&
                       mnt_from[7] >= '0' && mnt_from[7] <= '9') {
                md_unit = atoi(mnt_from + 7);
            }
            (void)fs_mount_try_unmount(mnt_on);
            if (lvd_unit >= 0) (void)fs_mount_detach_lvd(lvd_unit);
            if (md_unit  >= 0) (void)fs_mount_detach_md(md_unit);
            (void)rmdir(mnt_on);
            mount_tracker_remove(name);
            cleaned += 1;
        } else {
            kept += 1;
        }
    }
    if (cleaned > 0 || kept > 0) {
        fprintf(stderr,
            "[payload2] reconcile: kept %d mount(s), cleaned %d orphan(s)\n",
            kept, cleaned);
    }
}

/* Read the .src tracker written at mount time. Returns 1 on success
 * with out filled; 0 if no tracker exists (unknown source — likely a
 * mount from before this tracking was added, or a hand-crafted one).
 * Silent failure: the Volumes screen tolerates a missing source. */
static int mount_tracker_read(const char *mount_name, char *out, size_t out_cap) {
    char tracker[512];
    int n = snprintf(tracker, sizeof(tracker), "%s/%s.src",
                     PS5UPLOAD2_MOUNTS_DIR, mount_name);
    if (n < 0 || (size_t)n >= sizeof(tracker)) return 0;
    int fd = open(tracker, O_RDONLY);
    if (fd < 0) return 0;
    ssize_t got = read(fd, out, out_cap - 1);
    close(fd);
    if (got <= 0) { out[0] = '\0'; return 0; }
    out[got] = '\0';
    /* Strip trailing newline if present (hand-edited files might have one). */
    if (out[got - 1] == '\n') out[got - 1] = '\0';
    return 1;
}

/* Write the source-path tracker for a mount. Failure is non-fatal —
 * the mount itself has already succeeded; losing the tracker just
 * means the Volumes screen won't know which file backs the mount. */
static void mount_tracker_write(const char *mount_name, const char *src_path) {
    char tracker[512];
    int n = snprintf(tracker, sizeof(tracker), "%s/%s.src",
                     PS5UPLOAD2_MOUNTS_DIR, mount_name);
    if (n < 0 || (size_t)n >= sizeof(tracker)) return;
    int fd = open(tracker, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return;
    size_t len = strlen(src_path);
    ssize_t w = write(fd, src_path, len);
    (void)w;
    close(fd);
}

/* Remove the tracker when a mount goes away. Tolerant of already-gone
 * trackers because reconciliation may have cleaned up orphans first. */
static void mount_tracker_remove(const char *mount_name) {
    char tracker[512];
    int n = snprintf(tracker, sizeof(tracker), "%s/%s.src",
                     PS5UPLOAD2_MOUNTS_DIR, mount_name);
    if (n < 0 || (size_t)n >= sizeof(tracker)) return;
    (void)unlink(tracker);
}

int runtime_init(runtime_state_t *state) {
    if (!state) return -1;
    memset(state, 0, sizeof(*state));
    state->instance_id      = (uint64_t)time(NULL);
    state->started_at_unix  = (uint64_t)time(NULL);
    state->runtime_port     = PS5UPLOAD2_RUNTIME_PORT;
    state->mgmt_port        = PS5UPLOAD2_MGMT_PORT;
    state->listener_fd      = -1;
    state->mgmt_listener_fd = -1;
    state->startup_reason   = PS5UPLOAD2_STARTUP_FRESH;
    /* Mutex guards cross-thread reads/writes of tx counters + tx_entries.
     * Single-threaded in the transfer path; only STATUS + QUERY_TX from
     * the mgmt thread need synchronization. */
    if (pthread_mutex_init(&state->state_mtx, NULL) != 0) {
        fprintf(stderr, "[payload2] pthread_mutex_init failed\n");
        return -1;
    }
    snprintf(state->ownership_path, sizeof(state->ownership_path),
             "%s/active_instance.txt", PS5UPLOAD2_RUNTIME_DIR);
    snprintf(state->tx_state_path, sizeof(state->tx_state_path),
             "%s/runtime_tx_state.txt", PS5UPLOAD2_TX_DIR);
    snprintf(state->tx_journal_path, sizeof(state->tx_journal_path),
             "%s/events.log", PS5UPLOAD2_TX_DIR);
    if (runtime_load_tx_state(state) != 0) {
        fprintf(stderr, "[payload2] failed to load tx state from %s\n", state->tx_state_path);
        return -1;
    }
    if (runtime_load_tx_entries(state) != 0) {
        fprintf(stderr, "[payload2] failed to load tx entries from %s\n", PS5UPLOAD2_TX_DIR);
        return -1;
    }
    runtime_reconcile_tx_state(state);
    state->recovered_transactions = state->active_transactions;
    if (state->recovered_transactions > 0) {
        (void)runtime_append_tx_event(state, "startup_replay");
    }
    return 0;
}

int runtime_write_ownership(const runtime_state_t *state) {
    FILE *fp = NULL;
    if (!state) return -1;
    fp = fopen(state->ownership_path, "w");
    if (!fp) {
        fprintf(stderr, "[payload2] failed to open ownership record %s\n",
                state->ownership_path);
        return -1;
    }
    fprintf(fp,
            "instance_id=%llu\nruntime_port=%d\nstartup_reason=%d\nstarted_at_unix=%llu\n",
            (unsigned long long)state->instance_id,
            state->runtime_port,
            state->startup_reason,
            (unsigned long long)state->started_at_unix);
    fclose(fp);
    printf("[payload2] ownership record instance=%llu port=%d path=%s\n",
           (unsigned long long)state->instance_id,
           state->runtime_port,
           state->ownership_path);
    return 0;
}

int runtime_clear_ownership(const runtime_state_t *state) {
    if (!state) return -1;
    if (unlink(state->ownership_path) == 0 || errno == ENOENT) return 0;
    fprintf(stderr, "[payload2] failed to remove ownership record %s\n",
            state->ownership_path);
    return -1;
}

/* ── Wire I/O ─────────────────────────────────────────────────────────────────── */

static int recv_exact(int fd, void *buf, size_t len) {
    unsigned char *p = (unsigned char *)buf;
    size_t got = 0;
    while (got < len) {
        ssize_t n = recv(fd, p + got, len - got, 0);
        if (n < 0) {
            if (errno == EINTR) continue;
            /* Log the actual cause so a stalled transfer produces a
             * diagnostic in the engine/renderer log, not silence.
             * EAGAIN / EWOULDBLOCK here means SO_RCVTIMEO fired — the
             * client hasn't sent data within the idle window. Keep it
             * distinguishable from other recv errors. */
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                fprintf(stderr,
                        "[payload] recv_exact(fd=%d): timed out after %zu/%zu bytes "
                        "(SO_RCVTIMEO expired, client stalled)\n",
                        fd, got, len);
            } else {
                fprintf(stderr,
                        "[payload] recv_exact(fd=%d): got %zu/%zu bytes, recv failed: %s (errno=%d)\n",
                        fd, got, len, strerror(errno), errno);
            }
            return -1;
        }
        if (n == 0) {
            /* Peer closed the connection mid-frame. Usually means the
             * client crashed or the user cancelled — not a bug on our
             * side, but logging the byte-count we reached helps
             * distinguish "nothing arrived" from "truncated after K
             * bytes". */
            fprintf(stderr,
                    "[payload] recv_exact(fd=%d): peer closed after %zu/%zu bytes\n",
                    fd, got, len);
            return -1;
        }
        got += (size_t)n;
    }
    return 0;
}

/* Monotonic microseconds for per-phase timing. */
static uint64_t now_us(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) return 0;
    return (uint64_t)ts.tv_sec * 1000000ull + (uint64_t)(ts.tv_nsec / 1000);
}

static uint16_t read_le16(const unsigned char *p) {
    return (uint16_t)p[0] | ((uint16_t)p[1] << 8);
}

static uint32_t read_le32(const unsigned char *p) {
    return (uint32_t)p[0]
         | ((uint32_t)p[1] << 8)
         | ((uint32_t)p[2] << 16)
         | ((uint32_t)p[3] << 24);
}

static uint64_t read_le64(const unsigned char *p) {
    uint64_t v = 0;
    int i = 0;
    for (i = 0; i < 8; i++) v |= ((uint64_t)p[i]) << (8 * i);
    return v;
}

static void write_le16(unsigned char *p, uint16_t v) {
    p[0] = (unsigned char)(v & 0xff);
    p[1] = (unsigned char)((v >> 8) & 0xff);
}

static void write_le32(unsigned char *p, uint32_t v) {
    p[0] = (unsigned char)(v & 0xff);
    p[1] = (unsigned char)((v >> 8) & 0xff);
    p[2] = (unsigned char)((v >> 16) & 0xff);
    p[3] = (unsigned char)((v >> 24) & 0xff);
}

static void write_le64(unsigned char *p, uint64_t v) {
    int i = 0;
    for (i = 0; i < 8; i++) p[i] = (unsigned char)((v >> (8 * i)) & 0xff);
}

static int send_frame(int fd, uint16_t frame_type, uint32_t flags,
                      uint64_t trace_id, const void *body, uint64_t body_len) {
    unsigned char hdr[FTX2_HEADER_LEN];
    write_le32(hdr + 0, FTX2_MAGIC);
    write_le16(hdr + 4, FTX2_VERSION);
    write_le16(hdr + 6, frame_type);
    write_le32(hdr + 8, flags);
    write_le64(hdr + 12, body_len);
    write_le64(hdr + 20, trace_id);
    if (send(fd, hdr, sizeof(hdr), 0) != (ssize_t)sizeof(hdr)) return -1;
    if (body_len > 0 && body) {
        if (send(fd, body, (size_t)body_len, 0) != (ssize_t)body_len) return -1;
    }
    return 0;
}

/* ── TX meta parsing ──────────────────────────────────────────────────────────── */

static int parse_tx_meta(const char *body, uint64_t body_len,
                          ftx2_tx_meta_t *out,
                          const char **extra, uint64_t *extra_len) {
    if (!body || !out) return -1;
    memset(out, 0, sizeof(*out));
    if (body_len < FTX2_TX_META_LEN) return -1;
    memcpy(out->tx_id, body, 16);
    out->kind  = read_le32((const unsigned char *)body + 16);
    out->flags = read_le32((const unsigned char *)body + 20);
    if (extra)     *extra     = body + FTX2_TX_META_LEN;
    if (extra_len) *extra_len = body_len - FTX2_TX_META_LEN;
    return 0;
}

/* ── STREAM_SHARD handler ─────────────────────────────────────────────────────── */

/* Drain exactly `remaining` bytes from `fd` in 64 KiB chunks. */
static int drain_shard_data(int fd, uint64_t remaining) {
    char buf[FTX2_SHARD_DRAIN_BUF];
    while (remaining > 0) {
        size_t take = remaining > sizeof(buf) ? sizeof(buf) : (size_t)remaining;
        if (recv_exact(fd, buf, take) != 0) return -1;
        remaining -= (uint64_t)take;
    }
    return 0;
}

/*
 * Write shard data from the socket into a spool file.
 * Falls back to drain if the spool directory cannot be created or the file
 * cannot be opened (so the connection is never left in a bad read state).
 *
 * Spool layout:  PS5UPLOAD2_SPOOL_DIR/spool_<tx_id_hex>/<shard_seq>
 */
/*
 * Write `len` bytes from `buf` to `fd` with retry on short writes / EINTR.
 * Returns 0 on success, -1 on any permanent I/O error.
 */
static int write_full(int fd, const void *buf, size_t len) {
    const unsigned char *p = (const unsigned char *)buf;
    size_t written = 0;
    while (written < len) {
        ssize_t w = write(fd, p + written, len - written);
        if (w < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (w == 0) return -1;
        written += (size_t)w;
    }
    return 0;
}

/* ── Double-buffered writer thread ────────────────────────────────────────────
 *
 * The shard receive loop is recv-then-write per chunk, serialised on the main
 * thread. For a 128 MiB transfer on PS5, recv takes ~2.3 s and write takes
 * ~4 s — overlapping them with a helper thread lets the two phases run in
 * parallel, bounded by `max(recv, write)` instead of the sum.
 *
 * Design: exactly two buffers. The producer (main thread) fills one while the
 * consumer (writer thread) drains the other, then they swap. One mutex, one
 * condvar per slot. Consumer exits on a sentinel `len < 0`.
 */

typedef struct {
    unsigned char *buf;
    ssize_t        len;  /* 0 = empty (writer may refill), >0 = full (writer must drain), <0 = shutdown */
} piped_slot_t;

typedef struct {
    piped_slot_t    slot[2];
    pthread_mutex_t lock;
    pthread_cond_t  cv_full[2];   /* signaled when slot[i] becomes full */
    pthread_cond_t  cv_empty[2];  /* signaled when slot[i] becomes empty */
    int             fd;
    int             writer_error; /* set by writer on I/O failure */
} piped_writer_t;

/*
 * Persistent writer handle attached to runtime_tx_entry_t::direct_writer.
 *
 * Lifecycle (single-file direct mode, total_bytes >= threshold):
 *   BEGIN_TX       → nothing (lazy)
 *   first shard    → direct_writer_start() mallocs this struct + 2× io bufs,
 *                    pthread_creates the writer thread, stores the handle
 *   each shard     → producer reuses pw->slot[direct_slot] just like today's
 *                    inline writer path, but never sends the sentinel
 *   COMMIT_TX      → direct_writer_finish() sends sentinel + joins + returns
 *                    pw->writer_error
 *   any terminal   → direct_writer_discard() sends sentinel, joins, frees,
 *                    ignores writer_error (we're unlinking the tmp anyway)
 *
 * The writer thread itself is vanilla piped_writer_thread() — no change.
 * All state the thread touches lives in `pw` (heap), so tx_entry re-shuffling
 * or slot eviction during background writes is safe as long as we haven't
 * freed `pw` yet. direct_writer_discard() is the only legal free path. */
typedef struct {
    piped_writer_t pw;          /* first so &handle == &pw (mutex init unchanged) */
    pthread_t      tid;
    int            started;     /* 1 when tid is joinable */
    unsigned char *bufs[2];     /* heap-owned double buffers (also in pw.slot[i].buf) */
} direct_writer_handle_t;

/* Forward declarations for the persistent direct-write helpers. */
static int  direct_writer_start(runtime_tx_entry_t *entry, int fd);
static int  direct_writer_finish(runtime_tx_entry_t *entry);
static void direct_writer_discard(runtime_tx_entry_t *entry);

static void *piped_writer_thread(void *arg) {
    piped_writer_t *pw = (piped_writer_t *)arg;
    int i = 0;
    for (;;) {
        ssize_t len;
        unsigned char *buf;
        pthread_mutex_lock(&pw->lock);
        while (pw->slot[i].len == 0) {
            pthread_cond_wait(&pw->cv_full[i], &pw->lock);
        }
        len = pw->slot[i].len;
        buf = pw->slot[i].buf;
        pthread_mutex_unlock(&pw->lock);

        if (len < 0) break; /* shutdown */
        if (write_full(pw->fd, buf, (size_t)len) != 0) {
            pthread_mutex_lock(&pw->lock);
            pw->writer_error = 1;
            /* Mark remaining slots empty so producer can unblock. */
            pw->slot[i].len = 0;
            pthread_cond_signal(&pw->cv_empty[i]);
            pthread_mutex_unlock(&pw->lock);
            return NULL;
        }
        pthread_mutex_lock(&pw->lock);
        pw->slot[i].len = 0;
        pthread_cond_signal(&pw->cv_empty[i]);
        pthread_mutex_unlock(&pw->lock);
        i = 1 - i;
    }
    return NULL;
}

/*
 * Spawn a persistent writer thread bound to `fd` and attach it to `entry`.
 * `fd` ownership transfers to the handle on success (closed by finish/discard).
 * On failure, `fd` is left open for the caller to deal with.
 * Returns 0 on success, -1 on allocation/pthread failure.
 */
static int direct_writer_start(runtime_tx_entry_t *entry, int fd) {
    direct_writer_handle_t *h = NULL;
    if (!entry || entry->direct_writer || fd < 0) return -1;
    h = (direct_writer_handle_t *)calloc(1, sizeof(*h));
    if (!h) return -1;
    h->bufs[0] = (unsigned char *)malloc(PS5UPLOAD2_SHARD_IO_BUF);
    h->bufs[1] = (unsigned char *)malloc(PS5UPLOAD2_SHARD_IO_BUF);
    if (!h->bufs[0] || !h->bufs[1]) {
        free(h->bufs[0]);
        free(h->bufs[1]);
        free(h);
        return -1;
    }
    h->pw.fd = fd;
    h->pw.slot[0].buf = h->bufs[0];
    h->pw.slot[1].buf = h->bufs[1];
    pthread_mutex_init(&h->pw.lock, NULL);
    pthread_cond_init(&h->pw.cv_full[0], NULL);
    pthread_cond_init(&h->pw.cv_full[1], NULL);
    pthread_cond_init(&h->pw.cv_empty[0], NULL);
    pthread_cond_init(&h->pw.cv_empty[1], NULL);
    if (pthread_create(&h->tid, NULL, piped_writer_thread, &h->pw) != 0) {
        pthread_mutex_destroy(&h->pw.lock);
        pthread_cond_destroy(&h->pw.cv_full[0]);
        pthread_cond_destroy(&h->pw.cv_full[1]);
        pthread_cond_destroy(&h->pw.cv_empty[0]);
        pthread_cond_destroy(&h->pw.cv_empty[1]);
        free(h->bufs[0]);
        free(h->bufs[1]);
        free(h);
        return -1;
    }
    h->started = 1;
    entry->direct_writer = h;
    entry->direct_slot   = 0;
    return 0;
}

/*
 * Drain the persistent writer cleanly on the success path: send the `len<0`
 * sentinel on the next producer slot, wait for it to be accepted, join the
 * thread, tear down sync primitives + buffers + fd, clear the handle.
 * Returns 0 if the writer reported no I/O error, -1 otherwise.
 *
 * `direct_fd_open` / `direct_fd` on the entry are cleared by this function
 * because the handle owns the fd once direct_writer_start succeeds.
 */
static int direct_writer_finish(runtime_tx_entry_t *entry) {
    direct_writer_handle_t *h = NULL;
    int writer_error = 0;
    int slot = 0;
    if (!entry || !entry->direct_writer) return 0;
    h = (direct_writer_handle_t *)entry->direct_writer;
    slot = entry->direct_slot;
    if (slot < 0 || slot > 1) slot = 0;
    /* Wait for the sentinel slot to be free, then post the shutdown marker. */
    pthread_mutex_lock(&h->pw.lock);
    while (h->pw.slot[slot].len > 0 && !h->pw.writer_error) {
        pthread_cond_wait(&h->pw.cv_empty[slot], &h->pw.lock);
    }
    h->pw.slot[slot].len = -1;
    pthread_cond_signal(&h->pw.cv_full[slot]);
    pthread_mutex_unlock(&h->pw.lock);
    if (h->started) pthread_join(h->tid, NULL);
    writer_error = h->pw.writer_error;
    pthread_mutex_destroy(&h->pw.lock);
    pthread_cond_destroy(&h->pw.cv_full[0]);
    pthread_cond_destroy(&h->pw.cv_full[1]);
    pthread_cond_destroy(&h->pw.cv_empty[0]);
    pthread_cond_destroy(&h->pw.cv_empty[1]);
    free(h->bufs[0]);
    free(h->bufs[1]);
    if (entry->direct_fd_open) {
        close(entry->direct_fd);
        entry->direct_fd_open = 0;
        entry->direct_fd      = -1;
    }
    free(h);
    entry->direct_writer = NULL;
    entry->direct_slot   = 0;
    return writer_error ? -1 : 0;
}

/*
 * Teardown variant for abort/takeover/shutdown/fatal paths. Sends the
 * sentinel + joins + frees exactly like finish(), but ignores writer_error
 * (the caller is about to unlink the tmp anyway). Idempotent and safe to
 * call when the writer was never started. Also closes direct_fd if it was
 * opened without spawning a writer (small-tx path below the threshold).
 */
static void direct_writer_discard(runtime_tx_entry_t *entry) {
    if (!entry) return;
    if (entry->direct_writer) {
        (void)direct_writer_finish(entry);
    } else if (entry->direct_fd_open) {
        close(entry->direct_fd);
        entry->direct_fd_open = 0;
        entry->direct_fd      = -1;
    }
}

/* ── Packed-shard worker pool ─────────────────────────────────────────────────
 *
 * For many-small-file workloads, per-record `open + write + close` dominates.
 * Serialising it in the main thread caps throughput at the FS's per-file cost
 * (~23 ms/file on PS5 UFS), wasting the wire (NIC is ~100× faster than disk
 * metadata). A small pool of kernel threads lets metadata operations overlap
 * across files — the real gain is "files in different directories can be
 * created concurrently"; a secondary gain is "open/write/close of one file
 * overlap with open/write/close of another in the same dir".
 *
 * Lifecycle mirrors the single-file persistent writer (runtime_release_tx_resources
 * is the sole teardown site):
 *   first packed shard   → pack_pool_start() mallocs handle + spawns workers
 *   every record         → reader pushes (path, data); worker pops + writes
 *   end of each shard    → pack_pool_drain() waits for queue empty before ACK,
 *                          preserving the "SPOOLED == persisted" semantic the
 *                          protocol promises on StreamShard ACK
 *   terminal transition  → pack_pool_teardown() drains + shutdown + join,
 *                          sums per-worker timings into the tx entry so they
 *                          surface in COMMIT_TX_ACK, frees all memory
 *
 * Safety notes:
 *   - reader owns the fs-metadata cache (last_parent_dir); workers don't
 *     touch it, so no mutex needed for the mkdir-p cache
 *   - workers drain the queue even after worker_error is set, so the reader
 *     is never blocked on a permanently-stuck pool_drain
 *   - worker errors are sticky; no retry semantics inside the pool
 */

typedef struct pack_work_item {
    char          *path;      /* NUL-terminated, malloc'd by reader, freed by worker */
    unsigned char *data;      /* may be NULL when data_len == 0 */
    uint32_t       data_len;
} pack_work_item_t;

typedef struct pack_worker_pool {
    pthread_t        tid[PS5UPLOAD2_PACK_WORKERS];
    int              tid_started;      /* count of actually-created threads */
    pack_work_item_t queue[PS5UPLOAD2_PACK_QUEUE_DEPTH];
    int              q_head;           /* next slot to pop */
    int              q_tail;           /* next slot to push */
    int              q_count;
    int              shutdown;         /* set on teardown; workers drain then exit */
    int              worker_error;     /* sticky I/O failure flag */
    pthread_mutex_t  lock;
    pthread_cond_t   cv_not_full;      /* reader waits when queue full */
    pthread_cond_t   cv_not_empty;     /* workers wait when queue empty */
    pthread_cond_t   cv_drained;       /* reader waits during drain */
    /* Accumulators — updated by workers under `lock` at end of each item.
     * Contention is minimal (4 workers, each update is a few arithmetic ops
     * and a conditional signal), so sharing one mutex keeps code simple. */
    uint64_t         t_unlink_us;
    uint64_t         t_open_us;
    uint64_t         t_ftruncate_us;
    uint64_t         t_write_us;
    uint64_t         t_close_us;
    uint64_t         records_processed;
} pack_worker_pool_t;

static void *pack_worker_thread(void *arg) {
    pack_worker_pool_t *pool = (pack_worker_pool_t *)arg;
    for (;;) {
        pack_work_item_t item;
        pthread_mutex_lock(&pool->lock);
        while (pool->q_count == 0 && !pool->shutdown) {
            pthread_cond_wait(&pool->cv_not_empty, &pool->lock);
        }
        if (pool->q_count == 0 && pool->shutdown) {
            pthread_mutex_unlock(&pool->lock);
            return NULL;
        }
        item = pool->queue[pool->q_head];
        pool->q_head = (pool->q_head + 1) % PS5UPLOAD2_PACK_QUEUE_DEPTH;
        pool->q_count -= 1;
        pthread_cond_signal(&pool->cv_not_full);
        if (pool->q_count == 0) pthread_cond_signal(&pool->cv_drained);
        pthread_mutex_unlock(&pool->lock);

        if (!pool->worker_error) {
            /* Packed records write directly to their final path — no tmp +
             * rename dance. The tmp pattern's only benefit is mid-transfer
             * atomicity, but on PS5 UFS each `.ps5up2-tmp` unlink/open/rename
             * costs ~40 ms of metadata work, and the COMMIT-side rename of
             * 5000 files added another ~5 s of apply time. By writing
             * directly we lose only the "partial-file-on-abort" guarantee,
             * which is already an acceptable state: aborted multi-file
             * transfers were never cleaned up field-by-field anyway, and the
             * CLEANUP frame exists precisely to handle residual state. Huge
             * single-file direct writes still use the tmp+rename pattern
             * (`entry->tmp_path`) because there the rename is O(1) and the
             * atomicity guarantee is more valuable. */
            int fd = -1;
            uint64_t t0, t_op = 0, t_tr = 0, t_wr = 0, t_cl = 0;

            t0 = now_us();
            fd = open(item.path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
            t_op = now_us() - t0;

            if (fd < 0) {
                fprintf(stderr, "[payload2] pack worker: open %s failed errno=%d\n",
                        item.path, errno);
                pool->worker_error = 1;
            } else {
                /* Historical note: an earlier experiment called
                 * `posix_fadvise(fd, 0, 0, POSIX_FADV_DONTNEED)` here to try
                 * to keep the page cache clean between transactions. On the
                 * e1000 lab NIC it produced a measurable throughput regression
                 * on the 1 GiB single-file path (88.65 -> 84.87 MiB/s), likely
                 * because DONTNEED forces synchronous writeback of dirty pages
                 * through the one-remaining-writer-thread bottleneck. The hint
                 * turned out to have negative ROI on this hardware, so we
                 * don't call it. Revisit when we have different NIC / workload
                 * evidence that shows it actually helping. */
                if (item.data_len > 0) {
                    t0 = now_us();
                    (void)ftruncate(fd, (off_t)item.data_len);
                    t_tr = now_us() - t0;

                    t0 = now_us();
                    if (write_full(fd, item.data, item.data_len) != 0) {
                        fprintf(stderr, "[payload2] pack worker: write %s failed errno=%d\n",
                                item.path, errno);
                        pool->worker_error = 1;
                    }
                    t_wr = now_us() - t0;
                }
                t0 = now_us();
                (void)close(fd);
                t_cl = now_us() - t0;
            }

            /* Fold per-item timings into the shared accumulator. */
            pthread_mutex_lock(&pool->lock);
            pool->t_open_us       += t_op;
            pool->t_ftruncate_us  += t_tr;
            pool->t_write_us      += t_wr;
            pool->t_close_us      += t_cl;
            pool->records_processed += 1;
            pthread_mutex_unlock(&pool->lock);
        }

        /* Free the reader-allocated buffers regardless of error state so the
         * queue always drains cleanly — the reader relies on that invariant. */
        free(item.path);
        if (item.data) free(item.data);
    }
}

/* Lazily start a pool attached to `entry`. Returns 0 on success, -1 on
 * pthread/allocation failure; on failure `entry->pack_pool` is left NULL so
 * the caller can fall back to the inline serial path. */
static int pack_pool_start(runtime_tx_entry_t *entry) {
    pack_worker_pool_t *pool;
    int i;
    if (!entry || entry->pack_pool) return 0;
    pool = (pack_worker_pool_t *)calloc(1, sizeof(*pool));
    if (!pool) return -1;
    pthread_mutex_init(&pool->lock, NULL);
    pthread_cond_init(&pool->cv_not_full, NULL);
    pthread_cond_init(&pool->cv_not_empty, NULL);
    pthread_cond_init(&pool->cv_drained, NULL);
    for (i = 0; i < (int)PS5UPLOAD2_PACK_WORKERS; i++) {
        if (pthread_create(&pool->tid[i], NULL, pack_worker_thread, pool) != 0) {
            /* Could not spawn all workers. Tear down the ones we did spawn
             * and fall back. Setting shutdown + broadcasting wakes the
             * already-running workers; they exit because queue is empty. */
            pool->shutdown = 1;
            pthread_cond_broadcast(&pool->cv_not_empty);
            {
                int j;
                for (j = 0; j < pool->tid_started; j++) {
                    pthread_join(pool->tid[j], NULL);
                }
            }
            pthread_mutex_destroy(&pool->lock);
            pthread_cond_destroy(&pool->cv_not_full);
            pthread_cond_destroy(&pool->cv_not_empty);
            pthread_cond_destroy(&pool->cv_drained);
            free(pool);
            return -1;
        }
        pool->tid_started += 1;
    }
    entry->pack_pool = pool;
    return 0;
}

/* Push one work item. Blocks if queue full. Returns 0 on success; returns
 * -1 only if the pool already has a sticky worker_error (in which case
 * ownership of path/data stays with the caller, which must free them). */
static int pack_pool_push(pack_worker_pool_t *pool, char *path,
                          unsigned char *data, uint32_t data_len) {
    if (!pool || !path) return -1;
    pthread_mutex_lock(&pool->lock);
    while (pool->q_count >= (int)PS5UPLOAD2_PACK_QUEUE_DEPTH && !pool->worker_error) {
        pthread_cond_wait(&pool->cv_not_full, &pool->lock);
    }
    if (pool->worker_error) {
        pthread_mutex_unlock(&pool->lock);
        return -1;
    }
    pool->queue[pool->q_tail].path     = path;
    pool->queue[pool->q_tail].data     = data;
    pool->queue[pool->q_tail].data_len = data_len;
    pool->q_tail = (pool->q_tail + 1) % PS5UPLOAD2_PACK_QUEUE_DEPTH;
    pool->q_count += 1;
    pthread_cond_signal(&pool->cv_not_empty);
    pthread_mutex_unlock(&pool->lock);
    return 0;
}

/* Block until the queue is empty. Returns 0 if the pool is clean, -1 if a
 * worker has set the sticky error flag. The reader calls this at end-of-shard
 * so SHARD_ACK means "persisted", same contract as the serial code path. */
static int pack_pool_drain(pack_worker_pool_t *pool) {
    int err;
    if (!pool) return 0;
    pthread_mutex_lock(&pool->lock);
    while (pool->q_count > 0) {
        pthread_cond_wait(&pool->cv_drained, &pool->lock);
    }
    err = pool->worker_error;
    pthread_mutex_unlock(&pool->lock);
    return err ? -1 : 0;
}

/* Drain + shutdown + join + sum timings into entry + free. Called from
 * runtime_release_tx_resources only; idempotent against NULL. */
static void pack_pool_teardown(runtime_tx_entry_t *entry) {
    pack_worker_pool_t *pool;
    int i;
    if (!entry || !entry->pack_pool) return;
    pool = (pack_worker_pool_t *)entry->pack_pool;

    /* Wait for in-flight work to finish — workers are reliable drain-ers
     * (they continue popping + freeing even after worker_error is set). */
    pthread_mutex_lock(&pool->lock);
    while (pool->q_count > 0) {
        pthread_cond_wait(&pool->cv_drained, &pool->lock);
    }
    pool->shutdown = 1;
    pthread_cond_broadcast(&pool->cv_not_empty);
    pthread_mutex_unlock(&pool->lock);

    for (i = 0; i < pool->tid_started; i++) {
        pthread_join(pool->tid[i], NULL);
    }

    /* Fold pool timings into the entry so COMMIT_TX_ACK can report them. */
    entry->pack_records      += pool->records_processed;
    entry->pack_unlink_us    += pool->t_unlink_us;
    entry->pack_open_us      += pool->t_open_us;
    entry->pack_ftruncate_us += pool->t_ftruncate_us;
    entry->pack_write_us     += pool->t_write_us;
    entry->pack_close_us     += pool->t_close_us;

    pthread_mutex_destroy(&pool->lock);
    pthread_cond_destroy(&pool->cv_not_full);
    pthread_cond_destroy(&pool->cv_not_empty);
    pthread_cond_destroy(&pool->cv_drained);
    free(pool);
    entry->pack_pool = NULL;
}

/*
 * Read shard data from the socket, BLAKE3-hash it on the fly, and write it
 * to `path`. Uses raw POSIX `open(2)` + `write(2)` — bypassing stdio buffering
 * matches the legacy payload's high-throughput write pattern.
 *
 * `truncate`:
 *   - non-zero: open with O_WRONLY|O_CREAT|O_TRUNC — starts a fresh file
 *   - zero:     open with O_WRONLY|O_APPEND       — appends to existing file
 *
 * `preallocate_bytes`: if > 0 and the file is being truncated, `ftruncate(2)`
 * the file to that size before writing, so the kernel can lay out contiguous
 * blocks. Matches `payload/protocol.c` `handle_upload` large-file path.
 *
 * On success, if `out_digest` is non-NULL, it holds the BLAKE3-256 of the
 * shard contents — no re-read is needed for verification.
 *
 * Falls back to plain drain if `open` fails, so the socket is never left
 * in a bad read state.
 */
static int runtime_write_shard_to_path(runtime_tx_entry_t *entry,
                                        const char *path,
                                        int truncate,
                                        uint64_t preallocate_bytes,
                                        int client_fd,
                                        uint64_t data_len,
                                        unsigned char *out_digest /* BLAKE3_OUT_LEN */) {
    unsigned char *bufs[2] = {NULL, NULL};
    int fd = -1;
    int flags;
    uint64_t remaining = data_len;
    uint64_t t_recv = 0;
    uint64_t t_write_wait = 0;
    uint64_t t_hash = 0;
    uint64_t t_func_start = now_us();
    uint64_t t_open_start = 0;
    uint64_t t_open_us = 0;
    uint64_t t_join_us = 0;
    uint64_t t_close_us = 0;
    blake3_hasher hasher;
    int hash_enabled = (out_digest != NULL);
    piped_writer_t pw;
    pthread_t writer_tid;
    int slot = 0;
    int rc_ret = 0;

    if (out_digest) memset(out_digest, 0, BLAKE3_OUT_LEN);
    if (hash_enabled) blake3_hasher_init(&hasher);

    if (data_len == 0) {
        if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
        return 0;
    }

    bufs[0] = (unsigned char *)malloc(PS5UPLOAD2_SHARD_IO_BUF);
    bufs[1] = (unsigned char *)malloc(PS5UPLOAD2_SHARD_IO_BUF);
    if (!bufs[0] || !bufs[1]) {
        free(bufs[0]);
        free(bufs[1]);
        return drain_shard_data(client_fd, data_len);
    }

    t_open_start = now_us();
    flags = O_WRONLY | O_CREAT | (truncate ? O_TRUNC : O_APPEND);
    fd = open(path, flags, 0666);
    if (fd < 0) {
        free(bufs[0]);
        free(bufs[1]);
        return drain_shard_data(client_fd, data_len);
    }
    if (truncate && preallocate_bytes > 0) {
        /* Pre-allocate to reduce filesystem fragmentation on large files. */
        (void)ftruncate(fd, (off_t)preallocate_bytes);
    }
    t_open_us = now_us() - t_open_start;

    /* Decide whether to spawn the double-buffered writer thread at all.
     * For small shards (typical PS5 game-dir file <64 KiB), the pthread
     * create/join cost dominates — measured at ~4–6 ms per shard on FreeBSD 11,
     * vs ~20 µs for the 4 KiB write itself. For large shards the overlap
     * between recv and write is still worth the thread overhead. */
    if (data_len < PS5UPLOAD2_PIPED_THREAD_MIN_BYTES) {
        while (remaining > 0) {
            size_t take = remaining > PS5UPLOAD2_SHARD_IO_BUF
                            ? PS5UPLOAD2_SHARD_IO_BUF
                            : (size_t)remaining;
            uint64_t t0 = now_us();
            if (recv_exact(client_fd, bufs[0], take) != 0) { rc_ret = -1; break; }
            t_recv += (now_us() - t0);
            if (hash_enabled) {
                uint64_t th = now_us();
                blake3_hasher_update(&hasher, bufs[0], take);
                t_hash += (now_us() - th);
            }
            if (write_full(fd, bufs[0], take) != 0) { rc_ret = -1; break; }
            remaining -= (uint64_t)take;
        }
        {
            uint64_t t_close_start = now_us();
            close(fd);
            t_close_us = now_us() - t_close_start;
        }
        free(bufs[0]);
        free(bufs[1]);
        if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
        if (entry && rc_ret == 0) {
            entry->recv_us       += t_recv;
            entry->open_us       += t_open_us;
            entry->close_us      += t_close_us;
            entry->hash_us       += t_hash;
            entry->shard_func_us += (now_us() - t_func_start);
        }
        return rc_ret;
    }

    /* Set up double-buffered writer thread so recv and write overlap. */
    memset(&pw, 0, sizeof(pw));
    pw.fd = fd;
    pw.slot[0].buf = bufs[0];
    pw.slot[1].buf = bufs[1];
    pthread_mutex_init(&pw.lock, NULL);
    pthread_cond_init(&pw.cv_full[0], NULL);
    pthread_cond_init(&pw.cv_full[1], NULL);
    pthread_cond_init(&pw.cv_empty[0], NULL);
    pthread_cond_init(&pw.cv_empty[1], NULL);
    if (pthread_create(&writer_tid, NULL, piped_writer_thread, &pw) != 0) {
        /* Fall back to single-buffered in-thread writes if thread create fails. */
        pthread_mutex_destroy(&pw.lock);
        pthread_cond_destroy(&pw.cv_full[0]);
        pthread_cond_destroy(&pw.cv_full[1]);
        pthread_cond_destroy(&pw.cv_empty[0]);
        pthread_cond_destroy(&pw.cv_empty[1]);
        while (remaining > 0) {
            size_t take = remaining > PS5UPLOAD2_SHARD_IO_BUF
                            ? PS5UPLOAD2_SHARD_IO_BUF
                            : (size_t)remaining;
            if (recv_exact(client_fd, bufs[0], take) != 0) { rc_ret = -1; break; }
            if (hash_enabled) blake3_hasher_update(&hasher, bufs[0], take);
            if (write_full(fd, bufs[0], take) != 0) { rc_ret = -1; break; }
            remaining -= (uint64_t)take;
        }
        close(fd);
        free(bufs[0]);
        free(bufs[1]);
        if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
        return rc_ret;
    }

    while (remaining > 0) {
        uint64_t t0, t1, t2;
        size_t take = remaining > PS5UPLOAD2_SHARD_IO_BUF
                        ? PS5UPLOAD2_SHARD_IO_BUF
                        : (size_t)remaining;
        /* Wait for the target slot to be free (writer has finished draining it). */
        t0 = now_us();
        pthread_mutex_lock(&pw.lock);
        while (pw.slot[slot].len > 0 && !pw.writer_error) {
            pthread_cond_wait(&pw.cv_empty[slot], &pw.lock);
        }
        if (pw.writer_error) {
            pthread_mutex_unlock(&pw.lock);
            rc_ret = -1;
            break;
        }
        pthread_mutex_unlock(&pw.lock);
        t1 = now_us();
        t_write_wait += (t1 - t0);

        if (recv_exact(client_fd, pw.slot[slot].buf, take) != 0) {
            rc_ret = -1;
            break;
        }
        t2 = now_us();
        t_recv += (t2 - t1);

        if (hash_enabled) {
            uint64_t t_hash_start = now_us();
            blake3_hasher_update(&hasher, pw.slot[slot].buf, take);
            t_hash += (now_us() - t_hash_start);
        }

        /* Hand slot to writer. */
        pthread_mutex_lock(&pw.lock);
        pw.slot[slot].len = (ssize_t)take;
        pthread_cond_signal(&pw.cv_full[slot]);
        pthread_mutex_unlock(&pw.lock);
        slot = 1 - slot;
        remaining -= (uint64_t)take;
    }

    /* Signal shutdown on the next slot the writer will pick up. */
    pthread_mutex_lock(&pw.lock);
    /* Wait for the sentinel slot to be free before writing it. */
    while (pw.slot[slot].len > 0 && !pw.writer_error) {
        pthread_cond_wait(&pw.cv_empty[slot], &pw.lock);
    }
    pw.slot[slot].len = -1;
    pthread_cond_signal(&pw.cv_full[slot]);
    pthread_mutex_unlock(&pw.lock);

    {
        uint64_t t_join_start = now_us();
        pthread_join(writer_tid, NULL);
        t_join_us = now_us() - t_join_start;
    }
    if (pw.writer_error) rc_ret = -1;

    {
        uint64_t t_close_start = now_us();
        close(fd);
        t_close_us = now_us() - t_close_start;
    }
    pthread_mutex_destroy(&pw.lock);
    pthread_cond_destroy(&pw.cv_full[0]);
    pthread_cond_destroy(&pw.cv_full[1]);
    pthread_cond_destroy(&pw.cv_empty[0]);
    pthread_cond_destroy(&pw.cv_empty[1]);
    free(bufs[0]);
    free(bufs[1]);
    if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
    if (entry && rc_ret == 0) {
        entry->recv_us       += t_recv;
        /* `write_us` now counts time the producer was blocked waiting on the
         * writer — roughly the *excess* write time over recv time. A healthy
         * pipeline has write_us small; if it's large, writes are the limit. */
        entry->write_us      += t_write_wait;
        entry->open_us       += t_open_us;
        entry->join_us       += t_join_us;
        entry->close_us      += t_close_us;
        entry->hash_us       += t_hash;
        entry->shard_func_us += (now_us() - t_func_start);
    }
    return rc_ret;
}

/*
 * Single-file direct-mode persistent-fd shard write path.
 *
 * The first shard opens `entry->tmp_path` with O_TRUNC + ftruncate
 * preallocation (if total_bytes is known), stashes the fd on `entry`, and
 * — if the expected total tx size is large enough to amortise pthread
 * overhead — spawns a long-lived writer thread via direct_writer_start().
 *
 * Every shard pumps recv→hash→slot exactly like the large-shard inner loop
 * of runtime_write_shard_to_path, but without spawning/joining/closing.
 *
 * On write-path I/O failure the caller receives -1; it must then call
 * runtime_abort_tx_fatal (which goes through runtime_release_tx_resources
 * and tears the writer down) — the persistent writer is not safe to reuse
 * after writer_error is set.
 *
 * Small single-file txs (total_bytes below the threshold) keep the fd
 * persistent but do sync writes inline — the writer-thread overhead isn't
 * worth it for a few shards' worth of bytes, and fd reuse still saves the
 * 4× open+close overhead per shard that the old per-shard path paid.
 */
static int runtime_write_shard_persistent(runtime_tx_entry_t *entry,
                                           int client_fd,
                                           uint64_t data_len,
                                           int is_first_shard,
                                           uint64_t preallocate_bytes,
                                           unsigned char *out_digest) {
    uint64_t remaining = data_len;
    uint64_t t_recv = 0;
    uint64_t t_write_wait = 0;
    uint64_t t_hash = 0;
    uint64_t t_func_start = now_us();
    uint64_t t_open_us = 0;
    uint64_t t_close_us = 0;
    blake3_hasher hasher;
    int hash_enabled = (out_digest != NULL);
    int rc_ret = 0;
    direct_writer_handle_t *h = NULL;
    int use_writer_thread = 0;
    unsigned char *sync_buf = NULL;

    if (!entry) return -1;
    if (out_digest) memset(out_digest, 0, BLAKE3_OUT_LEN);
    if (hash_enabled) blake3_hasher_init(&hasher);

    /* First shard: open the tmp file and, if worth it, start the writer. */
    if (is_first_shard && !entry->direct_fd_open) {
        int flags = O_WRONLY | O_CREAT | O_TRUNC;
        uint64_t t_open_start = now_us();
        int fd = open(entry->tmp_path, flags, 0666);
        if (fd < 0) {
            fprintf(stderr, "[payload2] direct persistent: open %s failed errno=%d\n",
                    entry->tmp_path, errno);
            return drain_shard_data(client_fd, data_len);
        }
        if (preallocate_bytes > 0) {
            (void)ftruncate(fd, (off_t)preallocate_bytes);
        }
        /* posix_fadvise(SEQUENTIAL) was experimented with here; measured
         * neutral-to-slightly-negative on the e1000 lab NIC and produced no
         * observable benefit on huge-file throughput (already NIC-bound).
         * Removed to avoid any chance of unexpected kernel-scheduler
         * interactions. See bench/reports/2026-04-18T01* for the A/B data. */
        entry->direct_fd      = fd;
        entry->direct_fd_open = 1;
        t_open_us = now_us() - t_open_start;

        /* Spawn the writer thread only if the tx is big enough to amortise
         * pthread create/join cost. Threshold matches the per-shard threshold
         * on the old path — same "big enough to benefit from overlap" idea,
         * just applied once per tx instead of once per shard. */
        if (entry->total_bytes >= PS5UPLOAD2_PIPED_THREAD_MIN_BYTES) {
            if (direct_writer_start(entry, fd) != 0) {
                /* Fall back to sync writes on a persistent fd — slower but
                 * functionally correct. Don't close fd; entry owns it. */
                fprintf(stderr, "[payload2] direct persistent: writer start failed, sync fallback\n");
            }
        }
    }

    if (entry->direct_writer) {
        h = (direct_writer_handle_t *)entry->direct_writer;
        use_writer_thread = 1;
    }

    if (data_len == 0) {
        if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
        if (entry) {
            entry->open_us       += t_open_us;
            entry->shard_func_us += (now_us() - t_func_start);
        }
        return 0;
    }

    if (!use_writer_thread) {
        /* Sync write path — one reusable buffer, persistent fd. */
        sync_buf = (unsigned char *)malloc(PS5UPLOAD2_SHARD_IO_BUF);
        if (!sync_buf) return drain_shard_data(client_fd, data_len);
        while (remaining > 0) {
            size_t take = remaining > PS5UPLOAD2_SHARD_IO_BUF
                            ? PS5UPLOAD2_SHARD_IO_BUF
                            : (size_t)remaining;
            uint64_t t0 = now_us();
            if (recv_exact(client_fd, sync_buf, take) != 0) { rc_ret = -1; break; }
            t_recv += (now_us() - t0);
            if (hash_enabled) {
                uint64_t th = now_us();
                blake3_hasher_update(&hasher, sync_buf, take);
                t_hash += (now_us() - th);
            }
            if (write_full(entry->direct_fd, sync_buf, take) != 0) {
                rc_ret = -1; break;
            }
            remaining -= (uint64_t)take;
        }
        free(sync_buf);
        if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
        if (rc_ret == 0) {
            entry->recv_us       += t_recv;
            entry->open_us       += t_open_us;
            entry->hash_us       += t_hash;
            entry->shard_func_us += (now_us() - t_func_start);
        }
        return rc_ret;
    }

    /* Persistent writer-thread path — reuse pw.slot[direct_slot] across
     * shards. The writer's internal slot counter and our direct_slot both
     * advance 0→1→0 in lockstep, so as long as neither side re-initializes
     * they stay aligned across shards. */
    while (remaining > 0) {
        int slot = entry->direct_slot;
        uint64_t t0, t1, t2;
        size_t take = remaining > PS5UPLOAD2_SHARD_IO_BUF
                        ? PS5UPLOAD2_SHARD_IO_BUF
                        : (size_t)remaining;
        t0 = now_us();
        pthread_mutex_lock(&h->pw.lock);
        while (h->pw.slot[slot].len > 0 && !h->pw.writer_error) {
            pthread_cond_wait(&h->pw.cv_empty[slot], &h->pw.lock);
        }
        if (h->pw.writer_error) {
            pthread_mutex_unlock(&h->pw.lock);
            rc_ret = -1;
            break;
        }
        pthread_mutex_unlock(&h->pw.lock);
        t1 = now_us();
        t_write_wait += (t1 - t0);

        if (recv_exact(client_fd, h->pw.slot[slot].buf, take) != 0) {
            rc_ret = -1;
            break;
        }
        t2 = now_us();
        t_recv += (t2 - t1);

        if (hash_enabled) {
            uint64_t t_hash_start = now_us();
            blake3_hasher_update(&hasher, h->pw.slot[slot].buf, take);
            t_hash += (now_us() - t_hash_start);
        }

        pthread_mutex_lock(&h->pw.lock);
        h->pw.slot[slot].len = (ssize_t)take;
        pthread_cond_signal(&h->pw.cv_full[slot]);
        pthread_mutex_unlock(&h->pw.lock);
        entry->direct_slot = 1 - slot;
        remaining -= (uint64_t)take;
    }

    if (hash_enabled) blake3_hasher_finalize(&hasher, out_digest, BLAKE3_OUT_LEN);
    /* Note: close_us stays 0 for persistent path — fd only closes at COMMIT. */
    (void)t_close_us;
    if (rc_ret == 0) {
        entry->recv_us       += t_recv;
        entry->write_us      += t_write_wait;
        entry->open_us       += t_open_us;
        entry->hash_us       += t_hash;
        entry->shard_func_us += (now_us() - t_func_start);
    }
    return rc_ret;
}

/* Spool-path wrapper (kept for fallback when direct mode is not engaged). */
static int runtime_write_shard_data(runtime_tx_entry_t *entry,
                                     uint64_t shard_seq,
                                     int client_fd,
                                     uint64_t data_len,
                                     unsigned char *out_digest) {
    char spool_dir[512];
    char path[512];
    if (!entry) return drain_shard_data(client_fd, data_len);
    snprintf(spool_dir, sizeof(spool_dir), "%s/spool_%s",
             PS5UPLOAD2_SPOOL_DIR, entry->tx_id_hex);
    if (ensure_dir(spool_dir) != 0) {
        return drain_shard_data(client_fd, data_len);
    }
    snprintf(path, sizeof(path), "%s/%llu",
             spool_dir, (unsigned long long)shard_seq);
    /* Spool shard file is always a fresh write; data_len is known, so
     * preallocate helps reduce fragmentation when shards are large. */
    return runtime_write_shard_to_path(entry, path, 1, data_len,
                                        client_fd, data_len, out_digest);
}

/* ── Apply + cleanup ──────────────────────────────────────────────────────────── */

/*
 * mkdir -p for all ancestor directories of `path`.
 * Does not create `path` itself (assumes it is a file, not a dir).
 */
static int ensure_parent_dir(const char *path) {
    char tmp[512];
    char *p = NULL;
    size_t len = 0;
    if (!path || !*path) return -1;
    len = strlen(path);
    if (len >= sizeof(tmp)) return -1;
    memcpy(tmp, path, len + 1);
    /* Find the last slash to isolate the directory part. */
    p = tmp + len - 1;
    while (p > tmp && *p != '/') p--;
    if (p == tmp) return 0; /* no parent directory */
    *p = '\0'; /* truncate to parent path */
    /* Walk forward and mkdir each component. */
    for (p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = '\0';
            if (mkdir(tmp, 0777) != 0 && errno != EEXIST) return -1;
            *p = '/';
        }
    }
    if (mkdir(tmp, 0777) != 0 && errno != EEXIST) return -1;
    return 0;
}


/*
 * ensure_parent_dir() but backed by a single-slot cache on the tx entry.
 * For N files in the same directory (common in game dirs — 10s of siblings
 * per dir), only the first pays the mkdir walk. Subsequent calls just
 * strncmp the cached dirname against the new path's dirname.
 */
static int ensure_parent_dir_cached(runtime_tx_entry_t *entry,
                                     const char *path) {
    const char *slash;
    size_t dir_len;
    if (!path || !*path) return -1;
    /* Locate the dirname boundary without building a temp buffer. */
    slash = path + strlen(path);
    while (slash > path && *(slash - 1) != '/') slash -= 1;
    /* slash now points one past the last '/', so dir = [path, slash). */
    if (slash == path) return 0; /* no parent */
    dir_len = (size_t)((slash - 1) - path);
    if (dir_len == 0) return 0; /* root */
    if (entry && dir_len < sizeof(entry->last_parent_dir)) {
        if (entry->last_parent_dir[0] &&
            strlen(entry->last_parent_dir) == dir_len &&
            memcmp(entry->last_parent_dir, path, dir_len) == 0) {
            return 0; /* hit */
        }
    }
    if (ensure_parent_dir(path) != 0) return -1;
    if (entry && dir_len < sizeof(entry->last_parent_dir)) {
        memcpy(entry->last_parent_dir, path, dir_len);
        entry->last_parent_dir[dir_len] = '\0';
    }
    return 0;
}

/*
 * Manifest file entry parsed from the "files" JSON array.
 */
struct manifest_file_entry {
    char path[512];
    uint64_t shard_start; /* 1-based, first shard for this file */
    uint64_t shard_count; /* number of shards carrying this file */
};
typedef struct manifest_file_entry manifest_file_entry_t;

/*
 * Compact, parsed-once representation of a manifest file entry.
 * `path_offset` / `path_len` point into the owning entry's `manifest_blob`
 * heap buffer, so the full manifest JSON must outlive this index.
 * 32 bytes per entry — 223k files → 7.1 MiB of index on PS5 heap.
 */
struct manifest_index_entry {
    uint64_t shard_start;   /* 1-based, first shard carrying this file */
    uint32_t shard_count;   /* >= 1 */
    uint32_t path_offset;   /* offset into manifest_blob */
    uint32_t path_len;      /* length of the path in bytes */
    uint32_t _reserved;
    uint64_t size;          /* file size in bytes (informational) */
};

/*
 * Parse "files":[ … ] once from `blob` into a heap-allocated index sorted by
 * shard_start. Writes `*out_index` / `*out_count` on success. On any failure
 * (no files array, bad JSON, entry count > limit, OOM) frees partial work
 * and returns -1.
 *
 * `blob` is consumed by offset (not copied) — the caller must keep it alive
 * as long as the returned index is used.
 */
static int build_manifest_index(const char *blob, size_t blob_len,
                                uint64_t expected_count,
                                manifest_index_entry_t **out_index,
                                uint64_t *out_count) {
    const char *p = NULL;
    const char *arr_start = NULL;
    manifest_index_entry_t *idx = NULL;
    uint64_t cap = 0;
    uint64_t n = 0;

    if (!blob || !out_index || !out_count) return -1;
    *out_index = NULL;
    *out_count = 0;
    if (expected_count == 0 || expected_count > PS5UPLOAD2_MAX_MANIFEST_FILES) {
        return -1;
    }

    p = strstr(blob, "\"files\":");
    if (!p) return -1;
    p = strchr(p, '[');
    if (!p) return -1;
    arr_start = p + 1;

    /* Allocate to exact expected count so we never realloc mid-parse. If the
     * JSON has fewer entries than expected, we shrink `n` at the end; more
     * entries than expected is a malformed manifest. */
    cap = expected_count;
    idx = (manifest_index_entry_t *)calloc((size_t)cap, sizeof(*idx));
    if (!idx) return -1;

    p = arr_start;
    while (n < cap) {
        const char *obj_start;
        const char *obj_end;
        uint64_t s_start;
        uint64_t s_count;
        const char *path_key;
        const char *path_value_start;
        const char *path_value_end;
        size_t plen;

        obj_start = strchr(p, '{');
        if (!obj_start) break;
        obj_end = strchr(obj_start, '}');
        if (!obj_end) { free(idx); return -1; }

        /* Extract shard_start / shard_count from within this object only. We
         * temporarily NUL-terminate at obj_end to scope the helper's strstr. */
        {
            /* Safe: we don't mutate the caller's blob. We only read within
             * [obj_start, obj_end]. extract_* helpers take a const char * and
             * use strstr which stops at the first match — as long as the next
             * object's fields don't appear before this object's closing brace,
             * we're fine. The writer emits one object per file so fields are
             * well-separated. */
            s_start = extract_json_uint64_field(obj_start, "shard_start");
            s_count = extract_json_uint64_field(obj_start, "shard_count");
            if (s_count == 0) s_count = 1;
        }
        if (s_start == 0) { free(idx); return -1; }

        /* Locate "path":"..." inside this object. We find the opening quote
         * after the key, then the closing quote. The path must not contain
         * escaped quotes — the writer never emits those. */
        path_key = strstr(obj_start, "\"path\":\"");
        if (!path_key || path_key >= obj_end) { free(idx); return -1; }
        path_value_start = path_key + strlen("\"path\":\"");
        path_value_end = path_value_start;
        while (path_value_end < obj_end && *path_value_end != '"') {
            path_value_end += 1;
        }
        if (path_value_end >= obj_end) { free(idx); return -1; }
        plen = (size_t)(path_value_end - path_value_start);
        if (plen == 0 || plen >= 512) { free(idx); return -1; }

        idx[n].shard_start = s_start;
        idx[n].shard_count = (uint32_t)s_count;
        idx[n].path_offset = (uint32_t)(path_value_start - blob);
        idx[n].path_len    = (uint32_t)plen;
        idx[n].size        = extract_json_uint64_field(obj_start, "size");
        n += 1;

        /* Ordering tolerance: we require only non-decreasing shard_start.
         * Packed shards deliberately share a shard_start across many file
         * entries (all records live in the same STREAM_SHARD frame), so
         * strict inequality would reject valid manifests. Non-packed shards
         * whose ranges genuinely overlap would be a host bug — but packed
         * shards bypass the index entirely in handle_stream_shard, so a
         * misrouted lookup there is harmless. */
        if (n > 1 && idx[n - 1].shard_start < idx[n - 2].shard_start) {
            free(idx);
            return -1;
        }

        p = obj_end + 1;
        /* Allow blob_len to bound the scan (defence-in-depth vs. missing NUL). */
        if (blob_len > 0 && (size_t)(p - blob) >= blob_len) break;
    }

    if (n == 0) { free(idx); return -1; }

    *out_index = idx;
    *out_count = n;
    return 0;
}

/*
 * Binary-search the sorted index for the file entry that owns `shard_seq`.
 * Returns 0 and fills `out` (including copying the path out of `blob` into
 * `out->path`) on success; -1 if no entry covers the shard.
 */
static int lookup_manifest_index(const manifest_index_entry_t *idx,
                                 uint64_t count,
                                 const char *blob,
                                 uint64_t shard_seq,
                                 manifest_file_entry_t *out) {
    uint64_t lo = 0;
    uint64_t hi;
    if (!idx || count == 0 || !blob || !out) return -1;
    hi = count;
    while (lo < hi) {
        uint64_t mid = lo + (hi - lo) / 2;
        uint64_t s  = idx[mid].shard_start;
        uint64_t ec = idx[mid].shard_count > 0 ? idx[mid].shard_count : 1;
        if (shard_seq < s) {
            hi = mid;
        } else if (shard_seq >= s + ec) {
            lo = mid + 1;
        } else {
            size_t plen = idx[mid].path_len;
            if (plen >= sizeof(out->path)) return -1;
            memset(out, 0, sizeof(*out));
            memcpy(out->path, blob + idx[mid].path_offset, plen);
            out->path[plen] = '\0';
            out->shard_start = s;
            out->shard_count = ec;
            return 0;
        }
    }
    return -1;
}

/*
 * Free the heap-owned manifest state on a tx entry. Safe to call repeatedly
 * and on entries that never had a manifest (everything is NULL-checked).
 * Called on every terminal transition and on eviction of a terminal slot.
 */
static void runtime_release_tx_resources(runtime_tx_entry_t *entry) {
    if (!entry) return;
    /* Persistent direct-write cleanup: discards any in-flight writer +
     * closes the fd if still open. This is called from every terminal
     * transition (COMMIT after rename, ABORT, takeover/shutdown fan-out,
     * abort_tx_fatal). COMMIT's success path has already renamed the tmp
     * to dest_root before we get here, so unlink below is a no-op there.
     * For every other path, the tmp is stale and must be removed so
     * `.ps5up2-tmp` files do not survive a terminal transition. */
    direct_writer_discard(entry);
    /* Packed-shard worker pool teardown: drain + shutdown + join; fold
     * per-worker timings into the entry so the next COMMIT_TX_ACK can
     * report them. Must happen BEFORE the tmp unlink below so in-flight
     * worker writes cannot race with our cleanup. */
    pack_pool_teardown(entry);
    if (entry->direct_mode && entry->tmp_path[0]) {
        (void)unlink(entry->tmp_path);
    }
    if (entry->manifest_index) {
        free(entry->manifest_index);
        entry->manifest_index = NULL;
    }
    entry->manifest_index_count = 0;
    if (entry->manifest_blob) {
        free(entry->manifest_blob);
        entry->manifest_blob = NULL;
    }
    entry->manifest_blob_len = 0;
    if (entry->shard_log_fp) {
        fclose((FILE *)entry->shard_log_fp);
        entry->shard_log_fp = NULL;
    }
    entry->last_parent_dir[0] = '\0';
}

/*
 * Ephemeral-release: clear only thread-bound / heap state that the next
 * BEGIN_TX (resume) cannot rebuild from the journal. Specifically:
 *   - drain + destroy the persistent direct writer
 *   - drain + destroy the packed-shard worker pool (folds timings back in)
 *   - close the shard-log fd
 * Preserves:
 *   - entry->tmp_path on disk (holds partial shard data)
 *   - entry->manifest_blob / manifest_index (rebuilding on resume is
 *     wasted work at best and an OOM risk at worst on constrained heaps)
 *   - shards_received / bytes_received / dest_root (resume needs these)
 *
 * Used by the interrupt paths (connection drop, takeover, shutdown). Matches
 * the pattern used by `runtime_release_tx_resources` minus the tmp unlink
 * and heap free. */
static void runtime_release_tx_resources_ephemeral(runtime_tx_entry_t *entry) {
    if (!entry) return;
    direct_writer_discard(entry);
    pack_pool_teardown(entry);
    if (entry->shard_log_fp) {
        fclose((FILE *)entry->shard_log_fp);
        entry->shard_log_fp = NULL;
    }
    entry->last_parent_dir[0] = '\0';
}

/*
 * Mark a tx as fatally aborted from a mid-lifecycle error path.
 * Decrements active_transactions, sets state to "aborted", flushes the JSON
 * record, releases manifest heap state, saves summary. Balances the
 * `active_transactions += 1` that BEGIN_TX did up front so a run of broken
 * requests doesn't leak the active counter.
 *
 * Safe to call on an entry that may or may not already have manifest state.
 */
static void runtime_abort_tx_fatal(runtime_state_t *state,
                                    runtime_tx_entry_t *entry) {
    if (!state || !entry) return;
    pthread_mutex_lock(&state->state_mtx);
    if (state->active_transactions > 0) state->active_transactions -= 1;
    pthread_mutex_unlock(&state->state_mtx);
    snprintf(entry->state, sizeof(entry->state), "aborted");
    (void)runtime_flush_tx_record(state, entry);
    runtime_release_tx_resources(entry);
    (void)runtime_save_tx_state(state);
}

/* (manifest_find_file_for_shard removed — superseded by build_manifest_index +
 * lookup_manifest_index, which are O(log N) per shard instead of O(N).) */

/*
 * Parse the Nth (0-based) file entry from a JSON "files" array.
 * Extracts "path", "shard_start", and "shard_count".
 * Returns 0 on success, -1 if the Nth entry is not found.
 */
static int manifest_get_nth_file_path(const char *json, uint64_t n,
                                       manifest_file_entry_t *out) {
    const char *p = json;
    uint64_t idx = 0;
    /* Find "files":[ then scan for the Nth object. */
    p = strstr(p, "\"files\":");
    if (!p) return -1;
    p = strchr(p, '[');
    if (!p) return -1;
    p++;
    for (idx = 0; ; idx++) {
        const char *obj_start = strchr(p, '{');
        if (!obj_start) return -1;
        p = obj_start + 1;
        if (idx == n) {
            memset(out, 0, sizeof(*out));
            extract_json_string_field(obj_start, "path", out->path, sizeof(out->path));
            out->shard_start = extract_json_uint64_field(obj_start, "shard_start");
            out->shard_count = extract_json_uint64_field(obj_start, "shard_count");
            /* shard_count defaults to 1 if absent (old format compat). */
            if (out->shard_count == 0) out->shard_count = 1;
            return out->path[0] ? 0 : -1;
        }
        /* Skip to the closing brace of this object. */
        p = strchr(p, '}');
        if (!p) return -1;
        p++;
    }
}

/*
 * Read the persisted manifest JSON for a transaction.
 * Returns 0 and fills buf on success; -1 if the file is missing or unreadable.
 */
static int runtime_read_manifest(const runtime_tx_entry_t *entry,
                                  char *buf, size_t buf_len) {
    char path[512];
    FILE *fp = NULL;
    size_t got = 0;
    if (!entry || !buf || buf_len == 0) return -1;
    snprintf(path, sizeof(path), "%s/manifest_%s.json",
             PS5UPLOAD2_TX_DIR, entry->tx_id_hex);
    fp = fopen(path, "r");
    if (!fp) return -1;
    got = fread(buf, 1, buf_len - 1, fp);
    buf[got] = '\0';
    fclose(fp);
    return 0;
}

/*
 * Persist the manifest JSON to disk during BEGIN_TX so apply can use it.
 */
static int runtime_write_manifest(const runtime_tx_entry_t *entry,
                                   const char *manifest_json,
                                   size_t manifest_len) {
    char path[512];
    FILE *fp = NULL;
    if (!entry || !manifest_json || manifest_len == 0) return -1;
    snprintf(path, sizeof(path), "%s/manifest_%s.json",
             PS5UPLOAD2_TX_DIR, entry->tx_id_hex);
    fp = fopen(path, "w");
    if (!fp) return -1;
    fwrite(manifest_json, 1, manifest_len, fp);
    fclose(fp);
    return 0;
}

/*
 * After a successful COMMIT_TX: apply spooled shards to the filesystem.
 *
 * Single-file (file_count <= 1):
 *   Concatenate shards 1..shards_received into dest_root.
 *
 * Multi-file (file_count > 1):
 *   Read the persisted manifest; shard K maps to files[K-1].path.
 *   Each shard is a complete file (one shard per file).
 *
 * Parent directories are created as needed.
 */
static int runtime_apply_spool(const runtime_tx_entry_t *entry) {
    char spool_dir[512];
    char shard_path[512];
    char buf[65536];
    uint64_t seq = 0;

    if (!entry || entry->dest_root[0] == '\0') return -1;

    snprintf(spool_dir, sizeof(spool_dir), "%s/spool_%s",
             PS5UPLOAD2_SPOOL_DIR, entry->tx_id_hex);

    /* ── Multi-file path ── */
    if (entry->file_count > 1) {
        char manifest_buf[8192];
        uint64_t fi = 0;
        if (runtime_read_manifest(entry, manifest_buf, sizeof(manifest_buf)) != 0) {
            fprintf(stderr, "[payload2] could not read manifest for tx %s\n",
                    entry->tx_id_hex);
            return -1;
        }
        for (fi = 0; fi < entry->file_count; fi++) {
            manifest_file_entry_t mf;
            uint64_t s = 0;
            FILE *out = NULL;
            if (manifest_get_nth_file_path(manifest_buf, fi, &mf) != 0) {
                fprintf(stderr, "[payload2] manifest missing entry %llu\n",
                        (unsigned long long)fi);
                return -1;
            }
            if (ensure_parent_dir(mf.path) != 0) {
                fprintf(stderr, "[payload2] ensure_parent_dir failed: %s\n", mf.path);
                return -1;
            }
            out = fopen(mf.path, "wb");
            if (!out) {
                fprintf(stderr, "[payload2] open dest failed: %s errno=%d\n",
                        mf.path, errno);
                return -1;
            }
            for (s = 0; s < mf.shard_count; s++) {
                uint64_t this_seq = mf.shard_start + s;
                char ibuf[65536];
                FILE *in = NULL;
                size_t got = 0;
                snprintf(shard_path, sizeof(shard_path), "%s/%llu",
                         spool_dir, (unsigned long long)this_seq);
                in = fopen(shard_path, "rb");
                if (!in) {
                    fprintf(stderr, "[payload2] open shard failed: %s errno=%d\n",
                            shard_path, errno);
                    fclose(out);
                    return -1;
                }
                while ((got = fread(ibuf, 1, sizeof(ibuf), in)) > 0) {
                    if (fwrite(ibuf, 1, got, out) != got) {
                        fclose(in);
                        fclose(out);
                        return -1;
                    }
                }
                fclose(in);
            }
            fclose(out);
            printf("[payload2] applied %llu shards -> %s\n",
                   (unsigned long long)mf.shard_count, mf.path);
        }
        printf("[payload2] multi-file apply done: %llu files\n",
               (unsigned long long)entry->file_count);
        return 0;
    }

    /* ── Single-file path: concatenate all shards to dest_root ── */
    {
        FILE *out = NULL;
        if (ensure_parent_dir(entry->dest_root) != 0) {
            fprintf(stderr, "[payload2] ensure_parent_dir failed: %s\n", entry->dest_root);
            return -1;
        }
        out = fopen(entry->dest_root, "wb");
        if (!out) {
            fprintf(stderr, "[payload2] open dest_root failed: %s errno=%d\n",
                    entry->dest_root, errno);
            return -1;
        }
        for (seq = 1; seq <= entry->shards_received; seq++) {
            FILE *in = NULL;
            size_t got = 0;
            snprintf(shard_path, sizeof(shard_path), "%s/%llu",
                     spool_dir, (unsigned long long)seq);
            in = fopen(shard_path, "rb");
            if (!in) {
                fprintf(stderr, "[payload2] open shard failed: %s errno=%d\n",
                        shard_path, errno);
                fclose(out);
                return -1;
            }
            while ((got = fread(buf, 1, sizeof(buf), in)) > 0) {
                if (fwrite(buf, 1, got, out) != got) {
                    fclose(in);
                    fclose(out);
                    return -1;
                }
            }
            fclose(in);
        }
        fclose(out);
        printf("[payload2] apply done: %llu shards -> %s\n",
               (unsigned long long)entry->shards_received, entry->dest_root);
        return 0;
    }
}

/*
 * Remove all shard files in the spool directory for a transaction and
 * then rmdir the directory itself. Called after a successful apply.
 */
static int runtime_cleanup_spool(const runtime_tx_entry_t *entry) {
    char spool_dir[512];
    DIR *dir = NULL;
    struct dirent *de = NULL;

    if (!entry) return -1;
    snprintf(spool_dir, sizeof(spool_dir), "%s/spool_%s",
             PS5UPLOAD2_SPOOL_DIR, entry->tx_id_hex);

    dir = opendir(spool_dir);
    if (!dir) {
        if (errno == ENOENT) return 0;
        return -1;
    }
    while ((de = readdir(dir)) != NULL) {
        char path[512];
        if (de->d_name[0] == '.') continue;
        snprintf(path, sizeof(path), "%s/%s", spool_dir, de->d_name);
        (void)unlink(path);
    }
    closedir(dir);
    (void)rmdir(spool_dir);
    return 0;
}

/* Encode a SHARD_ACK into a 48-byte buffer (no struct padding assumptions). */
static void encode_shard_ack(unsigned char *out,
                              const unsigned char *tx_id,
                              uint64_t shard_seq,
                              uint8_t ack_state,
                              uint64_t bytes_committed,
                              uint64_t files_committed) {
    memset(out, 0, FTX2_SHARD_ACK_LEN);
    memcpy(out, tx_id, 16);
    write_le64(out + 16, shard_seq);
    out[24] = ack_state;
    /* out[25..31] = pad (zeroed) */
    write_le64(out + 32, bytes_committed);
    write_le64(out + 40, files_committed);
}

/*
 * Packed-shard handler.
 *
 * Body layout (set of `record_count` back-to-back records):
 *   [u32 path_len LE][u32 data_len LE][path bytes][data bytes]
 *
 * Each record is written straight to `<path>.ps5up2-tmp` so the COMMIT_TX
 * rename loop picks it up just like the single-file-per-shard path. No
 * manifest_index lookup is involved — packed shards are self-describing.
 *
 * BLAKE3 digest in ShardHeader covers the entire body (record prefixes +
 * paths + data in wire order). We stream-hash on every byte received.
 *
 * On any parse / write / digest error the transaction is aborted via
 * `runtime_abort_tx_fatal`. There is no per-record retry — packed writes
 * are append-atomic only within a record; a partial pack corrupts tmps.
 */
static int handle_packed_shard(runtime_state_t *state, int client_fd,
                                uint64_t trace_id,
                                runtime_tx_entry_t *entry,
                                const unsigned char *tx_id,
                                uint64_t shard_seq,
                                uint64_t body_len,
                                uint32_t record_count,
                                const unsigned char *expected_digest,
                                int want_verify) {
    blake3_hasher hasher;
    uint64_t remaining = body_len;
    uint32_t r;
    unsigned char ack_bytes[FTX2_SHARD_ACK_LEN];
    /* Lazily start the per-tx worker pool on first packed shard. If the
     * start fails (OOM, kernel thread cap), fall back to serial inline
     * processing — functionally identical to pre-pool behaviour. */
    int pool_active = 0;
    if (entry && !entry->pack_pool) {
        if (pack_pool_start(entry) == 0) {
            pool_active = 1;
        } else {
            fprintf(stderr,
                    "[payload2] pack: worker pool start failed — serial fallback\n");
        }
    } else if (entry && entry->pack_pool) {
        pool_active = 1;
    }

    if (want_verify) blake3_hasher_init(&hasher);

    for (r = 0; r < record_count; r++) {
        unsigned char prefix[FTX2_PACKED_RECORD_PREFIX_LEN];
        uint32_t path_len;
        uint32_t rec_data_len;
        char path[512];

        if (remaining < FTX2_PACKED_RECORD_PREFIX_LEN) {
            if (remaining > 0) (void)drain_shard_data(client_fd, remaining);
            runtime_abort_tx_fatal(state, entry);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "packed_truncated", 16);
        }
        if (recv_exact(client_fd, prefix, FTX2_PACKED_RECORD_PREFIX_LEN) != 0) return -1;
        if (want_verify) {
            blake3_hasher_update(&hasher, prefix, FTX2_PACKED_RECORD_PREFIX_LEN);
        }
        remaining -= FTX2_PACKED_RECORD_PREFIX_LEN;

        path_len     = read_le32(prefix);
        rec_data_len = read_le32(prefix + 4);

        if (path_len == 0 || path_len >= sizeof(path) ||
            (uint64_t)path_len + (uint64_t)rec_data_len > remaining) {
            if (remaining > 0) (void)drain_shard_data(client_fd, remaining);
            runtime_abort_tx_fatal(state, entry);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "packed_bad_record", 17);
        }

        if (recv_exact(client_fd, path, path_len) != 0) return -1;
        if (want_verify) blake3_hasher_update(&hasher, (unsigned char *)path, path_len);
        path[path_len] = '\0';
        remaining -= path_len;

        /* Parent-dir cache is reader-thread-only; this stays correct whether
         * the pool is active or not. */
        (void)ensure_parent_dir_cached(entry, path);

        if (pool_active) {
            /* Pool path: reader receives data into a heap buffer, pushes to
             * workers, hashes as it reads. Workers do open+write+close.
             *
             * Allocation pattern:
             *   path_buf  — malloc'd strdup, freed by worker
             *   data_buf  — malloc'd, may be NULL for 0-byte records,
             *               freed by worker
             */
            char          *path_buf = NULL;
            unsigned char *data_buf = NULL;

            path_buf = (char *)malloc((size_t)path_len + 1);
            if (!path_buf) {
                fprintf(stderr, "[payload2] pack: path alloc failed\n");
                if (remaining > 0) (void)drain_shard_data(client_fd, remaining);
                runtime_abort_tx_fatal(state, entry);
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                  "out_of_memory", 13);
            }
            memcpy(path_buf, path, (size_t)path_len + 1);

            if (rec_data_len > 0) {
                data_buf = (unsigned char *)malloc(rec_data_len);
                if (!data_buf) {
                    free(path_buf);
                    fprintf(stderr, "[payload2] pack: data alloc %u failed\n", rec_data_len);
                    if (remaining > 0) (void)drain_shard_data(client_fd, remaining);
                    runtime_abort_tx_fatal(state, entry);
                    return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                      "out_of_memory", 13);
                }
                if (recv_exact(client_fd, data_buf, rec_data_len) != 0) {
                    free(path_buf);
                    free(data_buf);
                    return -1;
                }
                if (want_verify) blake3_hasher_update(&hasher, data_buf, rec_data_len);
            }

            if (pack_pool_push((pack_worker_pool_t *)entry->pack_pool,
                               path_buf, data_buf, rec_data_len) != 0) {
                /* Sticky worker error — push returned -1 without taking
                 * ownership, so we own the frees. Drain remaining bytes
                 * and bail. */
                free(path_buf);
                if (data_buf) free(data_buf);
                if (remaining > (uint64_t)rec_data_len) {
                    (void)drain_shard_data(client_fd, remaining - (uint64_t)rec_data_len);
                }
                runtime_abort_tx_fatal(state, entry);
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                  "pack_worker_io_error", 20);
            }
            remaining -= rec_data_len;
            if (entry) entry->bytes_received += rec_data_len;
        } else {
            /* Serial fallback path: unchanged semantics from pre-pool code. */
            char tmp_path[512 + 16];
            int fd;
            snprintf(tmp_path, sizeof(tmp_path), "%s.ps5up2-tmp", path);
            (void)unlink(tmp_path);

            fd = open(tmp_path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
            if (fd < 0) {
                (void)drain_shard_data(client_fd, (uint64_t)rec_data_len + remaining - (uint64_t)rec_data_len);
                remaining = 0;
                runtime_abort_tx_fatal(state, entry);
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                  "packed_open_failed", 18);
            }
            if (rec_data_len > 0) {
                (void)ftruncate(fd, (off_t)rec_data_len);
            }
            {
                uint64_t rem = rec_data_len;
                unsigned char buf[FTX2_SHARD_DRAIN_BUF];
                while (rem > 0) {
                    size_t take = rem > sizeof(buf) ? sizeof(buf) : (size_t)rem;
                    if (recv_exact(client_fd, buf, take) != 0) { close(fd); return -1; }
                    if (want_verify) blake3_hasher_update(&hasher, buf, take);
                    if (write_full(fd, buf, take) != 0) { close(fd); return -1; }
                    rem -= take;
                }
            }
            close(fd);
            remaining -= rec_data_len;
            if (entry) entry->bytes_received += rec_data_len;
        }
    }

    if (remaining > 0) {
        (void)drain_shard_data(client_fd, remaining);
        if (pool_active) (void)pack_pool_drain((pack_worker_pool_t *)entry->pack_pool);
        runtime_abort_tx_fatal(state, entry);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "packed_trailing_bytes", 21);
    }

    /* Drain the pool before ACK so SHARD_ACK means "persisted", preserving
     * the contract the serial path always had. */
    if (pool_active && pack_pool_drain((pack_worker_pool_t *)entry->pack_pool) != 0) {
        runtime_abort_tx_fatal(state, entry);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "pack_worker_io_error", 20);
    }

    if (want_verify) {
        unsigned char computed[BLAKE3_OUT_LEN];
        blake3_hasher_finalize(&hasher, computed, BLAKE3_OUT_LEN);
        if (memcmp(computed, expected_digest, BLAKE3_OUT_LEN) != 0) {
            runtime_abort_tx_fatal(state, entry);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "direct_tx_corrupt", 17);
        }
    }

    if (entry) {
        entry->shards_received += 1;
        runtime_append_shard_log(entry, shard_seq, body_len);
        /* Per-ACK journal flush enables shard-level resume (TX_FLAG_RESUME).
         * Without this, shards_received only hits disk at commit/abort, so
         * a network drop mid-transfer loses the counter and the reconnecting
         * client has no way to know what the payload already received. Cost
         * is one fopen+fprintf+fclose per shard ACK (~0.5 ms on PS5 UFS at
         * 32 MB shard size = <0.2% of shard-send time). */
        (void)runtime_flush_tx_record(state, entry);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    encode_shard_ack(ack_bytes, tx_id, shard_seq, FTX2_ACK_STATE_SPOOLED,
                     entry ? entry->bytes_received : 0, 0);
    return send_frame(client_fd, FTX2_FRAME_SHARD_ACK, 0, trace_id,
                      ack_bytes, FTX2_SHARD_ACK_LEN);
}

/*
 * Handle a STREAM_SHARD frame.
 *
 * Called BEFORE the main body-read path because shard bodies can be
 * very large (up to 32 MiB). We read the 64-byte shard header directly,
 * then drain the payload in chunks.
 *
 * Stub behaviour:
 *   - parse shard header
 *   - look up the transaction in the in-memory table
 *   - update per-tx shard stats
 *   - append to per-tx shard log
 *   - drain shard data (not yet written to storage)
 *   - reply with binary SHARD_ACK (spooled state)
 */
static int handle_stream_shard(runtime_state_t *state, int client_fd,
                                uint64_t trace_id, uint64_t body_len) {
    unsigned char shard_hdr_bytes[FTX2_SHARD_HEADER_LEN];
    unsigned char ack_bytes[FTX2_SHARD_ACK_LEN];
    uint64_t shard_seq = 0;
    uint64_t data_len = 0;
    unsigned char tx_id[16];
    runtime_tx_entry_t *entry = NULL;

    if (!state) return -1;
    if (body_len < FTX2_SHARD_HEADER_LEN) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "shard_header_too_short", 22);
    }
    if (recv_exact(client_fd, shard_hdr_bytes, FTX2_SHARD_HEADER_LEN) != 0) return -1;

    memcpy(tx_id, shard_hdr_bytes, 16);
    shard_seq = read_le64(shard_hdr_bytes + 16);
    /* shard_digest[24..56], record_count[56], flags[60] */
    {
        uint32_t record_count_hdr = read_le32(shard_hdr_bytes + 56);
        uint32_t flags_hdr        = read_le32(shard_hdr_bytes + 60);
        /* Look up the transaction before consuming data so we can route. */
        entry = runtime_find_tx_entry(state, tx_id);
        data_len = body_len - FTX2_SHARD_HEADER_LEN;

        if (flags_hdr & FTX2_SHARD_FLAG_PACKED) {
            int want_verify = 0;
            int i;
            for (i = 0; i < BLAKE3_OUT_LEN; i++) {
                if (shard_hdr_bytes[24 + i] != 0) { want_verify = 1; break; }
            }
            if (!entry || !entry->direct_mode || entry->file_count <= 1 ||
                record_count_hdr == 0) {
                /* Packed shards are only meaningful for an active multi-file
                 * direct-mode tx with ≥1 records. Reject cleanly. */
                (void)drain_shard_data(client_fd, data_len);
                if (entry) runtime_abort_tx_fatal(state, entry);
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                  "packed_unsupported", 18);
            }
            return handle_packed_shard(state, client_fd, trace_id,
                                       entry, tx_id, shard_seq,
                                       data_len, record_count_hdr,
                                       shard_hdr_bytes + 24, want_verify);
        }
    }
    {
        unsigned char computed[BLAKE3_OUT_LEN];
        int want_verify = 0;
        int i = 0;
        /* Only hash if the sender provided a non-zero expected digest. */
        for (i = 0; i < BLAKE3_OUT_LEN; i++) {
            if (shard_hdr_bytes[24 + i] != 0) { want_verify = 1; break; }
        }
        if (data_len > 0) {
            int write_ok;
            if (entry && entry->direct_mode && entry->file_count <= 1) {
                /* Single-file direct: stream into <dest>.ps5up2-tmp via a
                 * persistent fd and (for large txs) a persistent writer
                 * thread. First shard opens + ftruncates, COMMIT_TX closes +
                 * renames, and any terminal transition (ABORT / takeover /
                 * shutdown / fatal) unlinks the tmp through
                 * runtime_release_tx_resources. */
                int is_first = (shard_seq == 1);
                uint64_t prealloc = (is_first && entry->total_bytes > 0)
                                      ? entry->total_bytes : 0;
                write_ok = runtime_write_shard_persistent(entry, client_fd,
                                                          data_len,
                                                          is_first, prealloc,
                                                          want_verify ? computed : NULL);
            } else if (entry && entry->direct_mode && entry->file_count > 1) {
                /* Multi-file direct: route this shard to the owning file's
                 * tmp via the in-memory manifest index built at BEGIN_TX.
                 * If the index is missing (e.g. a recovered-after-takeover
                 * tx whose client is gone), or routing fails, the shard
                 * is unroutable — we must NOT fall through to digest
                 * comparison with an uninitialised `computed` buffer. */
                manifest_file_entry_t mf;
                char tmp_path[sizeof(mf.path) + 16];
                if (!entry->manifest_index ||
                    lookup_manifest_index(
                        (const manifest_index_entry_t *)entry->manifest_index,
                        entry->manifest_index_count,
                        entry->manifest_blob,
                        shard_seq, &mf) != 0) {
                    fprintf(stderr, "[payload2] direct multi: no manifest file owns shard %llu\n",
                            (unsigned long long)shard_seq);
                    (void)drain_shard_data(client_fd, data_len);
                    runtime_abort_tx_fatal(state, entry);
                    return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                      "manifest_shard_not_owned", 24);
                }
                snprintf(tmp_path, sizeof(tmp_path), "%s.ps5up2-tmp", mf.path);
                if (shard_seq == mf.shard_start) {
                    if (ensure_parent_dir_cached(entry, mf.path) != 0) {
                        fprintf(stderr, "[payload2] direct multi: parent dir failed %s\n",
                                mf.path);
                    }
                    (void)unlink(tmp_path);
                }
                /* First shard of this file truncates + preallocates;
                 * subsequent shards append. shard_count==1 (the common
                 * directory case) means the entire file is this shard. */
                {
                    int is_first = (shard_seq == mf.shard_start);
                    uint64_t prealloc = 0;
                    if (is_first && mf.shard_count == 1) {
                        prealloc = data_len;
                    }
                    write_ok = runtime_write_shard_to_path(entry, tmp_path,
                                                           is_first, prealloc,
                                                           client_fd, data_len,
                                                           want_verify ? computed : NULL);
                }
            } else if (entry) {
                write_ok = runtime_write_shard_data(entry, shard_seq, client_fd,
                                                    data_len,
                                                    want_verify ? computed : NULL);
            } else {
                write_ok = drain_shard_data(client_fd, data_len);
            }
            if (write_ok != 0) return -1;
        } else if (want_verify) {
            /* Zero-length shard: digest is BLAKE3 of empty input. */
            blake3_hasher h;
            blake3_hasher_init(&h);
            blake3_hasher_finalize(&h, computed, BLAKE3_OUT_LEN);
        }
        /* Streaming verify — digest was computed while writing. */
        if (entry && want_verify &&
            memcmp(computed, shard_hdr_bytes + 24, BLAKE3_OUT_LEN) != 0) {
            if (entry->direct_mode) {
                /* Direct-write cannot surgically undo an already-appended
                 * suffix in the tmp file, so a mid-transfer digest mismatch
                 * is fatal for the whole transaction. Drop the tmp, mark the
                 * entry aborted, and return a distinct error so the host
                 * treats this as a hard failure (not a per-shard retry). */
                (void)unlink(entry->tmp_path);
                runtime_abort_tx_fatal(state, entry);
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                                  "direct_tx_corrupt", 17);
            }
            {
                char bad_path[512];
                snprintf(bad_path, sizeof(bad_path), "%s/spool_%s/%llu",
                         PS5UPLOAD2_SPOOL_DIR, entry->tx_id_hex,
                         (unsigned long long)shard_seq);
                (void)unlink(bad_path);
            }
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "shard_digest_mismatch", 21);
        }
    }

    /* Update per-tx stats. */
    if (entry) {
        entry->shards_received += 1;
        entry->bytes_received  += data_len;
        runtime_append_shard_log(entry, shard_seq, data_len);
        /* Per-ACK journal flush enables shard-level resume — see the matching
         * call in handle_packed_stream_shard for rationale. */
        (void)runtime_flush_tx_record(state, entry);
    }

    /* Per-shard append to the tx event log removed: it costs ~0.5–1 ms/shard
     * in open/write/close and offered little diagnostic value beyond the
     * per-tx shard log (which is still written). begin/commit/abort/shutdown
     * events remain in the event log. */
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    encode_shard_ack(ack_bytes,
                     tx_id,
                     shard_seq,
                     FTX2_ACK_STATE_SPOOLED,
                     entry ? entry->bytes_received : 0,
                     0);
    return send_frame(client_fd, FTX2_FRAME_SHARD_ACK, 0, trace_id,
                      ack_bytes, FTX2_SHARD_ACK_LEN);
}

/* ── CLEANUP handler ──────────────────────────────────────────────────────────
 *
 * Exposes a narrow `rm -rf <path>` primitive to the host so bench/smoke
 * sweeps can reset PS5 state between profiles without waiting for a reboot.
 *
 * Safety:
 *   - Path must start with one of the allowlisted prefixes below. Anything
 *     else — `/data`, `/system`, `/`, empty, or paths containing `..` — is
 *     refused outright. This is not a general-purpose delete RPC.
 *   - Recursive delete stops on any removal error rather than pushing on
 *     through; the ACK reports how many files/dirs were removed.
 */

/* Unified test sandbox: everything the bench / smoke / sweep harnesses
 * write lives under `<root>/ps5upload/tests/…`, where `<root>` is one of:
 *
 *   /data                      built-in storage
 *   /mnt/ext[0-9]+             M.2 expansion slot (when mounted)
 *   /mnt/usb[0-9]+             USB storage slot (when mounted)
 *
 * Consolidating everything under this shape lets the user wipe the entire
 * test footprint on a drive with a single cleanup call — e.g.
 *   POST /api/ps5/cleanup {"path":"/data/ps5upload/tests"}
 *
 * Safety: the path is matched against the allowed shapes by the
 * `cleanup_path_allowed` helper below; literally everything else is
 * refused, so `/data/ps5upload/runtime`, `/data/ps5upload/tx`, and any
 * non-tests area stay off-limits.
 */
static int path_has_test_suffix(const char *p) {
    /* Matches ".../ps5upload/tests" optionally followed by '/<sub>'. */
    const char *suffix = "/ps5upload/tests";
    size_t slen = strlen(suffix);
    if (strncmp(p, suffix, slen) != 0) return 0;
    return (p[slen] == '\0' || p[slen] == '/');
}

static int cleanup_path_allowed(const char *path) {
    if (!path || !*path) return 0;
    /* Defence-in-depth: reject anything with '..' anywhere. */
    if (strstr(path, "..") != NULL) return 0;

    /* Case A: /data/ps5upload/tests[/...] */
    {
        const char *prefix = "/data";
        size_t plen = strlen(prefix);
        if (strncmp(path, prefix, plen) == 0 && path_has_test_suffix(path + plen)) {
            return 1;
        }
    }

    /* Case B: /mnt/{ext,usb}<digits>/ps5upload/tests[/...] */
    if (strncmp(path, "/mnt/", 5) == 0) {
        const char *p = path + 5;
        int is_ext = (strncmp(p, "ext", 3) == 0);
        int is_usb = (strncmp(p, "usb", 3) == 0);
        if (is_ext || is_usb) {
            p += 3;
            /* One or more digits after ext/usb. */
            if (*p < '0' || *p > '9') return 0;
            while (*p >= '0' && *p <= '9') p++;
            if (path_has_test_suffix(p)) return 1;
        }
    }

    return 0;
}

/* Recursive removal of a single path. Counts files + dirs actually removed
 * so the ACK can report work done. Returns 0 on success, -1 on error. A
 * missing path (ENOENT) counts as success with zero removals. */
static int remove_recursive_path(const char *path,
                                  uint64_t *removed_files,
                                  uint64_t *removed_dirs) {
    struct stat st;
    DIR *dir = NULL;
    struct dirent *ent = NULL;
    if (!path) return -1;
    if (lstat(path, &st) != 0) {
        if (errno == ENOENT) return 0;
        return -1;
    }
    if (!S_ISDIR(st.st_mode)) {
        if (unlink(path) != 0 && errno != ENOENT) return -1;
        if (removed_files) *removed_files += 1;
        return 0;
    }
    dir = opendir(path);
    if (!dir) return -1;
    while ((ent = readdir(dir)) != NULL) {
        char child[512];
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        if (snprintf(child, sizeof(child), "%s/%s", path, ent->d_name) >= (int)sizeof(child)) {
            closedir(dir);
            return -1; /* path too long — refuse to truncate and leak state */
        }
        if (remove_recursive_path(child, removed_files, removed_dirs) != 0) {
            closedir(dir);
            return -1;
        }
    }
    closedir(dir);
    if (rmdir(path) != 0 && errno != ENOENT) return -1;
    if (removed_dirs) *removed_dirs += 1;
    return 0;
}

static int handle_cleanup(runtime_state_t *state, int client_fd,
                           uint64_t trace_id,
                           const char *request_body, uint64_t body_len) {
    char path[512];
    char resp[256];
    uint64_t removed_files = 0;
    uint64_t removed_dirs = 0;
    int rc;
    int len;
    if (!state) return -1;
    (void)body_len;
    path[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "path", path, sizeof(path));
    }
    if (!path[0]) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "cleanup_missing_path", 20);
    }
    if (!cleanup_path_allowed(path)) {
        fprintf(stderr, "[payload2] cleanup: refusing disallowed path %s\n", path);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "cleanup_path_denied", 19);
    }
    rc = remove_recursive_path(path, &removed_files, &removed_dirs);
    if (rc != 0) {
        fprintf(stderr, "[payload2] cleanup: remove_recursive_path(%s) failed errno=%d\n",
                path, errno);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "cleanup_io_error", 16);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    len = snprintf(resp, sizeof(resp),
                   "{\"ok\":true,\"path\":\"%s\",\"removed_files\":%llu,\"removed_dirs\":%llu}",
                   path,
                   (unsigned long long)removed_files,
                   (unsigned long long)removed_dirs);
    if (len < 0) return -1;
    return send_frame(client_fd, FTX2_FRAME_CLEANUP_ACK, 0, trace_id,
                      resp, (uint64_t)len);
}

/* ── FS_LIST_VOLUMES handler ─────────────────────────────────────────────────
 *
 * Enumerates storage volumes using `getmntinfo(MNT_WAIT)`, the FreeBSD
 * primitive that returns every mounted filesystem in one syscall. The old
 * per-path lstat+statfs probe has been retired: it had to guess at paths
 * (it missed the PS5-specific `/mnt/ext1` layout until we listed `/mnt`),
 * and it reported placeholder tmpfs slots as if they were real drives.
 *
 * Pattern matches the canonical example at
 * `/opt/ps5-payload-sdk/samples/mntinfo/main.c`.
 *
 * Each returned volume carries:
 *   path            — mount-on name (e.g. `/mnt/ext1`)
 *   mount_from      — device source (`/dev/nvme1`, `/dev/ssd0.user`, ...)
 *                     or pseudo name for tmpfs/etc.
 *   fs_type         — `bfs`, `nullfs`, `ufs`, `tmpfs`, `devfs`, ...
 *   total_bytes     — f_blocks × f_bsize
 *   free_bytes      — f_bavail × f_bsize (non-root availability)
 *   writable        — true if MNT_RDONLY is unset
 *   is_placeholder  — true for tmpfs/pseudo mounts or volumes <256 MiB.
 *                     UI filters these out by default; advanced views can
 *                     show them to make hot-plug state visible.
 *
 * Response size: a typical PS5 has ~40 mount entries (system partitions +
 * process sandboxes). At ~240 bytes/entry that's ~10 KiB; we heap-allocate
 * a 16 KiB buffer to keep the dispatch thread's stack bounded.
 */

/* Best-effort PS5 kernel version probe. Reads `kern.version` via
 * sysctl(2) — returns a string like "FreeBSD 11.0-RELEASE-pN #M ..."
 * that PS5 firmware extends with a sys-revision tag. Not the
 * user-visible firmware number (e.g. "5.00") — the PS5 doesn't
 * expose that to user-space via a stable sysctl we've found. The
 * kernel string is still useful: users can cross-reference it
 * against psdevwiki build strings to identify their console's
 * firmware exactly.
 *
 * Writes into `dst` (null-terminated). On failure writes "unknown"
 * and returns. Swallows errors — STATUS should never fail because
 * a sysctl was unavailable. */
static void read_ps5_kernel_version(char *dst, size_t dst_cap) {
    int mib[2] = { CTL_KERN, KERN_VERSION };
    size_t len = dst_cap;
    if (!dst || dst_cap == 0) return;
    if (sysctl(mib, 2, dst, &len, NULL, 0) != 0 || len == 0) {
        snprintf(dst, dst_cap, "unknown");
        return;
    }
    /* sysctl returns the length INCLUDING the trailing NUL most of the
     * time, but not always — clamp to dst_cap-1 and force-terminate
     * so downstream JSON embedding stays sane. Also replace any stray
     * newlines with spaces, since some builds embed them. */
    if (len > dst_cap - 1) len = dst_cap - 1;
    dst[len] = '\0';
    for (size_t i = 0; i < len; i++) {
        if (dst[i] == '\n' || dst[i] == '\r') dst[i] = ' ';
    }
}

/* JSON-escape path/device names. PS5 mount names are ASCII but defend
 * anyway — tamper-resistant for anything we feed into a JSON context. */
static void json_escape_into(const char *src, char *dst, size_t dst_cap) {
    size_t ei = 0, ni = 0;
    if (!dst || dst_cap == 0) return;
    while (src && src[ni] && ei + 2 < dst_cap) {
        unsigned char c = (unsigned char)src[ni];
        if (c == '"' || c == '\\') {
            dst[ei++] = '\\';
            dst[ei++] = (char)c;
        } else if (c < 0x20) {
            dst[ei++] = '?';
        } else {
            dst[ei++] = (char)c;
        }
        ni++;
    }
    dst[ei] = '\0';
}

/* Returns non-zero iff the mount is one we want to surface to the client:
 * the three PS5 storage shapes the UI cares about, and only when a real
 * device is backing the mount (not a sandbox nullfs view or a pseudo fs).
 *
 *   /data                    — internal SSD user partition
 *   /mnt/ext[0-9]+           — M.2 expansion *or* USB extended storage
 *                              (PS5 reformats both to UFS; they share
 *                              the `ext` namespace — `mount_from` is the
 *                              only way to tell them apart: `/dev/nvme*`
 *                              = M.2, `/dev/da*` = USB extended)
 *   /mnt/usb[0-9]+           — plain USB stick (exFAT / FAT32)
 *
 * Everything else — /user (nullfs alias of /data), /system*, /mnt/sandbox,
 * /mnt/pfs, /preinst, tmpfs, etc. — is hidden. We also drop entries whose
 * `mount_from` isn't under `/dev/` so an unmounted slot or a sandbox nullfs
 * with a /mnt/ext-shaped path can't sneak through. */
static int is_user_storage_path(const char *path) {
    if (!path) return 0;
    if (strcmp(path, "/data") == 0) return 1;
    if (strncmp(path, "/mnt/ext", 8) == 0) {
        /* guard against /mnt/externalsomething */
        return (path[8] >= '0' && path[8] <= '9') ? 1 : 0;
    }
    if (strncmp(path, "/mnt/usb", 8) == 0) {
        return (path[8] >= '0' && path[8] <= '9') ? 1 : 0;
    }
    /* Surface /mnt/ps5upload/<name>/ mounts — that's where FS_MOUNT
     * puts disk images, and users need to see them in the Volumes
     * list so they can trigger FS_UNMOUNT from the UI. The base
     * directory itself (/mnt/ps5upload without a child) is skipped. */
    if (strncmp(path, "/mnt/ps5upload/", 15) == 0 && path[15] != '\0') {
        return 1;
    }
    return 0;
}

static int handle_fs_list_volumes(runtime_state_t *state, int client_fd,
                                   uint64_t trace_id) {
    const size_t RESP_CAP = 16u * 1024u;
    char *resp = NULL;
    size_t off = 0;
    struct statfs *mnts = NULL;
    int nmnts;
    int first_volume = 1;
    int i;
    int n;
    int rc;

    if (!state) return -1;

    /* Man page: buf is libc-owned; do NOT free. Each subsequent call
     * clobbers the previous buffer.
     *
     * MNT_NOWAIT (not MNT_WAIT) so we use the kernel's cached mount
     * table instead of forcing a fresh statfs on every mount. A fresh
     * statfs pass can block indefinitely if any mount is in an error
     * state (flaky USB, dead NAS, a .exfat image whose backing file
     * became unreachable). The cache is refreshed by the kernel on
     * actual mount/unmount events so it's always reasonably current
     * for a listing query -- and if it's slightly stale, the Volumes
     * tab just shows one spurious entry that disappears on the next
     * refresh, which is much better than a wedged mgmt thread. The
     * FS_UNMOUNT handler already uses MNT_NOWAIT for the same reason. */
    nmnts = getmntinfo(&mnts, MNT_NOWAIT);
    if (nmnts < 0 || mnts == NULL) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_list_volumes_getmntinfo_failed", 33);
    }

    resp = (char *)malloc(RESP_CAP);
    if (!resp) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_list_volumes_oom", 19);
    }

    n = snprintf(resp + off, RESP_CAP - off, "{\"volumes\":[");
    if (n < 0 || (size_t)n >= RESP_CAP - off) { free(resp); return -1; }
    off += (size_t)n;

    for (i = 0; i < nmnts; i++) {
        /* f_mntonname and f_mntfromname are fixed-size char arrays
         * inside struct statfs -- never NULL, but may be empty. Alias
         * them to local pointers so the filter conditions below read
         * cleanly without repeated mnts[i].* indexing. */
        const char *mnt_on   = mnts[i].f_mntonname;
        const char *mnt_from = mnts[i].f_mntfromname;
        uint64_t total = (uint64_t)mnts[i].f_blocks * (uint64_t)mnts[i].f_bsize;
        uint64_t avail = (uint64_t)mnts[i].f_bavail * (uint64_t)mnts[i].f_bsize;
        int writable   = (mnts[i].f_flags & MNT_RDONLY) ? 0 : 1;

        /* Path-prefix allowlist — drops everything outside /data, /mnt/ext*,
         * /mnt/usb*. This is what the UI wants to render as "available
         * storage" and consolidates all the classification logic on the
         * payload side so the engine + renderer don't need to second-guess. */
        if (!is_user_storage_path(mnt_on)) continue;

        /* Ours-or-theirs split. For mounts under /mnt/ps5upload/<name> we
         * already know the mount is real (we created it), so skip the
         * /dev/-prefix and total>0 filters. Those filters exist to hide
         * ghost /mnt/ext*, /mnt/usb* slots the firmware pre-creates even
         * without a drive attached, and to hide LVD-layered mounts that
         * report f_blocks==0 for other tools. For our own mounts, those
         * filters incorrectly hide the volume from the Volumes screen,
         * which in turn hides the Unmount button. */
        const int is_ours = (strncmp(mnt_on, "/mnt/ps5upload/", 15) == 0);
        if (!is_ours) {
            /* Real-device gate for the /mnt/ext* and /mnt/usb* slots:
             * their paths are hot-plug placeholders, so we require a
             * `/dev/` prefix on mount_from to avoid surfacing sandbox
             * nullfs mounts or unmounted slots. /data is exempt — it's
             * a single well-known internal mount whose backing can be
             * a label ref, a nullfs view on some firmware configurations,
             * or a block device. The path allowlist above already
             * guarantees /data is the real internal user partition, and
             * the total==0 check below filters ghost mounts regardless
             * of backing shape. */
            if (strcmp(mnt_on, "/data") != 0 &&
                strncmp(mnt_from, "/dev/", 5) != 0) continue;

            /* Sanity: a mount reporting zero blocks is either being set up
             * or tearing down — hide it rather than showing a broken slot. */
            if (total == 0) continue;
        }

        char path_esc[256];
        char from_esc[128];
        char source_esc[512] = "";
        json_escape_into(mnt_on,   path_esc, sizeof(path_esc));
        json_escape_into(mnt_from, from_esc, sizeof(from_esc));

        /* For our own mounts, look up the .src tracker to surface the
         * backing image path. Non-ours mounts leave source_image empty. */
        if (is_ours) {
            /* mount_name is the basename after /mnt/ps5upload/ — skip
             * the 15-byte prefix to isolate it. */
            const char *mount_name = mnt_on + 15;
            char src_raw[256];
            if (mount_tracker_read(mount_name, src_raw, sizeof(src_raw))) {
                json_escape_into(src_raw, source_esc, sizeof(source_esc));
            }
        }

        /* `is_placeholder` is kept in the schema for engine/UI compat
         * with older builds, but is always false now — the filter above
         * already rejects anything we'd have called a placeholder. */
        n = snprintf(resp + off, RESP_CAP - off,
                     "%s{\"path\":\"%s\",\"mount_from\":\"%s\","
                     "\"fs_type\":\"%s\","
                     "\"total_bytes\":%llu,\"free_bytes\":%llu,"
                     "\"writable\":%s,\"is_placeholder\":false,"
                     "\"source_image\":\"%s\"}",
                     first_volume ? "" : ",",
                     path_esc,
                     from_esc,
                     mnts[i].f_fstypename,
                     (unsigned long long)total,
                     (unsigned long long)avail,
                     writable ? "true" : "false",
                     source_esc);
        if (n < 0 || (size_t)n >= RESP_CAP - off) {
            fprintf(stderr, "[payload2] fs_list_volumes: response buffer full at %d/%d mounts\n",
                    i, nmnts);
            break;
        }
        off += (size_t)n;
        first_volume = 0;
    }

    n = snprintf(resp + off, RESP_CAP - off, "]}");
    if (n < 0 || (size_t)n >= RESP_CAP - off) { free(resp); return -1; }
    off += (size_t)n;

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_FS_LIST_VOLUMES_ACK, 0, trace_id,
                    resp, (uint64_t)off);
    free(resp);
    return rc;
}

/* ── FS_LIST_DIR handler ─────────────────────────────────────────────────────
 *
 * Enumerates immediate children of a directory. Bounded response:
 *   - max 256 entries per call; clients paginate with `offset`
 *   - each entry is {name, kind, size}; kind in {file, dir, link, other}
 *   - response body sized to fit a 32 KiB stack buffer (256 × ~120B/entry)
 *
 * Safety: path must be absolute (`/`-prefixed) and must not contain `..`.
 * The payload does not impose a prefix allowlist here — read access is
 * gated by the kernel's own permissions on the running payload process.
 *
 * Request JSON: `{path, offset?, limit?}`
 * Response JSON: `{path, entries:[{name,kind,size}], truncated:bool, total_scanned:N}`
 */
static int handle_fs_list_dir(runtime_state_t *state, int client_fd,
                               uint64_t trace_id,
                               const char *request_body, uint64_t body_len) {
    const size_t RESP_CAP = 32u * 1024u;
    char path[512];
    char *resp = NULL;
    size_t off = 0;
    int n;
    int rc;
    DIR *dir = NULL;
    struct dirent *ent = NULL;
    uint64_t offset_req = 0;
    uint64_t limit_req = 256;
    uint64_t idx = 0;      /* running index over all readdir results */
    uint64_t emitted = 0;
    int first_entry = 1;
    int truncated = 0;

    if (!state) return -1;
    (void)body_len;
    path[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "path", path, sizeof(path));
        {
            uint64_t v = extract_json_uint64_field(request_body, "offset");
            if (v > 0) offset_req = v;
        }
        {
            uint64_t v = extract_json_uint64_field(request_body, "limit");
            if (v > 0 && v < 1024) limit_req = v;
            /* Clamp to the buffer-safe ceiling. 256 ≈ 30 KiB response body. */
            if (limit_req > 256) limit_req = 256;
        }
    }
    /* 32 KiB on the stack is risky on PS5's bounded thread stacks; heap-alloc. */
    resp = (char *)malloc(RESP_CAP);
    if (!resp) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_list_dir_oom", 15);
    }
    if (!path[0] || path[0] != '/') {
        free(resp);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_list_dir_bad_path", 20);
    }
    if (strstr(path, "..") != NULL) {
        free(resp);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_list_dir_path_denied", 23);
    }

    dir = opendir(path);
    if (!dir) {
        char err[80];
        int el = snprintf(err, sizeof(err), "fs_list_dir_opendir_errno_%d", errno);
        if (el < 0) el = 0;
        free(resp);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id, err, (uint64_t)el);
    }

    n = snprintf(resp + off, RESP_CAP - off,
                 "{\"path\":\"%s\",\"entries\":[", path);
    if (n < 0 || (size_t)n >= RESP_CAP - off) {
        closedir(dir);
        free(resp);
        return -1;
    }
    off += (size_t)n;

    while ((ent = readdir(dir)) != NULL) {
        const char *name = ent->d_name;
        char full[1024];
        struct stat st;
        const char *kind = "other";
        uint64_t size = 0;
        int stat_ok;

        /* Skip "." and ".." — callers never care and they would otherwise
         * inflate every response by two entries. */
        if (name[0] == '.' && (name[1] == '\0' || (name[1] == '.' && name[2] == '\0'))) {
            continue;
        }

        if (idx < offset_req) {
            idx += 1;
            continue;
        }
        if (emitted >= limit_req) {
            truncated = 1;
            break;
        }

        if (snprintf(full, sizeof(full), "%s/%s", path, name) >= (int)sizeof(full)) {
            idx += 1;
            continue;  /* name too long — skip rather than truncate JSON */
        }
        stat_ok = (lstat(full, &st) == 0);
        if (stat_ok) {
            if (S_ISDIR(st.st_mode)) kind = "dir";
            else if (S_ISREG(st.st_mode)) { kind = "file"; size = (uint64_t)st.st_size; }
            else if (S_ISLNK(st.st_mode)) kind = "link";
        } else {
            /* stat failure is common on cross-mount symlinks and special
             * entries; skip silently rather than poisoning the whole list. */
            kind = "unknown";
        }

        /* Escape name for JSON: only double-quote and backslash need escaping;
         * control chars are rare in filenames and would require a bigger
         * escape table. Conservative: replace them with '?'. */
        {
            char esc[256];
            size_t ei = 0;
            size_t ni = 0;
            for (; name[ni] && ei + 2 < sizeof(esc); ni++) {
                unsigned char c = (unsigned char)name[ni];
                if (c == '"' || c == '\\') {
                    esc[ei++] = '\\';
                    esc[ei++] = (char)c;
                } else if (c < 0x20) {
                    esc[ei++] = '?';
                } else {
                    esc[ei++] = (char)c;
                }
            }
            esc[ei] = '\0';

            n = snprintf(resp + off, RESP_CAP - off,
                         "%s{\"name\":\"%s\",\"kind\":\"%s\",\"size\":%llu}",
                         first_entry ? "" : ",",
                         esc, kind,
                         (unsigned long long)size);
        }
        if (n < 0 || (size_t)n >= RESP_CAP - off) {
            truncated = 1;
            break;
        }
        off += (size_t)n;
        first_entry = 0;
        emitted += 1;
        idx += 1;
    }
    closedir(dir);

    n = snprintf(resp + off, RESP_CAP - off,
                 "],\"truncated\":%s,\"total_scanned\":%llu,\"returned\":%llu}",
                 truncated ? "true" : "false",
                 (unsigned long long)idx,
                 (unsigned long long)emitted);
    if (n < 0 || (size_t)n >= RESP_CAP - off) {
        free(resp);
        return -1;
    }
    off += (size_t)n;

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_FS_LIST_DIR_ACK, 0, trace_id,
                    resp, (uint64_t)off);
    free(resp);
    return rc;
}

/* ── FS_HASH handler ─────────────────────────────────────────────────────────
 *
 * Computes the BLAKE3 hash of a single file on the PS5 and returns the
 * 32-byte digest as hex, plus the file's size for consistency checks.
 * Used by the desktop app's "Safe" resume mode to reconcile local-vs-
 * remote content when size equality alone isn't enough.
 *
 * Request JSON:  `{"path":"/abs/path/to/file"}`
 * Response JSON: `{"path":"...","size":N,"hash":"<64-hex-chars>"}`
 *
 * Safety: path must be absolute and must not contain `..`. Read access
 * is gated by the kernel's own permissions; no prefix allowlist.
 *
 * Performance: streams the file in 64 KiB chunks — doesn't buffer the
 * whole file in memory. A 1 GiB file hashes in ~2-3 s on PS5 UFS. For
 * large trees the client should be judicious about how often it calls
 * this (Safe mode only, and only after Fast already filtered by size).
 */
static int handle_fs_hash(runtime_state_t *state, int client_fd,
                           uint64_t trace_id,
                           const char *request_body, uint64_t body_len) {
    /* Per-function stack budget is tight on the PS5 mgmt thread — keep
     * large buffers on the heap. An earlier version had `buf[65536]` on
     * stack which crashed the payload when Safe-mode reconcile called
     * FS_HASH in rapid succession. Local allocations here are all small
     * (<1 KiB combined); the 64 KiB read buffer + the blake3_hasher
     * live on the heap. */
    enum { READ_BUF_SIZE = 65536 };
    char path[512];
    char resp[256];
    char hex[BLAKE3_OUT_LEN * 2 + 1];
    blake3_hasher *hasher = NULL;
    unsigned char *buf = NULL;
    uint8_t digest[BLAKE3_OUT_LEN];
    struct stat st;
    ssize_t r;
    int fd = -1;
    int n;
    int rc;
    static const char hexchars[] = "0123456789abcdef";
    size_t i;

    if (!state) return -1;
    (void)body_len;
    path[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "path", path, sizeof(path));
    }
    if (!path[0] || path[0] != '/' || strstr(path, "..") != NULL) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_hash_bad_path", 16);
    }
    if (stat(path, &st) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_hash_stat_failed", 19);
    }
    if (!S_ISREG(st.st_mode)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_hash_not_regular_file", 24);
    }
    buf = (unsigned char *)malloc(READ_BUF_SIZE);
    hasher = (blake3_hasher *)malloc(sizeof(*hasher));
    if (!buf || !hasher) {
        free(buf);
        free(hasher);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_hash_oom", 11);
    }
    fd = open(path, O_RDONLY);
    if (fd < 0) {
        free(buf);
        free(hasher);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_hash_open_failed", 19);
    }
    blake3_hasher_init(hasher);
    for (;;) {
        r = read(fd, buf, READ_BUF_SIZE);
        if (r < 0) {
            if (errno == EINTR) continue;
            close(fd);
            free(buf);
            free(hasher);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_hash_read_failed", 19);
        }
        if (r == 0) break;
        blake3_hasher_update(hasher, buf, (size_t)r);
    }
    close(fd);
    blake3_hasher_finalize(hasher, digest, BLAKE3_OUT_LEN);
    free(buf);
    free(hasher);

    for (i = 0; i < BLAKE3_OUT_LEN; i++) {
        hex[2 * i]     = hexchars[(digest[i] >> 4) & 0xf];
        hex[2 * i + 1] = hexchars[digest[i] & 0xf];
    }
    hex[BLAKE3_OUT_LEN * 2] = '\0';

    n = snprintf(resp, sizeof(resp),
                 "{\"path\":\"%s\",\"size\":%llu,\"hash\":\"%s\"}",
                 path, (unsigned long long)st.st_size, hex);
    if (n < 0 || (size_t)n >= sizeof(resp)) return -1;

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_FS_HASH_ACK, 0, trace_id,
                    resp, (uint64_t)n);
    return rc;
}

/* ── Writable-root allowlist ─────────────────────────────────────────────────
 *
 * Destructive FS ops (delete/move/chmod/mkdir) must never touch system
 * paths. The rule: absolute paths only, no `..`, and the path must
 * start with one of the writable roots below. A user running `rm -rf`
 * on `/system` would be bad; this allowlist is the guard.
 *
 *   /data/...         — internal storage user area
 *   /user/...         — per-user profile + save data
 *   /mnt/ext<digit>/  — external SSDs
 *   /mnt/usb<digit>/  — USB-attached storage
 *
 * Intentionally excludes /system, / (root), /dev, /tmp. If a PS5 variant
 * exposes more writable roots, extend `is_path_allowed`. */
static int is_path_allowed(const char *p) {
    if (!p || p[0] != '/') return 0;
    if (strstr(p, "..") != NULL) return 0;
    /* Accept exactly /data or /data/... */
    if (strcmp(p, "/data") == 0 || strncmp(p, "/data/", 6) == 0) return 1;
    if (strcmp(p, "/user") == 0 || strncmp(p, "/user/", 6) == 0) return 1;
    /* /mnt/ext<digit>[/...] and /mnt/usb<digit>[/...] */
    if (strncmp(p, "/mnt/ext", 8) == 0 || strncmp(p, "/mnt/usb", 8) == 0) {
        const char *q = p + 8;
        if (*q < '0' || *q > '9') return 0;
        q++;
        /* Optionally a second digit, then either end-of-string or '/'. */
        if (*q >= '0' && *q <= '9') q++;
        return *q == '\0' || *q == '/';
    }
    /* /mnt/ps5upload/<name>[/...] — we create these via FS_MOUNT and
     * surface them in FS_LIST_VOLUMES, so destructive ops (delete/move/
     * chmod/copy/mkdir/read) need to apply to their contents too. Without
     * this, the File System tab could list files inside a mounted image
     * but every edit would hit "path not allowed". The base dir itself
     * (/mnt/ps5upload or /mnt/ps5upload/) is deliberately excluded —
     * callers should target a specific mount's subtree. */
    if (strncmp(p, "/mnt/ps5upload/", 15) == 0 && p[15] != '\0') return 1;
    return 0;
}

/* Recursively remove `path`. Descends directories, unlinks regular files
 * and symlinks, rmdir's empty directories. Depth cap prevents a
 * pathological symlink loop from exhausting the thread stack. */
static int rm_rf(const char *path, int depth) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    char sub[1024];
    int rc = 0;

    if (depth > 64) return -1;
    if (lstat(path, &st) != 0) return -1;
    if (!S_ISDIR(st.st_mode)) {
        /* Regular file / symlink / device. unlink() works for all. */
        return unlink(path);
    }
    d = opendir(path);
    if (!d) return -1;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        int n = snprintf(sub, sizeof(sub), "%s/%s", path, e->d_name);
        if (n < 0 || (size_t)n >= sizeof(sub)) { rc = -1; break; }
        if (rm_rf(sub, depth + 1) != 0) { rc = -1; /* keep going; best effort */ }
    }
    closedir(d);
    if (rmdir(path) != 0) rc = -1;
    return rc;
}

/* Recursive chmod on `path`. Descends dirs. Same depth cap as rm_rf. */
static int chmod_rf(const char *path, mode_t mode, int depth) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    char sub[1024];
    int rc = 0;

    if (depth > 64) return -1;
    if (lstat(path, &st) != 0) return -1;
    /* chmod first, then descend — so even if recursion fails partially
     * we've at least updated the top. */
    if (chmod(path, mode) != 0) rc = -1;
    if (!S_ISDIR(st.st_mode)) return rc;
    d = opendir(path);
    if (!d) return -1;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        int n = snprintf(sub, sizeof(sub), "%s/%s", path, e->d_name);
        if (n < 0 || (size_t)n >= sizeof(sub)) { rc = -1; break; }
        if (chmod_rf(sub, mode, depth + 1) != 0) rc = -1;
    }
    closedir(d);
    return rc;
}

/* Recursive copy from `src` to `dst`. Descends dirs, copies regular
 * files byte-for-byte with mode bits preserved. Refuses to overwrite
 * an existing `dst` — the caller (FS_COPY handler) pre-checks. Depth
 * cap matches rm_rf/chmod_rf to bound symlink-loop stack usage.
 *
 * Symlinks are recreated as symlinks (not dereferenced) — matches
 * `cp -R` semantics. Special files (devices, sockets, fifos) are
 * skipped with a log line; they don't belong in a user data tree.
 *
 * Throughput: 4 MiB IOs + posix_fadvise(SEQUENTIAL) hint + up-front
 * posix_fallocate so the FS reserves contiguous extents. The previous
 * 64 KiB buffer required ~540 thousand read/write syscall pairs for
 * a 33 GiB file — syscall overhead alone capped throughput at
 * ~22 MiB/s on PS5 NVMe, roughly 10× below the disk-limited rate.
 * 4 MiB buffer × kernel-readahead hint should bring us close to the
 * sequential-IO ceiling (~200 MiB/s observed on internal NVMe).
 *
 * The buffer is allocated once at the cp_rf entry that triggers the
 * S_ISREG branch — recursive descent re-allocates per file. Trading
 * peak RSS for throughput here is fine because cp_rf isn't run on
 * the FTX2 transfer port that needs its own 32 MiB shard buffer. */
static int cp_rf(const char *src, const char *dst, int depth) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    char sub_src[1024];
    char sub_dst[1024];
    int rc = 0;
    /* 4 MiB copy buffer. PS5 NVMe sequential IOs reach disk-limited
     * throughput around 1-4 MiB request size; below that we're
     * syscall-bound. Cost of the alloc is one mmap on FreeBSD's
     * malloc — negligible against multi-second copies. */
    const size_t COPY_BUF = 4u * 1024u * 1024u;

    if (depth > 64) return -1;
    if (lstat(src, &st) != 0) return -1;

    if (S_ISLNK(st.st_mode)) {
        char target[1024];
        ssize_t len = readlink(src, target, sizeof(target) - 1);
        if (len < 0) return -1;
        target[len] = '\0';
        if (symlink(target, dst) != 0) return -1;
        return 0;
    }

    if (S_ISREG(st.st_mode)) {
        int sfd = open(src, O_RDONLY);
        if (sfd < 0) return -1;
        int dfd = open(dst, O_WRONLY | O_CREAT | O_EXCL, st.st_mode & 0777);
        if (dfd < 0) { close(sfd); return -1; }
        /* Tell the kernel: we'll read this file front-to-back, prefetch
         * aggressively. FreeBSD's readahead defaults to 64 KiB which is
         * what was throttling us. With the SEQUENTIAL hint the kernel
         * scales the readahead window up to handle large IOs. Failures
         * are non-fatal — just lose the perf hint. */
        (void)posix_fadvise(sfd, 0, 0, POSIX_FADV_SEQUENTIAL);
        /* Reserve contiguous extents on the dst FS so the writer
         * doesn't interleave block-allocation with data writes. UFS2
         * (PS5 internal) honours this; exfatfs (USB exFAT) does too,
         * via the fallocate emulation path.
         *
         * Most failures are non-fatal (FS doesn't support the hint —
         * EINVAL/EOPNOTSUPP on tmpfs etc.) — fall through to the
         * lazy-allocation path. But ENOSPC and EFBIG are early
         * disk-full / file-too-big signals worth surfacing now
         * instead of after we've written half the file: the kernel
         * already proved it can't take the whole thing. Fail fast so
         * the caller sees a specific "destination is full" error
         * rather than a generic mid-copy write failure at some
         * random offset. */
        if (st.st_size > 0) {
            int falloc_rc = posix_fallocate(dfd, 0, st.st_size);
            if (falloc_rc == ENOSPC || falloc_rc == EFBIG) {
                fprintf(stderr,
                        "[payload2] fs_copy: posix_fallocate(%lld bytes) returned %d for %s — bailing early\n",
                        (long long)st.st_size, falloc_rc, dst);
                close(sfd);
                close(dfd);
                /* Best-effort cleanup of the empty/zero-extent dst
                 * the open(O_CREAT) just produced — don't leave a
                 * 0-byte ghost file. */
                (void)unlink(dst);
                return -1;
            }
            /* Other return values (0 success, EINVAL/EOPNOTSUPP,
             * etc.) all fall through and let the read/write loop
             * proceed with lazy allocation. */
        }
        unsigned char *buf = (unsigned char *)malloc(COPY_BUF);
        if (!buf) { close(sfd); close(dfd); return -1; }
        off_t total_written = 0;
        for (;;) {
            ssize_t r = read(sfd, buf, COPY_BUF);
            if (r < 0) {
                if (errno == EINTR) continue;
                /* Log the errno + offset so the engine.log line
                 * "fs_copy failed" has root-cause context. The 4 MiB
                 * buffer + posix_fadvise made each remaining
                 * failure represent 64× more lost progress vs the
                 * pre-perf-fix path, so getting the actual reason
                 * surfaced here matters more now than it did. */
                fprintf(stderr,
                        "[payload2] fs_copy: read(%s) failed at offset %lld errno=%d\n",
                        src, (long long)total_written, errno);
                rc = -1;
                break;
            }
            if (r == 0) break;
            ssize_t written = 0;
            while (written < r) {
                ssize_t w = write(dfd, buf + written, (size_t)(r - written));
                if (w < 0) {
                    if (errno == EINTR) continue;
                    fprintf(stderr,
                            "[payload2] fs_copy: write(%s) failed at offset %lld errno=%d\n",
                            dst, (long long)(total_written + written), errno);
                    rc = -1;
                    break;
                }
                written += w;
            }
            if (rc != 0) break;
            total_written += r;
        }
        free(buf);
        close(sfd);
        close(dfd);
        return rc;
    }

    if (!S_ISDIR(st.st_mode)) {
        /* Special file (device/socket/fifo) — skip. User data trees
         * should never contain these, so silently dropping them is
         * safer than failing the whole copy. */
        fprintf(stderr, "[payload2] fs_copy: skipping non-regular %s\n", src);
        return 0;
    }

    /* Directory: mkdir then descend. EEXIST on dst would indicate the
     * pre-check missed something — treat as error so the caller sees
     * partial-state. */
    if (mkdir(dst, st.st_mode & 0777) != 0) return -1;
    d = opendir(src);
    if (!d) return -1;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        int n1 = snprintf(sub_src, sizeof(sub_src), "%s/%s", src, e->d_name);
        int n2 = snprintf(sub_dst, sizeof(sub_dst), "%s/%s", dst, e->d_name);
        if (n1 < 0 || (size_t)n1 >= sizeof(sub_src) ||
            n2 < 0 || (size_t)n2 >= sizeof(sub_dst)) { rc = -1; break; }
        if (cp_rf(sub_src, sub_dst, depth + 1) != 0) { rc = -1; /* keep going */ }
    }
    closedir(d);
    return rc;
}

/* ── FS_DELETE handler ─────────────────────────────────────────────────── */
static int handle_fs_delete(runtime_state_t *state, int client_fd,
                             uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char path[512];
    (void)body_len;
    if (!state) return -1;
    path[0] = '\0';
    if (request_body) extract_json_string_field(request_body, "path", path, sizeof(path));
    if (!is_path_allowed(path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_delete_path_not_allowed", 26);
    }
    if (rm_rf(path, 0) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_delete_failed", 16);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_FS_DELETE_ACK, 0, trace_id, NULL, 0);
}

/* ── FS_MOVE handler ────────────────────────────────────────────────────
 * rename(2) only works intra-volume on POSIX. Cross-volume moves return
 * EXDEV, which we surface as a specific error so the client can tell
 * the user "move across mounts not supported". */
static int handle_fs_move(runtime_state_t *state, int client_fd,
                           uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char from[512], to[512];
    (void)body_len;
    if (!state) return -1;
    from[0] = '\0'; to[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "from", from, sizeof(from));
        extract_json_string_field(request_body, "to", to, sizeof(to));
    }
    if (!is_path_allowed(from) || !is_path_allowed(to)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_move_path_not_allowed", 24);
    }
    if (rename(from, to) != 0) {
        if (errno == EXDEV) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_move_cross_mount", 19);
        }
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_move_failed", 14);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_FS_MOVE_ACK, 0, trace_id, NULL, 0);
}

/* ── FS_CHMOD handler ──────────────────────────────────────────────────── */
static int handle_fs_chmod(runtime_state_t *state, int client_fd,
                            uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char path[512], mode_str[16];
    uint64_t mode_int;
    int recursive = 0;
    (void)body_len;
    if (!state) return -1;
    path[0] = '\0'; mode_str[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "path", path, sizeof(path));
        extract_json_string_field(request_body, "mode", mode_str, sizeof(mode_str));
        /* recursive is an unsigned field: 1 = recurse, 0 = top only. */
        recursive = extract_json_uint64_field(request_body, "recursive") != 0;
    }
    if (!is_path_allowed(path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_chmod_path_not_allowed", 25);
    }
    if (!mode_str[0]) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_chmod_missing_mode", 21);
    }
    /* Parse mode as octal (e.g. "0777", "0644"). Clamp to 0..07777. */
    mode_int = strtoull(mode_str, NULL, 8);
    if (mode_int > 07777) mode_int = 07777;
    int rc;
    if (recursive) {
        rc = chmod_rf(path, (mode_t)mode_int, 0);
    } else {
        rc = chmod(path, (mode_t)mode_int);
    }
    if (rc != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_chmod_failed", 15);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_FS_CHMOD_ACK, 0, trace_id, NULL, 0);
}

/* ── FS_MKDIR handler ──────────────────────────────────────────────────── */
static int handle_fs_mkdir(runtime_state_t *state, int client_fd,
                            uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char path[512];
    (void)body_len;
    if (!state) return -1;
    path[0] = '\0';
    if (request_body) extract_json_string_field(request_body, "path", path, sizeof(path));
    if (!is_path_allowed(path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mkdir_path_not_allowed", 25);
    }
    /* ensure_parent_dir walks and mkdir-p's every component. Matches the
     * "create intermediate dirs" behavior callers expect. */
    if (ensure_parent_dir(path) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mkdir_parents_failed", 23);
    }
    if (mkdir(path, 0777) != 0 && errno != EEXIST) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mkdir_failed", 15);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_FS_MKDIR_ACK, 0, trace_id, NULL, 0);
}

/* ── FS_READ handler ─────────────────────────────────────────────────────
 *
 * Bounded file-read for metadata fetches (param.json, icon0.png). The
 * payload enforces the same writable-root allowlist destructive ops use
 * plus an absolute hard cap (FS_READ_MAX_BYTES) on how much any single
 * read can return. Requests exceeding the cap are silently truncated —
 * callers that care about exact sizes can check the ACK body length
 * against the file's real size via a prior FS_LIST_DIR entry.
 *
 * Request JSON: {"path":"/abs/...","offset":N,"limit":N}  (offset/limit optional)
 * ACK body:     raw file bytes (no framing, no JSON wrap)
 */
static int handle_fs_read(runtime_state_t *state, int client_fd,
                          uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char path[512];
    struct stat st;
    uint64_t offset = 0;
    uint64_t limit = FS_READ_MAX_BYTES;
    unsigned char *buf = NULL;
    ssize_t r;
    int fd = -1;
    int rc;

    (void)body_len;
    if (!state) return -1;
    path[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "path", path, sizeof(path));
        uint64_t req_offset = extract_json_uint64_field(request_body, "offset");
        uint64_t req_limit  = extract_json_uint64_field(request_body, "limit");
        if (req_offset > 0) offset = req_offset;
        if (req_limit > 0 && req_limit < limit) limit = req_limit;
    }
    if (!is_path_allowed(path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_path_not_allowed", 24);
    }
    if (stat(path, &st) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_stat_failed", 19);
    }
    if (!S_ISREG(st.st_mode)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_not_regular_file", 24);
    }
    /* Clamp limit against actual file size so the ACK body matches what
     * the client can meaningfully consume. Offset past EOF returns an
     * empty ACK (no error) — simplifies paginated reads. */
    uint64_t remaining = 0;
    if ((uint64_t)st.st_size > offset) {
        remaining = (uint64_t)st.st_size - offset;
    }
    if (limit > remaining) limit = remaining;
    if (limit > FS_READ_MAX_BYTES) limit = FS_READ_MAX_BYTES;

    if (limit == 0) {
        /* Zero-length ACK is valid — client sees empty body. */
        return send_frame(client_fd, FTX2_FRAME_FS_READ_ACK, 0, trace_id, NULL, 0);
    }

    buf = (unsigned char *)malloc((size_t)limit);
    if (!buf) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_oom", 11);
    }
    fd = open(path, O_RDONLY);
    if (fd < 0) {
        free(buf);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_open_failed", 19);
    }
    if (offset > 0 && lseek(fd, (off_t)offset, SEEK_SET) < 0) {
        close(fd);
        free(buf);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_read_seek_failed", 19);
    }
    /* read() on FreeBSD can return less than requested even for regular
     * files (e.g. when reaching a page-cache boundary mid-read). Loop
     * until we've filled `limit` bytes or hit EOF. */
    size_t got = 0;
    while (got < (size_t)limit) {
        r = read(fd, buf + got, (size_t)limit - got);
        if (r < 0) {
            if (errno == EINTR) continue;
            close(fd);
            free(buf);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              "fs_read_read_failed", 19);
        }
        if (r == 0) break;
        got += (size_t)r;
    }
    close(fd);

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_FS_READ_ACK, 0, trace_id,
                    (const char *)buf, (uint64_t)got);
    free(buf);
    return rc;
}

/* ── FS_COPY handler ─────────────────────────────────────────────────────
 *
 * Request JSON: {"from":"/abs/...","to":"/abs/..."}
 * ACK body:     empty (success); FTX2_FRAME_ERROR with a diagnostic tag
 *               on failure.
 *
 * Pre-checks: both paths pass is_path_allowed, `from` exists, `to` does
 * not. Cross-mount copies are fine (unlike FS_MOVE's rename()) because
 * cp_rf reads and writes bytes explicitly. That's the whole point of
 * having a separate FS_COPY frame — users need a way to move content
 * off the internal drive onto a USB or vice versa.
 */
static int handle_fs_copy(runtime_state_t *state, int client_fd,
                           uint64_t trace_id, const char *request_body, uint64_t body_len) {
    char from[512], to[512];
    struct stat st;
    (void)body_len;
    if (!state) return -1;
    from[0] = '\0'; to[0] = '\0';
    if (request_body) {
        extract_json_string_field(request_body, "from", from, sizeof(from));
        extract_json_string_field(request_body, "to", to, sizeof(to));
    }
    if (!is_path_allowed(from) || !is_path_allowed(to)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_path_not_allowed", 24);
    }
    if (stat(from, &st) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_source_missing", 22);
    }
    /* Refuse to clobber. The engine should have pre-checked via
     * FS_LIST_DIR; if we got here with dst present, it's a race or a
     * client bug — fail loudly. */
    struct stat dst_st;
    if (lstat(to, &dst_st) == 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_dest_exists", 19);
    }
    if (cp_rf(from, to, 0) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_copy_failed", 14);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_FS_COPY_ACK, 0, trace_id, NULL, 0);
}

/* ── FS_MOUNT / FS_UNMOUNT helpers ───────────────────────────────────────
 *
 * MD-backend attach + nmount pipeline. Simplified to the subset we
 * need:
 *   - MD (memory-disk) attach only; no LVD, no PFS
 *   - exfatfs (.exfat) and ufs (.ffpkg) only; no PFS crypto
 *   - Single mount root (/mnt/ps5upload/) so it never collides
 *     with mount-points other utilities create.
 */

/* Fill `out` with an iovec entry for a NUL-terminated C string. The
 * iovec length MUST include the trailing NUL because nmount parses
 * key=value pairs via strings. Empty value (NULL) is allowed — used
 * for boolean flags like "async" / "noatime". */
static void fs_mount_iov(struct iovec *out, const char *s) {
    if (s == NULL) {
        out->iov_base = NULL;
        out->iov_len = 0;
    } else {
        out->iov_base = (void *)s;
        out->iov_len = strlen(s) + 1;
    }
}

/* Wait for a device node to exist (or disappear, when exist=0).
 * Returns 0 on success, -1 on timeout. */
static int fs_mount_wait_node(const char *devname, int exist) {
    struct stat st;
    int i;
    for (i = 0; i < FS_MOUNT_DEV_WAIT_RETRIES; i++) {
        int got = (stat(devname, &st) == 0);
        if (exist ? got : !got) return 0;
        usleep(FS_MOUNT_DEV_WAIT_US);
    }
    return -1;
}

/* Filename test: lowercase extension match for .exfat or .ffpkg. */
static int fs_mount_detect_fstype(const char *image_path, char *fstype_out, size_t cap) {
    size_t len = strlen(image_path);
    if (len < 6) return -1;
    const char *ext = image_path + len;
    while (ext > image_path && *(ext - 1) != '.') ext--;
    if (ext == image_path || *(ext - 1) != '.') return -1;
    char buf[16];
    size_t el = strlen(ext);
    if (el >= sizeof(buf)) return -1;
    size_t j;
    for (j = 0; j < el; j++) {
        char c = ext[j];
        buf[j] = (c >= 'A' && c <= 'Z') ? (char)(c - 'A' + 'a') : c;
    }
    buf[el] = '\0';
    if (strcmp(buf, "exfat") == 0) {
        snprintf(fstype_out, cap, "exfatfs");
        return 0;
    }
    if (strcmp(buf, "ffpkg") == 0) {
        snprintf(fstype_out, cap, "ufs");
        return 0;
    }
    return -1;
}

/* Derive a filesystem-safe mount name from the image basename:
 * strip directory prefix, strip the trailing .exfat/.ffpkg extension,
 * replace anything outside [A-Za-z0-9_.-] with '_'. Output is written
 * to `out` with a guaranteed NUL terminator.
 *
 * Examples:
 *   /data/homebrew/Foo Bar.exfat  → "Foo_Bar"
 *   /mnt/ext1/My-Game.ffpkg       → "My-Game"
 */
static void fs_mount_derive_name(const char *image_path, char *out, size_t cap) {
    if (cap == 0) return;
    if (image_path == NULL) {
        /* Defensive — extract_json_string_field always writes a NUL
         * before returning, so live callers never pass NULL. Keep the
         * guard so a future refactor that skips the extract can't
         * crash us with a NULL deref inside strrchr. */
        snprintf(out, cap, "image");
        return;
    }
    const char *base = strrchr(image_path, '/');
    base = base ? base + 1 : image_path;
    size_t len = strlen(base);
    /* Strip extension. */
    const char *dot = strrchr(base, '.');
    if (dot != NULL && dot > base) len = (size_t)(dot - base);
    if (len >= cap) len = cap - 1;
    size_t i;
    for (i = 0; i < len; i++) {
        char c = base[i];
        int ok = (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') ||
                 (c >= '0' && c <= '9') || c == '_' || c == '-' || c == '.';
        out[i] = ok ? c : '_';
    }
    out[len] = '\0';
    if (len == 0) snprintf(out, cap, "image");
}

/* Try ordinary unmount; if busy, force. Used by both FS_UNMOUNT and
 * the error-cleanup path of FS_MOUNT. Returns 0 on success, -1 on
 * failure (errno preserved). */
static int fs_mount_try_unmount(const char *mount_point) {
    if (unmount(mount_point, 0) == 0) return 0;
    if (errno == EINVAL || errno == ENOENT) return 0;
    if (unmount(mount_point, MNT_FORCE) == 0) return 0;
    return -1;
}

/* Detach an MD unit. Tries plain detach, then MD_FORCE if that fails
 * with EBUSY — leaving an attached unit orphans a kernel resource
 * until reboot, so we lean on force after a polite attempt. */
static int fs_mount_detach_md(int unit_id) {
    if (unit_id < 0) return 0;
    int fd = open(FS_MOUNT_MD_CTL, O_RDWR);
    if (fd < 0) return -1;
    struct md_ioctl req;
    memset(&req, 0, sizeof(req));
    req.md_version = MDIOVERSION;
    req.md_unit = (unsigned int)unit_id;
    int rc = ioctl(fd, MDIOCDETACH, &req);
    if (rc != 0) {
        req.md_options = MD_FORCE;
        rc = ioctl(fd, MDIOCDETACH, &req);
    }
    close(fd);
    return (rc == 0) ? 0 : -1;
}

/* Detach an LVD unit. No force variant on LVD — best effort. */
static int fs_mount_detach_lvd(int unit_id) {
    if (unit_id < 0) return 0;
    int fd = open(FS_MOUNT_LVD_CTL, O_RDWR);
    if (fd < 0) return -1;
    fs_mount_lvd_detach_t req;
    memset(&req, 0, sizeof(req));
    req.device_id = unit_id;
    int rc = ioctl(fd, FS_MOUNT_LVD_IOC_DETACH, &req);
    close(fd);
    return (rc == 0) ? 0 : -1;
}

/* Attach a disk image via the Sony LVD driver. Returns assigned unit
 * id on success, or -1 with errno set on failure. `is_exfat` chooses
 * the sector/flags/image_type triple. On PS5, this is the primary
 * path: MDIOCATTACH often returns EPERM or EINVAL on PS5 where LVD
 * succeeds, because PS5 routes file-backed block devices through
 * its own virtualized layer.
 *
 * The V0 attach ioctl takes a single layer descriptor pointing at
 * the user-space path. The kernel opens the file itself inside the
 * ioctl handler, so we don't need to keep an open fd around. */
static int fs_mount_attach_lvd(const char *image_path, off_t size, int is_exfat) {
    int fd = open(FS_MOUNT_LVD_CTL, O_RDWR);
    if (fd < 0) return -1;

    fs_mount_lvd_layer_t layer;
    memset(&layer, 0, sizeof(layer));
    layer.source_type = 1;                 /* LVD_ENTRY_TYPE_FILE */
    layer.flags       = 0x1;               /* LVD_ENTRY_FLAG_NO_BITMAP */
    layer.path        = image_path;
    layer.offset      = 0;
    layer.size        = (uint64_t)size;

    fs_mount_lvd_attach_t req;
    memset(&req, 0, sizeof(req));
    req.io_version     = 0;                          /* V0 */
    req.device_id      = -1;                         /* auto-assign */
    req.sector_size    = is_exfat ? 512u : 4096u;
    req.secondary_unit = is_exfat ? FS_MOUNT_LVD_SECONDARY_SINGLE : 4096u;
    req.flags          = is_exfat ? FS_MOUNT_LVD_FLAGS_EXFAT_RW
                                  : FS_MOUNT_LVD_FLAGS_UFS_RW;
    req.image_type     = is_exfat ? 0u /* SINGLE */ : 7u /* UFS_DD */;
    req.layer_count    = 1;
    req.device_size    = (uint64_t)size;
    req.layers_ptr     = &layer;

    int rc = ioctl(fd, FS_MOUNT_LVD_IOC_ATTACH_V0, &req);
    int saved_errno = errno;
    close(fd);
    if (rc != 0) {
        errno = saved_errno;
        return -1;
    }
    return req.device_id;
}

/* Attach via the plain FreeBSD memory-disk driver. Fallback path used
 * when LVD is unavailable or refuses the attach. Returns assigned
 * unit id on success, or -1 with errno set on failure. */
static int fs_mount_attach_md(const char *image_path, off_t size, int is_exfat) {
    /* `is_exfat` retained for parity with the LVD attach signature;
     * MD uses the same 512-byte sector size for both exfat and ufs on
     * PS5, so the parameter is informational only today. */
    (void)is_exfat;
    int fd = open(FS_MOUNT_MD_CTL, O_RDWR);
    if (fd < 0) return -1;

    struct md_ioctl req;
    memset(&req, 0, sizeof(req));
    req.md_version    = MDIOVERSION;
    req.md_type       = MD_VNODE;
    req.md_file       = (char *)image_path;
    req.md_mediasize  = size;
    /* PS5 images use 512-byte sectors for both exfat and ufs — 4096
     * fails MDIOCATTACH with EINVAL on most firmware. */
    req.md_sectorsize = 512;
    req.md_options    = MD_AUTOUNIT | MD_ASYNC;

    int rc = ioctl(fd, MDIOCATTACH, &req);
    int saved_errno = errno;
    close(fd);
    if (rc != 0) {
        errno = saved_errno;
        return -1;
    }
    return (int)req.md_unit;
}

/* ── FS_MOUNT handler ───────────────────────────────────────────────────
 *
 * Pipeline:
 *   1. Parse {image_path, mount_name?} from JSON body.
 *   2. Verify image_path passes is_path_allowed and names a regular
 *      file. Detect fstype by extension (.exfat → exfatfs, .ffpkg → ufs).
 *   3. Derive or accept a mount_name; mount point = /mnt/ps5upload/<name>/.
 *   4. open(/dev/mdctl); issue MDIOCATTACH with MD_VNODE; wait for
 *      /dev/md<N> to materialize.
 *   5. mkdir mount point; nmount with the appropriate fstype + standard
 *      PS5 mount knobs (async, noatime, budgetid=game, automounted).
 *   6. On nmount failure, detach the MD unit so we don't leak it.
 *   7. Reply with ACK carrying {mount_point, dev_node, fstype}.
 */
static int handle_fs_mount(runtime_state_t *state, int client_fd,
                            uint64_t trace_id, const char *request_body,
                            uint64_t body_len) {
    char image_path[512] = {0};
    char mount_name[128] = {0};
    char fstype[16] = {0};
    char mount_point[256] = {0};
    char devname[32] = {0};
    char resp[512];
    char mount_errmsg[256] = {0};
    struct stat st;
    int unit_id = -1;
    int n;
    (void)body_len;
    if (!state) return -1;

    if (request_body) {
        extract_json_string_field(request_body, "image_path", image_path, sizeof(image_path));
        extract_json_string_field(request_body, "mount_name", mount_name, sizeof(mount_name));
    }
    if (!is_path_allowed(image_path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_path_not_allowed", 25);
    }
    if (stat(image_path, &st) != 0 || !S_ISREG(st.st_mode)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_image_not_a_file", 25);
    }
    if (fs_mount_detect_fstype(image_path, fstype, sizeof(fstype)) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_unsupported_format", 27);
    }
    /* Derive name if caller didn't supply one. Reject caller-supplied
     * names containing path separators — they'd escape /mnt/ps5upload/. */
    if (mount_name[0] == '\0') {
        fs_mount_derive_name(image_path, mount_name, sizeof(mount_name));
    } else if (strchr(mount_name, '/') != NULL || strstr(mount_name, "..") != NULL) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_bad_name", 17);
    }

    n = snprintf(mount_point, sizeof(mount_point), "%s/%s",
                 FS_MOUNT_BASE, mount_name);
    if (n < 0 || (size_t)n >= sizeof(mount_point)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_name_too_long", 22);
    }

    /* Attach pipeline: try LVD first (PS5-native, works on more
     * firmware), fall back to MD if LVD returns an error. Each
     * failure records errno verbatim so clients can surface the
     * actual reason — "fs_mount_attach_failed" alone is useless for
     * diagnosis. The composite error names every attempt.
     *
     * `is_exfat` chooses sector/flags/image_type inside the attach
     * helpers so the call site stays readable. */
    const int is_exfat = (strcmp(fstype, "exfatfs") == 0);
    int lvd_unit = fs_mount_attach_lvd(image_path, st.st_size, is_exfat);
    int lvd_errno = errno;
    int used_lvd = 0;
    if (lvd_unit >= 0) {
        used_lvd = 1;
        unit_id = lvd_unit;
        snprintf(devname, sizeof(devname), "/dev/lvd%d", unit_id);
    } else {
        int md_unit = fs_mount_attach_md(image_path, st.st_size, is_exfat);
        int md_errno = errno;
        if (md_unit < 0) {
            /* Both backends failed — surface both errnos so the user
             * (or a log trawl) can tell whether LVD refused a policy
             * check or MD refused a sector-size, etc. */
            char errbuf[256];
            int el = snprintf(errbuf, sizeof(errbuf),
                              "fs_mount_attach_failed: lvd=%s md=%s",
                              strerror(lvd_errno), strerror(md_errno));
            if (el < 0) el = 0;
            if ((size_t)el >= sizeof(errbuf)) el = (int)sizeof(errbuf) - 1;
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                              errbuf, (uint64_t)el);
        }
        unit_id = md_unit;
        snprintf(devname, sizeof(devname), "/dev/md%d", unit_id);
    }
    if (fs_mount_wait_node(devname, 1) != 0) {
        if (used_lvd) fs_mount_detach_lvd(unit_id);
        else fs_mount_detach_md(unit_id);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_dev_node_missing", 25);
    }

    /* Ensure /mnt/ps5upload/ and /mnt/ps5upload/<name>/ exist. Both
     * EEXIST are fine — we'll nmount over whatever's there. */
    (void)mkdir(FS_MOUNT_BASE, 0777);
    if (mkdir(mount_point, 0777) != 0 && errno != EEXIST) {
        if (used_lvd) fs_mount_detach_lvd(unit_id);
        else fs_mount_detach_md(unit_id);
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_mount_mkdir_failed", 21);
    }

    /* Build nmount iovec. Keys + ordering chosen to match what the
     * PS5 kernel accepts across firmware revisions — experimenting
     * with these on hardware tends to break mounts. "budgetid=game"
     * is the PS5-specific resource class for user-installed titles.
     *
     * Slot budget for the exfatfs branch (worst case):
     *   fstype/from/fspath/budgetid          = 8
     *   large/timezone/ignoreacl             = 6 (exfat only)
     *   async/noatime/automounted            = 6
     *   errmsg                               = 2
     * = 22 slots. Array sized to 32 for headroom in case we add another
     * pair (e.g. "ro", "force") without needing to revisit this count. */
    struct iovec iov[32];
    int iovlen = 0;
    #define FS_MOUNT_PUSH(k, v) do { \
        fs_mount_iov(&iov[iovlen++], (k)); \
        fs_mount_iov(&iov[iovlen++], (v)); \
    } while (0)
    FS_MOUNT_PUSH("fstype", fstype);
    FS_MOUNT_PUSH("from", devname);
    FS_MOUNT_PUSH("fspath", mount_point);
    FS_MOUNT_PUSH("budgetid", "game");
    if (strcmp(fstype, "exfatfs") == 0) {
        FS_MOUNT_PUSH("large", "yes");
        FS_MOUNT_PUSH("timezone", "static");
        FS_MOUNT_PUSH("ignoreacl", NULL);
    }
    FS_MOUNT_PUSH("async", NULL);
    FS_MOUNT_PUSH("noatime", NULL);
    FS_MOUNT_PUSH("automounted", NULL);
    /* errmsg buffer — nmount writes the kernel-side error string here
     * on failure. Surface it verbatim to the client so diagnostics
     * survive the wire trip. */
    fs_mount_iov(&iov[iovlen++], "errmsg");
    iov[iovlen].iov_base = mount_errmsg;
    iov[iovlen].iov_len = sizeof(mount_errmsg);
    iovlen++;
    #undef FS_MOUNT_PUSH

    if (nmount(iov, (unsigned)iovlen, 0) != 0) {
        char errbuf[320];
        int len = snprintf(errbuf, sizeof(errbuf),
                           "fs_mount_nmount_failed: %s",
                           mount_errmsg[0] ? mount_errmsg : strerror(errno));
        if (used_lvd) fs_mount_detach_lvd(unit_id);
        else fs_mount_detach_md(unit_id);
        /* Clean up the empty mount dir so repeated attempts don't pile
         * up stale /mnt/ps5upload/<name> directories. */
        rmdir(mount_point);
        if (len < 0) len = 0;
        if ((size_t)len >= sizeof(errbuf)) len = (int)sizeof(errbuf) - 1;
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          errbuf, (uint64_t)len);
    }

    /* Record the source image path so Volumes can surface it and
     * reconciliation can validate on next boot. Best-effort — a
     * failed tracker write doesn't undo the successful mount. */
    mount_tracker_write(mount_name, image_path);

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    n = snprintf(resp, sizeof(resp),
                 "{\"mount_point\":\"%s\",\"dev_node\":\"%s\",\"fstype\":\"%s\","
                 "\"source_image\":\"%s\"}",
                 mount_point, devname, fstype, image_path);
    if (n < 0 || (size_t)n >= sizeof(resp)) n = 0;
    return send_frame(client_fd, FTX2_FRAME_FS_MOUNT_ACK, 0, trace_id,
                      resp, (uint64_t)n);
}

/* ── FS_UNMOUNT handler ─────────────────────────────────────────────────
 *
 * Only lets callers unmount things under /mnt/ps5upload/ so a malicious
 * or mistaken request can't tear down system mounts or mounts owned
 * by other tools. Resolves which backend (LVD or MD) backs the mount
 * via getmntinfo
 * and dispatches to the matching detach helper — skipping that would
 * leak the attachment per mount/unmount cycle. */
static int handle_fs_unmount(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *request_body,
                              uint64_t body_len) {
    char mount_point[256] = {0};
    (void)body_len;
    if (!state) return -1;
    if (request_body) {
        extract_json_string_field(request_body, "mount_point",
                                  mount_point, sizeof(mount_point));
    }
    const size_t base_len = strlen(FS_MOUNT_BASE);
    if (strncmp(mount_point, FS_MOUNT_BASE "/", base_len + 1) != 0 ||
        strstr(mount_point, "..") != NULL) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_unmount_not_our_mount", 24);
    }

    /* Backend detection: match the mount's f_mntfromname against
     * /dev/md<N> or /dev/lvd<N> so we dispatch to the right detach
     * helper after the unmount. Either backend may back a mount
     * depending on what FS_MOUNT's attach pipeline ended up using. */
    int detach_md_unit  = -1;
    int detach_lvd_unit = -1;
    struct statfs *mnts = NULL;
    int nmnts = getmntinfo(&mnts, MNT_NOWAIT);
    for (int i = 0; i < nmnts && mnts != NULL; i++) {
        if (strcmp(mnts[i].f_mntonname, mount_point) != 0) continue;
        const char *from = mnts[i].f_mntfromname;
        if (strncmp(from, "/dev/lvd", 8) == 0 &&
            from[8] >= '0' && from[8] <= '9') {
            detach_lvd_unit = atoi(from + 8);
        } else if (strncmp(from, "/dev/md", 7) == 0 &&
                   from[7] >= '0' && from[7] <= '9') {
            detach_md_unit = atoi(from + 7);
        }
        break;
    }

    if (fs_mount_try_unmount(mount_point) != 0) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "fs_unmount_failed", 17);
    }
    /* Best-effort detach. Worst case we leave the attachment; a fresh
     * mount of the same image gets a new unit. Never fail the user
     * request on detach alone. */
    if (detach_lvd_unit >= 0) (void)fs_mount_detach_lvd(detach_lvd_unit);
    if (detach_md_unit  >= 0) (void)fs_mount_detach_md(detach_md_unit);
    /* Clean up the (now-empty) mount-point directory. */
    rmdir(mount_point);
    /* Remove the per-mount source tracker. The mount_point is
     * /mnt/ps5upload/<name> so the trailing basename is the name
     * we used when writing the tracker. */
    {
        const char *name = mount_point + base_len + 1;  /* skip "/mnt/ps5upload/" */
        mount_tracker_remove(name);
    }

    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_FS_UNMOUNT_ACK, 0, trace_id,
                      NULL, 0);
}

/* ── APP_* handlers (thin wrappers over register.c) ─────────────────────
 *
 * The heavy lifting lives in payload/src/register.c. These handlers
 * only parse the request body, call into the register module, and
 * turn the returned err_reason
 * string into an ERROR frame. Each handler bumps command_count so
 * STATUS_ACK reflects management-port activity. */

static int handle_app_register(runtime_state_t *state, int client_fd,
                                uint64_t trace_id, const char *request_body,
                                uint64_t body_len) {
    char src_path[512] = {0};
    char title_id[REGISTER_MAX_TITLE_ID] = {0};
    char title_name[REGISTER_MAX_TITLE_NAME] = {0};
    char title_name_esc[REGISTER_MAX_TITLE_NAME * 2 + 2];
    char resp[REGISTER_MAX_TITLE_ID + REGISTER_MAX_TITLE_NAME * 2 + 128];
    int used_nullfs = 0;
    const char *err = NULL;
    (void)body_len;
    (void)src_path; (void)title_id; (void)title_name; (void)title_name_esc;
    (void)resp; (void)used_nullfs; (void)err; (void)request_body;
    if (!state) return -1;
    /* Gated off — Sony's sceAppInstUtilAppInstallTitleDir wedges the
     * payload from a standalone userland context on firmware 9.60.
     * Confirmed by hardware testing 2026-04-19: the thread enters
     * Sony's kernel stub and never returns, regardless of how we
     * resolve the symbol (link-time, dlopen + dlsym, or authid-
     * elevated). The caller-identity check is process-based, not
     * credential-based. Users install titles via PS5-side tools
     * (ShadowMountPlus, Itemzflow, etaHEN) which inject into
     * SceShellUI where those APIs behave normally. */
    {
        static const char e[] = "install_disabled_use_external_tool";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          e, (uint64_t)(sizeof(e) - 1));
    }
    if (request_body) {
        extract_json_string_field(request_body, "src_path",
                                  src_path, sizeof(src_path));
    }
    if (src_path[0] == '\0') {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "register_src_path_missing", 25);
    }
    if (!is_path_allowed(src_path)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "register_src_path_not_allowed", 29);
    }
    if (register_title_from_path(src_path, title_id, title_name,
                                  &used_nullfs, &err) != 0) {
        const char *reason = err ? err : "register_failed";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, (uint64_t)strlen(reason));
    }
    json_escape_into(title_name, title_name_esc, sizeof(title_name_esc));
    int n = snprintf(resp, sizeof(resp),
                     "{\"title_id\":\"%s\",\"title_name\":\"%s\","
                     "\"used_nullfs\":%s}",
                     title_id, title_name_esc,
                     used_nullfs ? "true" : "false");
    if (n < 0 || (size_t)n >= sizeof(resp)) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "register_response_overflow", 26);
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_APP_REGISTER_ACK, 0, trace_id,
                      resp, (uint64_t)n);
}

static int handle_app_unregister(runtime_state_t *state, int client_fd,
                                  uint64_t trace_id, const char *request_body,
                                  uint64_t body_len) {
    char title_id[REGISTER_MAX_TITLE_ID] = {0};
    const char *err = NULL;
    (void)body_len;
    (void)title_id; (void)err; (void)request_body;
    if (!state) return -1;
    /* Gated off — same Sony kernel-stub wedge as APP_REGISTER. */
    {
        static const char e[] = "uninstall_disabled_use_external_tool";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          e, (uint64_t)(sizeof(e) - 1));
    }
    if (request_body) {
        extract_json_string_field(request_body, "title_id",
                                  title_id, sizeof(title_id));
    }
    if (title_id[0] == '\0') {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "unregister_title_id_missing", 27);
    }
    if (unregister_title(title_id, &err) != 0) {
        const char *reason = err ? err : "unregister_failed";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_APP_UNREGISTER_ACK, 0,
                      trace_id, NULL, 0);
}

static int handle_app_launch(runtime_state_t *state, int client_fd,
                              uint64_t trace_id, const char *request_body,
                              uint64_t body_len) {
    char title_id[REGISTER_MAX_TITLE_ID] = {0};
    const char *err = NULL;
    (void)body_len;
    (void)title_id; (void)err; (void)request_body;
    if (!state) return -1;
    /* Gated off — sceLncUtilLaunchApp AND sceSystemServiceLaunchApp
     * both wedge from our process on firmware 9.60 (hardware-verified
     * 2026-04-19). Users launch titles from XMB directly, or via a
     * PS5-side launcher (etaHEN, Itemzflow, ShadowMountPlus). */
    {
        static const char e[] = "launch_disabled_use_xmb_directly";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          e, (uint64_t)(sizeof(e) - 1));
    }
    if (request_body) {
        extract_json_string_field(request_body, "title_id",
                                  title_id, sizeof(title_id));
    }
    if (title_id[0] == '\0') {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "launch_title_id_missing", 23);
    }
    if (launch_title(title_id, &err) != 0) {
        const char *reason = err ? err : "launch_failed";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_APP_LAUNCH_ACK, 0,
                      trace_id, NULL, 0);
}

static int handle_app_list_registered(runtime_state_t *state, int client_fd,
                                       uint64_t trace_id,
                                       const char *request_body,
                                       uint64_t body_len) {
    /* Buffer sized for a max-library PS5 with generous headroom. Each
     * entry is ~135 bytes after we dropped the title_name echo
     * ({"title_id":"PPSA00xxx","title_name":"PPSA00xxx","src":"/mnt/.../game","image_backed":false},
     * conservative estimate). 512 KiB holds ~3800 entries -- well past
     * Sony's own UI limit (~1.5k titles in practice) with room to grow
     * if a user has a fragmented app.db full of stale registrations. */
    const size_t cap = 512u * 1024u;
    char *buf = NULL;
    size_t written = 0;
    const char *err = NULL;
    int rc;
    (void)request_body;
    (void)body_len;
    if (!state) return -1;
    buf = (char *)malloc(cap);
    if (!buf) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "list_registered_oom", 19);
    }
    if (list_registered_titles_json(buf, cap, &written, &err) != 0) {
        const char *reason = err ? err : "list_registered_failed";
        rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                        reason, (uint64_t)strlen(reason));
        free(buf);
        return rc;
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_APP_LIST_REGISTERED_ACK, 0,
                    trace_id, buf, (uint64_t)written);
    free(buf);
    return rc;
}

/* ── Hardware monitoring handlers (thin wrappers over hw_info.c) ──────── */

static int handle_hw_text_op(runtime_state_t *state, int client_fd,
                              uint64_t trace_id,
                              int (*getter)(char *, size_t, size_t *, const char **),
                              uint16_t ack_type, const char *default_err) {
    char body[2048];
    size_t written = 0;
    const char *err = NULL;
    if (!state) return -1;
    if (getter(body, sizeof(body), &written, &err) != 0) {
        const char *reason = err ? err : default_err;
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, ack_type, 0, trace_id, body, (uint64_t)written);
}

static int handle_hw_info(runtime_state_t *state, int client_fd, uint64_t trace_id) {
    return handle_hw_text_op(state, client_fd, trace_id, hw_info_get_text,
                              FTX2_FRAME_HW_INFO_ACK, "hw_info_failed");
}

static int handle_hw_temps(runtime_state_t *state, int client_fd, uint64_t trace_id) {
    return handle_hw_text_op(state, client_fd, trace_id, hw_temps_get_text,
                              FTX2_FRAME_HW_TEMPS_ACK, "hw_temps_failed");
}

static int handle_hw_power(runtime_state_t *state, int client_fd, uint64_t trace_id) {
    return handle_hw_text_op(state, client_fd, trace_id, hw_power_get_text,
                              FTX2_FRAME_HW_POWER_ACK, "hw_power_failed");
}

/* Parse body as "NN" (ASCII decimal). Accepts an empty-body shortcut
 * meaning "reset to default 65 °C" so future UI can send a zero-body
 * frame as a quick reset. Leading whitespace and a trailing newline
 * are tolerated because some shells (lab CLI, curl) add them. */
static int handle_hw_set_fan_threshold(runtime_state_t *state, int client_fd,
                                        uint64_t trace_id,
                                        const char *body, uint64_t body_len) {
    if (!state) return -1;

    uint8_t threshold = 65;  /* Sony's approximate default. */
    if (body_len > 0 && body_len < 16) {
        char buf[16];
        memcpy(buf, body, (size_t)body_len);
        buf[body_len] = '\0';
        /* atoi is fine here — we only trust it for extracting the
         * numeric portion; hw_fan_set_threshold clamps the result,
         * so a non-numeric payload just degrades to 45 °C. */
        int parsed = atoi(buf);
        if (parsed > 0 && parsed < 255) {
            threshold = (uint8_t)parsed;
        }
    } else if (body_len >= 16) {
        static const char err[] = "body_too_long";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          err, (uint64_t)(sizeof(err) - 1));
    }

    const char *err_reason = NULL;
    if (hw_fan_set_threshold(threshold, &err_reason) != 0) {
        const char *reason = err_reason ? err_reason : "fan_set_failed";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          reason, (uint64_t)strlen(reason));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_HW_SET_FAN_THRESHOLD_ACK, 0, trace_id,
                      NULL, 0);
}

/* APP_LAUNCH_BROWSER: open the PS5 web browser. Implementation uses
 * sceSystemServiceLaunchApp with the known-stable NPXS browser title
 * id. Resolved via the same libSceSystemService handle the launch path
 * already uses. */
extern void *register_browser_launch_handle(void);  /* defined in register.c */
extern int   register_browser_launch(void);

static int handle_app_launch_browser(runtime_state_t *state, int client_fd,
                                      uint64_t trace_id) {
    if (!state) return -1;
    if (register_browser_launch() != 0) {
        static const char err[] = "launch_browser_unavailable";
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          err, (uint64_t)(sizeof(err) - 1));
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    return send_frame(client_fd, FTX2_FRAME_APP_LAUNCH_BROWSER_ACK, 0,
                      trace_id, NULL, 0);
}

/* PROC_LIST: walk allproc via kernel R/W and return a JSON array of
 * running processes. Read-only — we never touch process state here.
 * The body size is bounded so a system with a corrupt proc list can't
 * blow the buffer; see proc_list.c for the truncation policy. */
static int handle_proc_list(runtime_state_t *state, int client_fd,
                             uint64_t trace_id,
                             const char *request_body,
                             uint64_t body_len) {
    /* 64 KiB holds ~600 entries after JSON overhead; real PS5 process
     * counts sit in the 60–120 range. Generous-but-not-absurd cap. */
    const size_t cap = 64u * 1024u;
    char *buf = NULL;
    size_t written = 0;
    const char *err = NULL;
    int rc;
    (void)request_body;
    (void)body_len;
    if (!state) return -1;
    buf = (char *)malloc(cap);
    if (!buf) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                          "proc_list_oom", 13);
    }
    if (proc_list_get_json(buf, cap, &written, &err) != 0) {
        const char *reason = err ? err : "proc_list_failed";
        rc = send_frame(client_fd, FTX2_FRAME_ERROR, 0, trace_id,
                        reason, (uint64_t)strlen(reason));
        free(buf);
        return rc;
    }
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);
    rc = send_frame(client_fd, FTX2_FRAME_PROC_LIST_ACK, 0, trace_id,
                    buf, (uint64_t)written);
    free(buf);
    return rc;
}

/* ── Binary frame dispatcher ─────────────────────────────────────────────────── */

/* Frame-type → port classification. Transfer frames carry bulk data and
 * serialize a single client on port 9113; management frames are small,
 * responsive, and live on port 9114 in their own pthread. Keeping the
 * split strict (ERROR+close on a mismatch) prevents a badly-written
 * client from STREAM_SHARDing onto the mgmt port and DoS'ing it. */
static int is_transfer_frame_type(uint16_t t) {
    return t == FTX2_FRAME_BEGIN_TX
        || t == FTX2_FRAME_STREAM_SHARD
        || t == FTX2_FRAME_COMMIT_TX
        || t == FTX2_FRAME_ABORT_TX;
}

/* `tx_ctx` is NULL on the mgmt port (no transfer frames flow there). On
 * the transfer port, pass a per-connection `conn_tx_ctx_t` so the
 * accept-loop wrapper can mark an unfinished tx as interrupted when the
 * socket closes without seeing COMMIT/ABORT. */
static int handle_binary_frame(runtime_state_t *state, int client_fd,
                               int is_transfer_port,
                               conn_tx_ctx_t *tx_ctx) {
    unsigned char hdr_bytes[FTX2_HEADER_LEN];
    char body[2048]; /* large enough for QUERY_TX: outer JSON (~100B) + embedded record (~512B) */
    char request_body[1024];
    ftx2_header_t hdr;
    ftx2_tx_meta_t meta;
    const char *extra = NULL;
    uint64_t extra_len = 0;

    if (!state) return -1;
    if (recv_exact(client_fd, hdr_bytes, sizeof(hdr_bytes)) != 0) return -1;

    hdr.magic      = read_le32(hdr_bytes + 0);
    hdr.version    = read_le16(hdr_bytes + 4);
    hdr.frame_type = read_le16(hdr_bytes + 6);
    hdr.flags      = read_le32(hdr_bytes + 8);
    hdr.body_len   = read_le64(hdr_bytes + 12);
    hdr.trace_id   = read_le64(hdr_bytes + 20);

    if (hdr.magic != FTX2_MAGIC) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id, "bad_magic", 9);
    }
    if (hdr.version != FTX2_VERSION) {
        return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id, "bad_version", 11);
    }

    /* Reject wrong-port frames. We can't safely process them: a misrouted
     * STREAM_SHARD would carry up to 32 MiB we'd otherwise drain, and
     * processing a BEGIN_TX on the mgmt port would corrupt the transfer
     * thread's tx table. Emit ERROR and close so the client reconnects
     * to the correct port. */
    if (is_transfer_frame_type(hdr.frame_type) != !!is_transfer_port) {
        (void)send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                         "wrong_port", 10);
        return -1;
    }

    /*
     * STREAM_SHARD bodies can be up to 32 MiB — dispatch before the
     * 1024-byte body buffer guard; the shard handler reads its own body.
     */
    if (hdr.frame_type == FTX2_FRAME_STREAM_SHARD) {
        return handle_stream_shard(state, client_fd, hdr.trace_id, hdr.body_len);
    }

    /*
     * BEGIN_TX carries a manifest JSON that grows linearly with file count.
     * Heap-allocate the body so large directories don't hit the stack guard.
     */
    if (hdr.frame_type == FTX2_FRAME_BEGIN_TX) {
        char resp[2048];
        char *begin_body = NULL;
        uint64_t begin_body_len = hdr.body_len;
        const char *bextra = NULL;
        uint64_t bextra_len = 0;
        ftx2_tx_meta_t bmeta;
        runtime_tx_entry_t *entry = NULL;
        int ret;
        int len;

        if (begin_body_len > 0) {
            begin_body = (char *)malloc((size_t)begin_body_len + 1);
            if (!begin_body) {
                /* drain and report OOM */
                uint64_t rem = begin_body_len;
                char discard[256];
                while (rem > 0) {
                    size_t take = rem > sizeof(discard) ? sizeof(discard) : (size_t)rem;
                    if (recv_exact(client_fd, discard, take) != 0) return -1;
                    rem -= (uint64_t)take;
                }
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                                  "out_of_memory", 13);
            }
            if (recv_exact(client_fd, begin_body, (size_t)begin_body_len) != 0) {
                free(begin_body);
                return -1;
            }
            begin_body[begin_body_len] = '\0';
        }

        if (parse_tx_meta(begin_body ? begin_body : "", begin_body_len,
                          &bmeta, &bextra, &bextra_len) != 0) {
            free(begin_body);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "invalid_tx_meta", 15);
        }
        /* Resolve the tx entry against the journal, picking one of three
         * outcomes per (has-existing-entry) × (client-wants-resume):
         *
         *   existing + resume-flag:  RESUME — adopt, preserve shards_received,
         *                            tmp file, and manifest heap. (The entry
         *                            state may be either "interrupted" (clean
         *                            drop, payload saw it) or "active" (TCP
         *                            drop, payload's recv returned -1 and we
         *                            marked it interrupted in the connection
         *                            cleanup OR we didn't get a chance to yet
         *                            — both imply the client's partial data is
         *                            still on disk.)
         *
         *   existing + no-resume:    RESTART — client explicitly asked for a
         *                            fresh tx. Drop tmp, reset counters. This
         *                            catches "I cancelled and want to start
         *                            over" where the client intentionally
         *                            reuses the tx_id.
         *
         *   no existing:             FRESH — normal new tx allocation.
         *
         * `is_resume` drives whether later code in this handler re-runs
         * destructive side-effects: unlinking kind=1 tmp files and
         * re-parsing the kind=2 manifest index. Both must be skipped on
         * resume — they'd destroy the very state we're trying to adopt. */
        int is_resume = 0;
        int want_resume = (bmeta.flags & FTX2_TX_FLAG_RESUME) != 0;
        pthread_mutex_lock(&state->state_mtx);
        {
            runtime_tx_entry_t *existing = runtime_find_tx_entry(state, bmeta.tx_id);
            if (existing && want_resume) {
                /* Adopt. If state is "interrupted" we're coming back from
                 * a paused tx — the mark-interrupted path decremented
                 * `active_transactions` on pause, so bump it back now
                 * so the matching commit/abort decrement balances. If
                 * state is already "active" (TCP drop raced the first
                 * reconnect before mark-interrupted could fire), the
                 * counter already reflects this tx — don't double-count. */
                int was_interrupted = strcmp(existing->state, "interrupted") == 0;
                snprintf(existing->state, sizeof(existing->state), "active");
                if (was_interrupted) {
                    state->active_transactions += 1;
                }
                entry = existing;
                is_resume = 1;
            } else if (existing && !want_resume) {
                /* Explicit restart with same tx_id — common when the user
                 * cancels an upload and re-clicks Override. Drop partial
                 * state and reuse the slot. Counter accounting mirrors
                 * the want_resume branch: if the prior state was
                 * "interrupted" we're re-activating, so bump. If it was
                 * still "active" the counter already reflects this tx
                 * (TCP drop raced, mark-interrupted didn't fire). */
                int was_interrupted = strcmp(existing->state, "interrupted") == 0;
                entry = existing;
                runtime_release_tx_resources(entry); /* unlinks tmp */
                entry->shards_received = 0;
                entry->bytes_received  = 0;
                entry->total_shards    = 0;
                entry->total_bytes     = 0;
                entry->file_count      = 0;
                entry->direct_mode     = 0;
                entry->tmp_path[0]     = '\0';
                entry->dest_root[0]    = '\0';
                snprintf(entry->state, sizeof(entry->state), "active");
                if (was_interrupted) {
                    state->active_transactions += 1;
                }
                is_resume = 0;
            } else {
                /* Fresh allocation. */
                state->active_transactions += 1;
                state->last_tx_seq += 1;
                entry = runtime_alloc_tx_entry(state, bmeta.tx_id, state->last_tx_seq);
                is_resume = 0;
            }
        }
        pthread_mutex_unlock(&state->state_mtx);
        if (!entry) {
            free(begin_body);
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "tx_table_full", 13);
        }
        /* Skip manifest/metadata extraction on resume — the entry already
         * carries the correct values from the original BEGIN_TX (either
         * still in memory, or reloaded from the journal on payload start).
         * Re-extracting would be idempotent if the client re-sends the
         * exact same body, but it would overwrite our dest_root/total_shards
         * with whatever the client decides to send, and there's no upside. */
        if (!is_resume && bextra && bextra_len > 0) {
            extract_json_string_field(bextra, "dest_root",
                                      entry->dest_root, sizeof(entry->dest_root));
            entry->total_shards = extract_json_uint64_field(bextra, "total_shards");
            entry->total_bytes  = extract_json_uint64_field(bextra, "total_bytes");
            entry->file_count   = extract_json_uint64_field(bextra, "file_count");
            if (entry->file_count == 0) entry->file_count = 1;
            (void)runtime_write_manifest(entry, bextra, (size_t)bextra_len);
        }
        /* Enable direct-write path:
         *   - single-file (kind=1, file_count<=1): stream straight to
         *     <dest>.ps5up2-tmp, rename at commit.
         *   - multi-file (kind=2, files[] populated): stream each shard to
         *     <file_path>.ps5up2-tmp using the manifest to route shard_seq
         *     to its file. Rename each tmp at commit.
         *
         * Shards arrive in sequence within a single connection so per-file
         * append ordering is guaranteed.
         *
         * On resume: direct_mode / tmp_path / manifest_blob / manifest_index
         * are already set from the original BEGIN_TX, so both branches
         * short-circuit. The unlink in the kind=1 branch and the manifest
         * rebuild in the kind=2 branch are the bugs the `is_resume` guard
         * is here to prevent — both would destroy the partial state that
         * shards_received is about to tell the client to skip past.
         */
        if (is_resume) {
            /* Intentionally nothing — entry is adopted as-is. */
        } else if (bmeta.kind == 1 && entry->file_count <= 1 && entry->dest_root[0]) {
            entry->direct_mode = 1;
            snprintf(entry->tmp_path, sizeof(entry->tmp_path),
                     "%s.ps5up2-tmp", entry->dest_root);
            if (ensure_parent_dir(entry->dest_root) != 0) {
                fprintf(stderr, "[payload2] direct: ensure_parent_dir failed: %s\n",
                        entry->dest_root);
                entry->direct_mode = 0;
            } else {
                /* Fresh tx: unlink any leftover tmp. Not reached on resume. */
                (void)unlink(entry->tmp_path);
            }
        } else if (bmeta.kind == 2 && entry->file_count > 0 &&
                   bextra && bextra_len > 0 &&
                   strstr(bextra, "\"files\":[")) {
            /* Multi-file direct mode: parse the manifest once into a heap
             * index so per-shard routing is O(log N), not O(N) per shard.
             * If blob exceeds the hard cap or the parse fails, we refuse
             * the transaction outright — the old code silently truncated a
             * multi-KiB manifest into an 8 KiB stack buffer per shard, which
             * corrupted any transfer with more than ~60 files. */
            if (bextra_len > PS5UPLOAD2_MAX_MANIFEST_BLOB) {
                runtime_abort_tx_fatal(state, entry);
                free(begin_body);
                return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                                  "manifest_too_large", 18);
            }
            {
                char *blob = (char *)malloc((size_t)bextra_len + 1);
                manifest_index_entry_t *idx = NULL;
                uint64_t idx_count = 0;
                if (!blob) {
                    runtime_abort_tx_fatal(state, entry);
                    free(begin_body);
                    return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                                      "out_of_memory", 13);
                }
                memcpy(blob, bextra, (size_t)bextra_len);
                blob[bextra_len] = '\0';
                if (build_manifest_index(blob, (size_t)bextra_len,
                                         entry->file_count, &idx, &idx_count) != 0) {
                    free(blob);
                    runtime_abort_tx_fatal(state, entry);
                    free(begin_body);
                    return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                                      "manifest_invalid", 16);
                }
                entry->manifest_blob         = blob;
                entry->manifest_blob_len     = (size_t)bextra_len;
                entry->manifest_index        = idx;
                entry->manifest_index_count  = idx_count;
                entry->direct_mode = 1;
            }
        }
        (void)runtime_save_tx_state(state);
        (void)runtime_append_tx_event(state, "begin_tx");
        (void)runtime_flush_tx_record(state, entry);
        free(begin_body);
        pthread_mutex_lock(&state->state_mtx);
        state->command_count += 1;
        pthread_mutex_unlock(&state->state_mtx);

        len = snprintf(resp, sizeof(resp),
                       "{\"accepted\":true,\"tx_id\":\"%s\",\"tx_seq\":%llu,"
                       "\"active_transactions\":%llu,"
                       "\"last_acked_shard\":%llu}",
                       entry->tx_id_hex,
                       (unsigned long long)entry->tx_seq,
                       (unsigned long long)state->active_transactions,
                       (unsigned long long)entry->shards_received);
        if (len < 0) return -1;
        /* Remember this tx on the connection so a socket close before
         * COMMIT/ABORT marks it "interrupted" rather than leaving an
         * "active" orphan in the journal. */
        if (tx_ctx) {
            memcpy(tx_ctx->tx_id, entry->tx_id, 16);
            tx_ctx->has_tx = 1;
        }
        ret = send_frame(client_fd, FTX2_FRAME_BEGIN_TX_ACK, 0, hdr.trace_id,
                         resp, (uint64_t)len);
        return ret;
    }

    /* Read the body for all other (small) frame types. */
    request_body[0] = '\0';
    if (hdr.body_len > 0) {
        if (hdr.body_len >= sizeof(request_body)) {
            uint64_t remaining = hdr.body_len;
            char discard[256];
            while (remaining > 0) {
                size_t take = remaining > sizeof(discard) ? sizeof(discard) : (size_t)remaining;
                if (recv_exact(client_fd, discard, take) != 0) return -1;
                remaining -= (uint64_t)take;
            }
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "body_too_large", 14);
        }
        if (recv_exact(client_fd, request_body, (size_t)hdr.body_len) != 0) return -1;
        request_body[hdr.body_len] = '\0';
    }

    /* command_count is a cross-thread counter (incremented on both the
     * transfer and mgmt listeners). The mutex window is a single add,
     * so contention is negligible and a torn read from STATUS is
     * prevented entirely. */
    pthread_mutex_lock(&state->state_mtx);
    state->command_count += 1;
    pthread_mutex_unlock(&state->state_mtx);

    /* ── HELLO ── */
    if (hdr.frame_type == FTX2_FRAME_HELLO) {
        int len = snprintf(body, sizeof(body),
                           "{\"version\":%u,\"instance_id\":%llu,\"runtime_port\":%d}",
                           FTX2_VERSION,
                           (unsigned long long)state->instance_id,
                           state->runtime_port);
        /* snprintf returns the length that *would* have been written
         * (excluding NUL). A return > sizeof(body) means truncation;
         * clamp so we don't ask send_frame to read past the buffer. */
        if (len < 0) return -1;
        if ((size_t)len >= sizeof(body)) len = (int)(sizeof(body) - 1);
        return send_frame(client_fd, FTX2_FRAME_HELLO_ACK, 0, hdr.trace_id,
                          body, (uint64_t)len);
    }

    /* ── STATUS ── */
    if (hdr.frame_type == FTX2_FRAME_STATUS) {
        /* Snapshot cross-thread fields under the mutex so a concurrent
         * transfer-side BEGIN/COMMIT/ABORT can't produce a mid-flight
         * read of active_transactions or last_tx_seq. Keep the lock
         * window to memory copies only — never hold across send_frame. */
        uint64_t snap_instance_id, snap_started_at, snap_command_count;
        uint64_t snap_active_tx, snap_last_seq, snap_recovered;
        int snap_shutdown, snap_startup_reason, snap_takeover_req, snap_port;
        int len;
        char kernel_version_raw[256];
        char kernel_version_esc[512];
        read_ps5_kernel_version(kernel_version_raw, sizeof(kernel_version_raw));
        json_escape_into(kernel_version_raw, kernel_version_esc, sizeof(kernel_version_esc));
        pthread_mutex_lock(&state->state_mtx);
        snap_instance_id    = state->instance_id;
        snap_port           = state->runtime_port;
        snap_shutdown       = state->shutdown_requested;
        snap_startup_reason = state->startup_reason;
        snap_takeover_req   = state->takeover_requested;
        snap_started_at     = state->started_at_unix;
        snap_command_count  = state->command_count;
        snap_active_tx      = state->active_transactions;
        snap_last_seq       = state->last_tx_seq;
        snap_recovered      = state->recovered_transactions;
        pthread_mutex_unlock(&state->state_mtx);
        len = snprintf(body, sizeof(body),
                       "{\"version\":\"%s\","
                       "\"ps5_kernel\":\"%s\","
                       "\"instance_id\":%llu,\"runtime_port\":%d,"
                       "\"shutdown\":%d,\"startup_reason\":%d,"
                       "\"takeover_requested\":%d,\"started_at_unix\":%llu,"
                       "\"command_count\":%llu,\"active_transactions\":%llu,"
                       "\"last_tx_seq\":%llu,\"recovered_transactions\":%llu}",
                       PS5UPLOAD2_VERSION,
                       kernel_version_esc,
                       (unsigned long long)snap_instance_id,
                       snap_port,
                       snap_shutdown,
                       snap_startup_reason,
                       snap_takeover_req,
                       (unsigned long long)snap_started_at,
                       (unsigned long long)snap_command_count,
                       (unsigned long long)snap_active_tx,
                       (unsigned long long)snap_last_seq,
                       (unsigned long long)snap_recovered);
        /* Truncation-safe: if the fields ever grow past `body`, clamp
         * rather than emit a body_len that drives send_frame to read
         * past the stack buffer. The JSON is still valid-on-arrival
         * only when len < sizeof(body); the engine will fail the
         * serde_json::from_slice on a truncated body and return 502
         * to the UI, which is the right failure mode. */
        if (len < 0) return -1;
        if ((size_t)len >= sizeof(body)) len = (int)(sizeof(body) - 1);
        return send_frame(client_fd, FTX2_FRAME_STATUS_ACK, 0, hdr.trace_id,
                          body, (uint64_t)len);
    }

    /* ── QUERY_TX ── */
    if (hdr.frame_type == FTX2_FRAME_QUERY_TX) {
        char record[1024];
        int len;
        int requested_specific = 0;
        int have_snapshot = 0;
        uint64_t snap_active = 0;
        uint64_t snap_last = 0;
        runtime_tx_entry_t snap_entry;
        memset(&snap_entry, 0, sizeof(snap_entry));

        /* Snapshot the state we need under the lock, then release the
         * lock before doing I/O. The transfer side takes this mutex on
         * every SHARD ACK — holding it across `runtime_read_tx_record`
         * (which does fopen+fread on a UFS-backed file that can stall
         * tens of milliseconds under pressure) was dropping shards
         * mid-flight because the writer thread blocked on the lock
         * past its recv timeout. */
        pthread_mutex_lock(&state->state_mtx);
        {
            runtime_tx_entry_t *entry = NULL;
            snap_active = state->active_transactions;
            snap_last   = state->last_tx_seq;
            if (parse_tx_meta(request_body, hdr.body_len, &meta, &extra, &extra_len) == 0) {
                requested_specific = 1;
                entry = runtime_find_tx_entry(state, meta.tx_id);
            }
            if (!entry && !requested_specific && snap_last > 0) {
                int i = 0;
                for (i = 0; i < PS5UPLOAD2_MAX_TX; i++) {
                    if (state->tx_entries[i].in_use &&
                        state->tx_entries[i].tx_seq == snap_last) {
                        entry = &state->tx_entries[i];
                        break;
                    }
                }
            }
            if (entry) {
                snap_entry = *entry;   /* copy-by-value while locked */
                have_snapshot = 1;
            }
        }
        pthread_mutex_unlock(&state->state_mtx);

        if (requested_specific && !have_snapshot) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "tx_not_found", 12);
        }

        /* File read now happens lock-free. If the entry was evicted
         * between snapshot + read we get a stale or missing record;
         * that's surfaced as `last_tx_record: null` which is what the
         * pre-existing code already did for that case. */
        if (have_snapshot && runtime_read_tx_record(&snap_entry, record, 1024) == 0) {
            len = snprintf(body, sizeof(body),
                           "{\"active_transactions\":%llu,\"last_tx_seq\":%llu,"
                           "\"tx_id\":\"%s\",\"state\":\"%s\","
                           "\"shards_received\":%llu,\"bytes_received\":%llu,"
                           "\"last_tx_record\":%s}",
                           (unsigned long long)snap_active,
                           (unsigned long long)snap_last,
                           snap_entry.tx_id_hex,
                           snap_entry.state,
                           (unsigned long long)snap_entry.shards_received,
                           (unsigned long long)snap_entry.bytes_received,
                           record);
        } else {
            len = snprintf(body, sizeof(body),
                           "{\"active_transactions\":%llu,\"last_tx_seq\":%llu,"
                           "\"last_tx_record\":null}",
                           (unsigned long long)snap_active,
                           (unsigned long long)snap_last);
        }
        if (len < 0) return -1;
        if ((size_t)len >= sizeof(body)) len = (int)(sizeof(body) - 1);
        return send_frame(client_fd, FTX2_FRAME_QUERY_TX_ACK, 0, hdr.trace_id,
                          body, (uint64_t)len);
    }

    /* ── COMMIT_TX ── */
    if (hdr.frame_type == FTX2_FRAME_COMMIT_TX) {
        int len;
        runtime_tx_entry_t *entry = NULL;
        if (parse_tx_meta(request_body, hdr.body_len, &meta, &extra, &extra_len) != 0) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "invalid_tx_meta", 15);
        }
        entry = runtime_find_tx_entry(state, meta.tx_id);
        if (!entry) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "tx_not_found", 12);
        }
        if (strcmp(entry->state, "active") != 0) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "tx_not_active", 13);
        }
        /* Verify all expected shards arrived (skip check if total_shards unknown). */
        if (entry->total_shards > 0 &&
            entry->shards_received < entry->total_shards) {
            int len = snprintf(body, sizeof(body),
                               "{\"error\":\"shards_incomplete\","
                               "\"shards_received\":%llu,\"total_shards\":%llu}",
                               (unsigned long long)entry->shards_received,
                               (unsigned long long)entry->total_shards);
            if (len < 0) return -1;
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              body, (uint64_t)len);
        }
        pthread_mutex_lock(&state->state_mtx);
        if (state->active_transactions > 0) state->active_transactions -= 1;
        pthread_mutex_unlock(&state->state_mtx);
        (void)runtime_set_tx_state(state, entry->tx_id, "committed");
        (void)runtime_save_tx_state(state);
        (void)runtime_append_tx_event(state, "commit_tx");
        /* Apply: direct-write tx just renames its tmp file(s); spool tx copies
         * shards into the destination as before. */
        {
            uint64_t ta0 = now_us();
            if (entry->direct_mode && entry->file_count <= 1) {
                /* Drain the persistent writer + close the fd before rename.
                 * direct_writer_finish also clears direct_fd / direct_writer,
                 * so the upcoming runtime_release_tx_resources call will not
                 * unlink our freshly-renamed dest_root. */
                int finish_rc = direct_writer_finish(entry);
                if (finish_rc != 0) {
                    fprintf(stderr, "[payload2] WARN: direct writer finish reported I/O error for tx %s\n",
                            entry->tx_id_hex);
                }
                (void)unlink(entry->dest_root);
                if (rename(entry->tmp_path, entry->dest_root) != 0) {
                    fprintf(stderr, "[payload2] WARN: rename %s -> %s failed errno=%d\n",
                            entry->tmp_path, entry->dest_root, errno);
                } else {
                    /* Clear tmp_path so the upcoming release_tx_resources
                     * doesn't re-unlink a path that no longer exists. */
                    entry->tmp_path[0] = '\0';
                    printf("[payload2] direct apply rename -> %s\n", entry->dest_root);
                }
            } else if (entry->direct_mode && entry->file_count > 1) {
                uint64_t fi = 0;
                int rename_failed = 0;
                const manifest_index_entry_t *idx =
                    (const manifest_index_entry_t *)entry->manifest_index;
                if (!idx || !entry->manifest_blob) {
                    fprintf(stderr, "[payload2] direct multi commit: missing manifest index for tx %s\n",
                            entry->tx_id_hex);
                    rename_failed = 1;
                } else {
                    for (fi = 0; fi < entry->manifest_index_count; fi++) {
                        char file_path[512];
                        char tmp_path[512 + 16];
                        size_t plen = idx[fi].path_len;
                        if (plen == 0 || plen >= sizeof(file_path)) continue;
                        memcpy(file_path, entry->manifest_blob + idx[fi].path_offset, plen);
                        file_path[plen] = '\0';
                        snprintf(tmp_path, sizeof(tmp_path), "%s.ps5up2-tmp", file_path);
                        /* Two legitimate layouts here:
                         *   - non-packed record:  data at tmp_path, rename -> file_path
                         *   - packed record:      data already at file_path, tmp absent
                         * `rename(tmp, dest)` atomically replaces dest on POSIX, so
                         * there is no need for the old defensive `unlink(file_path)`
                         * that this branch used to do — and removing it is what
                         * keeps packed-record content from being wiped. ENOENT on
                         * the rename means "packed path already landed the file at
                         * the destination" which is success, not failure. */
                        if (rename(tmp_path, file_path) != 0 && errno != ENOENT) {
                            fprintf(stderr, "[payload2] direct multi rename %s -> %s errno=%d\n",
                                    tmp_path, file_path, errno);
                            rename_failed = 1;
                        }
                    }
                }
                if (!rename_failed) {
                    printf("[payload2] direct multi apply: %llu files renamed\n",
                           (unsigned long long)entry->file_count);
                }
            } else if (runtime_apply_spool(entry) == 0) {
                (void)runtime_cleanup_spool(entry);
            } else {
                fprintf(stderr, "[payload2] WARN: apply failed for tx %s dest=%s — spool preserved\n",
                    entry->tx_id_hex, entry->dest_root);
            }
            entry->apply_us += (now_us() - ta0);
        }
        /* Manifest blob + index are no longer needed — release the heap
         * immediately rather than waiting for the slot to be evicted. */
        runtime_release_tx_resources(entry);
        fprintf(stderr, "[payload2] tx %s timing(us): recv=%llu write=%llu verify=%llu apply=%llu "
                        "bytes=%llu shards=%llu\n",
                entry->tx_id_hex,
                (unsigned long long)entry->recv_us,
                (unsigned long long)entry->write_us,
                (unsigned long long)entry->verify_us,
                (unsigned long long)entry->apply_us,
                (unsigned long long)entry->bytes_received,
                (unsigned long long)entry->shards_received);
        len = snprintf(body, sizeof(body),
                       "{\"committed\":true,\"tx_id\":\"%s\",\"tx_seq\":%llu,"
                       "\"shards_received\":%llu,\"bytes_received\":%llu,"
                       "\"dest_root\":\"%s\",\"active_transactions\":%llu,"
                       "\"timing_us\":{\"recv\":%llu,\"write_wait\":%llu,"
                       "\"verify\":%llu,\"apply\":%llu,"
                       "\"open\":%llu,\"join\":%llu,\"close\":%llu,"
                       "\"hash\":%llu,\"shard_fn\":%llu,"
                       "\"pack_records\":%llu,\"pack_unlink\":%llu,"
                       "\"pack_open\":%llu,\"pack_ftruncate\":%llu,"
                       "\"pack_write\":%llu,\"pack_close\":%llu},"
                       "\"sock_rcvbuf\":%d,\"listener_rcvbuf_asked\":%d,"
                       "\"listener_rcvbuf_actual\":%d,\"listener_sndbuf_actual\":%d,"
                       "\"max_rcvbuf_probed\":%d}",
                       entry->tx_id_hex,
                       (unsigned long long)entry->tx_seq,
                       (unsigned long long)entry->shards_received,
                       (unsigned long long)entry->bytes_received,
                       entry->dest_root,
                       (unsigned long long)state->active_transactions,
                       (unsigned long long)entry->recv_us,
                       (unsigned long long)entry->write_us,
                       (unsigned long long)entry->verify_us,
                       (unsigned long long)entry->apply_us,
                       (unsigned long long)entry->open_us,
                       (unsigned long long)entry->join_us,
                       (unsigned long long)entry->close_us,
                       (unsigned long long)entry->hash_us,
                       (unsigned long long)entry->shard_func_us,
                       (unsigned long long)entry->pack_records,
                       (unsigned long long)entry->pack_unlink_us,
                       (unsigned long long)entry->pack_open_us,
                       (unsigned long long)entry->pack_ftruncate_us,
                       (unsigned long long)entry->pack_write_us,
                       (unsigned long long)entry->pack_close_us,
                       state->last_client_rcvbuf,
                       state->listener_rcvbuf_asked,
                       state->listener_rcvbuf_actual,
                       state->listener_sndbuf_actual,
                       state->max_rcvbuf_probed);
        if (len < 0) return -1;
        /* Commit took the tx to a terminal state; clear the connection's
         * "has open tx" marker so a subsequent socket close doesn't
         * mis-mark it as interrupted. */
        if (tx_ctx && tx_ctx->has_tx &&
            memcmp(tx_ctx->tx_id, meta.tx_id, 16) == 0) {
            tx_ctx->has_tx = 0;
        }
        return send_frame(client_fd, FTX2_FRAME_COMMIT_TX_ACK, 0, hdr.trace_id,
                          body, (uint64_t)len);
    }

    /* ── ABORT_TX ── */
    if (hdr.frame_type == FTX2_FRAME_ABORT_TX) {
        int len;
        runtime_tx_entry_t *entry = NULL;
        if (parse_tx_meta(request_body, hdr.body_len, &meta, &extra, &extra_len) == 0) {
            entry = runtime_find_tx_entry(state, meta.tx_id);
        }
        if (!entry) {
            return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                              "tx_not_found", 12);
        }
        pthread_mutex_lock(&state->state_mtx);
        if (state->active_transactions > 0) state->active_transactions -= 1;
        pthread_mutex_unlock(&state->state_mtx);
        (void)runtime_set_tx_state(state, entry->tx_id, "aborted");
        (void)runtime_save_tx_state(state);
        (void)runtime_append_tx_event(state, "abort_tx");
        runtime_release_tx_resources(entry);
        len = snprintf(body, sizeof(body),
                       "{\"aborted\":true,\"tx_id\":\"%s\","
                       "\"active_transactions\":%llu}",
                       entry->tx_id_hex,
                       (unsigned long long)state->active_transactions);
        if (len < 0) return -1;
        /* Abort took the tx terminal — see the matching clear in COMMIT_TX
         * for why we do this. */
        if (tx_ctx && tx_ctx->has_tx &&
            memcmp(tx_ctx->tx_id, meta.tx_id, 16) == 0) {
            tx_ctx->has_tx = 0;
        }
        return send_frame(client_fd, FTX2_FRAME_ABORT_TX_ACK, 0, hdr.trace_id,
                          body, (uint64_t)len);
    }

    /* ── TAKEOVER_REQUEST ── */
    if (hdr.frame_type == FTX2_FRAME_TAKEOVER_REQUEST) {
        state->takeover_requested = 1;
        runtime_mark_active_transactions(state, "interrupted");
        state->shutdown_requested = 1;
        (void)runtime_append_tx_event(state, "takeover_request");
        return send_frame(client_fd, FTX2_FRAME_TAKEOVER_ACK, 0, hdr.trace_id, "{}", 2);
    }

    /* ── SHUTDOWN ── */
    if (hdr.frame_type == FTX2_FRAME_SHUTDOWN) {
        runtime_mark_active_transactions(state, "interrupted");
        state->shutdown_requested = 1;
        (void)runtime_append_tx_event(state, "shutdown");
        return send_frame(client_fd, FTX2_FRAME_SHUTDOWN_ACK, 0, hdr.trace_id, "{}", 2);
    }

    /* ── CLEANUP ── */
    if (hdr.frame_type == FTX2_FRAME_CLEANUP) {
        return handle_cleanup(state, client_fd, hdr.trace_id,
                              request_body, hdr.body_len);
    }

    /* ── FS_LIST_VOLUMES ── */
    if (hdr.frame_type == FTX2_FRAME_FS_LIST_VOLUMES) {
        return handle_fs_list_volumes(state, client_fd, hdr.trace_id);
    }

    /* ── FS_LIST_DIR ── */
    if (hdr.frame_type == FTX2_FRAME_FS_LIST_DIR) {
        return handle_fs_list_dir(state, client_fd, hdr.trace_id,
                                   request_body, hdr.body_len);
    }

    /* ── FS_HASH ── */
    if (hdr.frame_type == FTX2_FRAME_FS_HASH) {
        return handle_fs_hash(state, client_fd, hdr.trace_id,
                               request_body, hdr.body_len);
    }

    /* ── FS destructive ops ── */
    if (hdr.frame_type == FTX2_FRAME_FS_DELETE) {
        return handle_fs_delete(state, client_fd, hdr.trace_id,
                                 request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_MOVE) {
        return handle_fs_move(state, client_fd, hdr.trace_id,
                               request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_CHMOD) {
        return handle_fs_chmod(state, client_fd, hdr.trace_id,
                                request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_MKDIR) {
        return handle_fs_mkdir(state, client_fd, hdr.trace_id,
                                request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_READ) {
        return handle_fs_read(state, client_fd, hdr.trace_id,
                               request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_COPY) {
        return handle_fs_copy(state, client_fd, hdr.trace_id,
                               request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_MOUNT) {
        return handle_fs_mount(state, client_fd, hdr.trace_id,
                                request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_FS_UNMOUNT) {
        return handle_fs_unmount(state, client_fd, hdr.trace_id,
                                  request_body, hdr.body_len);
    }

    /* ── App lifecycle (register / launch / list) ── */
    if (hdr.frame_type == FTX2_FRAME_APP_REGISTER) {
        return handle_app_register(state, client_fd, hdr.trace_id,
                                    request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_APP_UNREGISTER) {
        return handle_app_unregister(state, client_fd, hdr.trace_id,
                                      request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_APP_LAUNCH) {
        return handle_app_launch(state, client_fd, hdr.trace_id,
                                  request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_APP_LIST_REGISTERED) {
        return handle_app_list_registered(state, client_fd, hdr.trace_id,
                                           request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_APP_LAUNCH_BROWSER) {
        return handle_app_launch_browser(state, client_fd, hdr.trace_id);
    }

    /* ── Hardware monitoring ── */
    if (hdr.frame_type == FTX2_FRAME_HW_INFO) {
        return handle_hw_info(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_HW_TEMPS) {
        return handle_hw_temps(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_HW_POWER) {
        return handle_hw_power(state, client_fd, hdr.trace_id);
    }
    if (hdr.frame_type == FTX2_FRAME_HW_SET_FAN_THRESHOLD) {
        return handle_hw_set_fan_threshold(state, client_fd, hdr.trace_id,
                                            request_body, hdr.body_len);
    }
    if (hdr.frame_type == FTX2_FRAME_PROC_LIST) {
        return handle_proc_list(state, client_fd, hdr.trace_id,
                                request_body, hdr.body_len);
    }

    return send_frame(client_fd, FTX2_FRAME_ERROR, 0, hdr.trace_id,
                      "unsupported_frame", 17);
}

/* ── Listener cleanup (callable from signal handlers) ────────────────────────── */

void runtime_cleanup_listener(runtime_state_t *state) {
    if (!state) return;
    if (state->listener_fd >= 0) {
        close(state->listener_fd);
        state->listener_fd = -1;
    }
    if (state->mgmt_listener_fd >= 0) {
        close(state->mgmt_listener_fd);
        state->mgmt_listener_fd = -1;
    }
}

/* ── Server loop helpers ─────────────────────────────────────────────────────── */

/* Create, configure, bind, and listen on a TCP port. Returns the fd on
 * success, -1 on failure. `port` must be in network-byte-order untouched
 * (we htons it internally). Shared between the transfer and management
 * listeners so we don't duplicate the RCVBUF/SNDBUF probing dance. */
static int create_listener(int port, int probe_max_rcvbuf, int *out_asked,
                           int *out_actual_rcv, int *out_actual_snd,
                           int *out_probed_max) {
    struct sockaddr_in addr;
    int bind_attempt = 0;
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        perror("[payload2] socket");
        return -1;
    }
    {
        int one = 1;
        int rcvbuf = PS5UPLOAD2_CLIENT_RCVBUF_BYTES;
        int sndbuf = PS5UPLOAD2_CLIENT_SNDBUF_BYTES;
        int actual = 0;
        int rc_rcv, rc_snd;
        socklen_t actual_len = sizeof(actual);
        (void)setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
        rc_rcv = setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &rcvbuf, sizeof(rcvbuf));
        rc_snd = setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &sndbuf, sizeof(sndbuf));
        (void)rc_rcv; (void)rc_snd;
        if (out_asked) *out_asked = rcvbuf;
        if (getsockopt(fd, SOL_SOCKET, SO_RCVBUF, &actual, &actual_len) == 0) {
            if (out_actual_rcv) *out_actual_rcv = actual;
        }
        if (getsockopt(fd, SOL_SOCKET, SO_SNDBUF, &actual, &actual_len) == 0) {
            if (out_actual_snd) *out_actual_snd = actual;
        }
        if (probe_max_rcvbuf && out_probed_max) {
            int probes[] = {64*1024, 128*1024, 256*1024, 512*1024,
                            1024*1024, 2*1024*1024, 4*1024*1024,
                            8*1024*1024, 16*1024*1024};
            size_t i;
            int probe_fd = socket(AF_INET, SOCK_STREAM, 0);
            if (probe_fd >= 0) {
                int best = 0;
                for (i = 0; i < sizeof(probes)/sizeof(probes[0]); i++) {
                    int v = probes[i];
                    int got = 0;
                    socklen_t glen = sizeof(got);
                    (void)setsockopt(probe_fd, SOL_SOCKET, SO_RCVBUF, &v, sizeof(v));
                    if (getsockopt(probe_fd, SOL_SOCKET, SO_RCVBUF, &got, &glen) == 0) {
                        if (got > best) best = got;
                    }
                }
                close(probe_fd);
                *out_probed_max = best;
            }
        }
    }
    memset(&addr, 0, sizeof(addr));
    addr.sin_family      = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    addr.sin_port        = htons((uint16_t)port);
    for (bind_attempt = 0; bind_attempt < 20; bind_attempt++) {
        if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) == 0) break;
        if (errno != EADDRINUSE) {
            perror("[payload2] bind");
            close(fd);
            return -1;
        }
        usleep(100000);
    }
    if (bind_attempt >= 20) {
        fprintf(stderr, "[payload2] bind timeout on port %d\n", port);
        close(fd);
        return -1;
    }
    if (listen(fd, 8) != 0) {
        perror("[payload2] listen");
        close(fd);
        return -1;
    }
    return fd;
}

/* Per-connection socket tuning applied to every accepted fd. The PS5
 * kernel caps per-socket SO_RCVBUF at ~512 KiB, so we do the sizing on
 * the accepted socket rather than the listener. */
static void tune_accepted_client(int client_fd, int *out_last_rcvbuf) {
    int one = 1;
    int rcvbuf = PS5UPLOAD2_CLIENT_RCVBUF_BYTES;
    int sndbuf = PS5UPLOAD2_CLIENT_SNDBUF_BYTES;
    int actual_rcvbuf = 0;
    socklen_t actual_len = sizeof(actual_rcvbuf);
    struct timeval idle_to;
    idle_to.tv_sec  = PS5UPLOAD2_CLIENT_IDLE_SEC;
    idle_to.tv_usec = 0;
    (void)setsockopt(client_fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
    (void)setsockopt(client_fd, SOL_SOCKET, SO_KEEPALIVE, &one, sizeof(one));
    /* SO_RCVTIMEO is the knob that converts a stalled client into an
     * explicit recv error (EAGAIN) rather than an indefinite block on
     * recv(). If it fails silently, a hung client pins this worker
     * thread forever — no diagnostic, no recovery. Log any failure
     * loudly so the dev log carries the signal even if other settings
     * silently fall back. */
    if (setsockopt(client_fd, SOL_SOCKET, SO_RCVTIMEO, &idle_to, sizeof(idle_to)) != 0) {
        fprintf(stderr,
                "[payload] setsockopt(SO_RCVTIMEO=%lds) failed on fd=%d: %s (errno=%d) — "
                "a stalled client will now block this worker indefinitely\n",
                (long)idle_to.tv_sec, client_fd, strerror(errno), errno);
    }
    (void)setsockopt(client_fd, SOL_SOCKET, SO_RCVBUF, &rcvbuf, sizeof(rcvbuf));
    (void)setsockopt(client_fd, SOL_SOCKET, SO_SNDBUF, &sndbuf, sizeof(sndbuf));
    if (out_last_rcvbuf &&
        getsockopt(client_fd, SOL_SOCKET, SO_RCVBUF,
                   &actual_rcvbuf, &actual_len) == 0) {
        *out_last_rcvbuf = actual_rcvbuf;
    }
}

/* ── Transfer server loop (main thread, port 9113) ───────────────────────────── */

int runtime_server_loop(runtime_state_t *state) {
    if (!state) return -1;

    state->listener_fd = create_listener(state->runtime_port,
                                         /*probe_max_rcvbuf=*/1,
                                         &state->listener_rcvbuf_asked,
                                         &state->listener_rcvbuf_actual,
                                         &state->listener_sndbuf_actual,
                                         &state->max_rcvbuf_probed);
    if (state->listener_fd < 0) return -1;

    printf("[payload2] transfer listener ready on port %d\n", state->runtime_port);

    while (!state->shutdown_requested) {
        int client_fd = accept(state->listener_fd, NULL, NULL);
        if (client_fd < 0) {
            if (errno == EINTR) continue;
            if (state->shutdown_requested) break;  /* listener closed from outside */
            perror("[payload2] accept");
            break;
        }
        tune_accepted_client(client_fd, &state->last_client_rcvbuf);
        /* Handle frames on this connection until the client closes it, an I/O
         * error occurs, or a frame handler signals shutdown. Track the
         * currently-open tx so we can mark it "interrupted" on drop — a
         * plain TCP disconnect doesn't run COMMIT/ABORT and would otherwise
         * leave an "active" orphan in the journal (causing the next
         * TX_FLAG_RESUME begin to miss the entry). */
        conn_tx_ctx_t tx_ctx = {0};
        while (!state->shutdown_requested) {
            if (handle_binary_frame(state, client_fd, /*is_transfer_port=*/1, &tx_ctx) != 0) break;
        }
        if (tx_ctx.has_tx) {
            runtime_mark_tx_interrupted_by_id(state, tx_ctx.tx_id);
        }
        close(client_fd);
    }

    if (state->listener_fd >= 0) {
        close(state->listener_fd);
        state->listener_fd = -1;
    }
    printf("[payload2] transfer loop exiting shutdown=%d takeover=%d commands=%llu\n",
           state->shutdown_requested,
           state->takeover_requested,
           (unsigned long long)state->command_count);
    return 0;
}

/* ── Management server loop (pthread, port 9114) ─────────────────────────────── */

/* Per-client handler spawned from the mgmt accept loop. Running each
 * client on its own pthread is critical: the mgmt port services
 * FS_LIST_DIR / FS_HASH / FS_READ which call into the kernel's VFS,
 * and those syscalls can legitimately block indefinitely on a stuck
 * USB mount, a dead NAS, or a filesystem in an error state. If the
 * accept loop stayed single-threaded, one stuck readdir would wedge
 * the entire mgmt port -- meaning TAKEOVER_REQUEST from a newly-sent
 * payload never gets handled and the user has to network-reset to
 * kill the zombie. Per-thread isolation contains the stall to the
 * one client that triggered it.
 *
 * The thread arg is a heap-allocated { state, client_fd } tuple; the
 * handler frees it on exit. Threads are detached so no joinable state
 * leaks during normal churn. */
typedef struct {
    runtime_state_t *state;
    int              client_fd;
} mgmt_client_ctx_t;

/* Cap on concurrent mgmt-handler threads. Without a cap, a deep
 * Library scan (thousands of FS_LIST_DIR calls over a populated PS5
 * filesystem) spawns enough detached pthreads that the payload
 * eventually exhausts PS5's thread/memory budget and crashes. With a
 * cap, excess clients fall through to inline handling in the accept
 * thread -- slower but never resource-exhausting. 8 is enough to
 * hide a few slow syscalls without wedging the whole mgmt port, and
 * well under any firmware's thread ceiling. */
#define PS5UPLOAD2_MAX_MGMT_THREADS 8
static pthread_mutex_t g_mgmt_count_mtx = PTHREAD_MUTEX_INITIALIZER;
static int g_mgmt_thread_count = 0;

static void mgmt_thread_count_inc(void) {
    pthread_mutex_lock(&g_mgmt_count_mtx);
    g_mgmt_thread_count += 1;
    pthread_mutex_unlock(&g_mgmt_count_mtx);
}

static void mgmt_thread_count_dec(void) {
    pthread_mutex_lock(&g_mgmt_count_mtx);
    if (g_mgmt_thread_count > 0) g_mgmt_thread_count -= 1;
    pthread_mutex_unlock(&g_mgmt_count_mtx);
}

static int mgmt_thread_can_spawn(void) {
    int can;
    pthread_mutex_lock(&g_mgmt_count_mtx);
    can = (g_mgmt_thread_count < PS5UPLOAD2_MAX_MGMT_THREADS);
    pthread_mutex_unlock(&g_mgmt_count_mtx);
    return can;
}

static void *mgmt_client_thread(void *arg) {
    mgmt_client_ctx_t *ctx = (mgmt_client_ctx_t *)arg;
    runtime_state_t *state = ctx->state;
    int client_fd = ctx->client_fd;
    free(ctx);

    while (state && !state->shutdown_requested) {
        /* Mgmt port handles no transfer frames; no tx context needed. */
        if (handle_binary_frame(state, client_fd, /*is_transfer_port=*/0, NULL) != 0) break;
    }
    close(client_fd);
    mgmt_thread_count_dec();
    return NULL;
}

void *runtime_mgmt_server_loop(void *state_ptr) {
    runtime_state_t *state = (runtime_state_t *)state_ptr;
    if (!state) return NULL;

    state->mgmt_listener_fd = create_listener(state->mgmt_port,
                                              /*probe_max_rcvbuf=*/0,
                                              NULL, NULL, NULL, NULL);
    if (state->mgmt_listener_fd < 0) {
        fprintf(stderr, "[payload2] mgmt listener failed on port %d\n",
                state->mgmt_port);
        return NULL;
    }

    printf("[payload2] mgmt listener ready on port %d\n", state->mgmt_port);

    while (!state->shutdown_requested) {
        int client_fd = accept(state->mgmt_listener_fd, NULL, NULL);
        if (client_fd < 0) {
            if (errno == EINTR) continue;
            /* When main() signals shutdown it closes mgmt_listener_fd from
             * the outside; accept() returns with EBADF and we exit cleanly. */
            if (state->shutdown_requested) break;
            perror("[payload2] mgmt accept");
            break;
        }
        tune_accepted_client(client_fd, NULL);

        /* Hand the client off to its own thread so a stuck fs syscall
         * can't block the accept loop -- BUT cap the concurrent
         * thread count. A deep Library scan walks thousands of
         * directories; without the cap, we spawn one pthread per
         * connection and eventually exhaust PS5's thread/memory
         * budget (verified: 15k requests crashed the payload). When
         * the cap is reached, we fall back to inline handling in
         * the accept thread -- sequential but stable. */
        int use_thread = mgmt_thread_can_spawn();

        if (use_thread) {
            mgmt_client_ctx_t *ctx = (mgmt_client_ctx_t *)malloc(sizeof(*ctx));
            if (!ctx) {
                fprintf(stderr, "[payload2] mgmt: oom allocating client ctx; closing\n");
                close(client_fd);
                continue;
            }
            ctx->state = state;
            ctx->client_fd = client_fd;
            mgmt_thread_count_inc();
            /* Detached-from-birth. Previous code was pthread_create + late
             * pthread_detach, which has a window where a fast-exiting
             * thread becomes a zombie between create and detach. After
             * many zombies accumulate, the kernel exhausts thread slots
             * and every subsequent handler hangs. This was observed in
             * stress tests where trivial (validation-only) handlers
             * hung after a few calls because they exited too fast for
             * the late detach to matter. */
            pthread_attr_t attr;
            pthread_attr_t *attr_p = NULL;
            if (pthread_attr_init(&attr) == 0) {
                if (pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED) == 0) {
                    attr_p = &attr;
                }
            }
            pthread_t tid;
            int rc = pthread_create(&tid, attr_p, mgmt_client_thread, ctx);
            if (attr_p) pthread_attr_destroy(attr_p);
            if (rc == 0) {
                /* Already detached; no pthread_detach call needed. */
            } else {
                /* Thread creation failed despite our cap allowing it
                 * (kernel refused for other reasons, e.g., EAGAIN).
                 * Roll back our counter and fall through to inline
                 * handling rather than drop the client. */
                mgmt_thread_count_dec();
                free(ctx);
                fprintf(stderr, "[payload2] mgmt: pthread_create failed (%d); inline fallback\n", rc);
                use_thread = 0;
            }
        }

        if (!use_thread) {
            /* Inline fallback: process this client's frames directly
             * in the accept thread. Slower for burst traffic but
             * guarantees we don't exhaust thread resources during a
             * deep Library scan. */
            while (!state->shutdown_requested) {
                if (handle_binary_frame(state, client_fd, /*is_transfer_port=*/0, NULL) != 0) break;
            }
            close(client_fd);
        }
    }

    if (state->mgmt_listener_fd >= 0) {
        close(state->mgmt_listener_fd);
        state->mgmt_listener_fd = -1;
    }
    printf("[payload2] mgmt loop exiting shutdown=%d\n", state->shutdown_requested);
    return NULL;
}
