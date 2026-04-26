#ifndef PS5UPLOAD2_RUNTIME_H
#define PS5UPLOAD2_RUNTIME_H

#include <stdint.h>
#include <stddef.h>
#include <pthread.h>

#define PS5UPLOAD2_MAX_TX 32

/* Hard upper bounds on the manifest we will accept at BEGIN_TX.
 * These cap PS5-heap usage for a pathological client manifest so a broken
 * request fails fast instead of OOMing the payload. Current sizing:
 *   - 128 MiB blob covers a 223k-file real-game manifest (~35 MiB) with
 *     ~3× headroom for any future per-entry enrichment.
 *   - 1M file entries × 32 B index = 32 MiB; same 223k × 4 headroom. */
#define PS5UPLOAD2_MAX_MANIFEST_BLOB   (128u * 1024u * 1024u)
#define PS5UPLOAD2_MAX_MANIFEST_FILES  (1u * 1000u * 1000u)

typedef struct {
    int in_use;
    unsigned char tx_id[16];
    char tx_id_hex[33];
    uint64_t tx_seq;
    char state[16];
    uint64_t shards_received;
    uint64_t bytes_received;
    /* manifest fields — populated from BEGIN_TX body */
    char dest_root[256];
    uint64_t file_count;    /* number of files in this transaction */
    uint64_t total_shards;  /* 0 = unknown */
    uint64_t total_bytes;   /* 0 = unknown */
    /* cumulative wall-clock microseconds per phase (for throughput tracing) */
    uint64_t recv_us;       /* recv() of shard body from socket */
    uint64_t write_us;      /* producer time blocked on writer slot */
    uint64_t verify_us;     /* BLAKE3 verify (streaming — always 0 now) */
    uint64_t apply_us;      /* COMMIT rename (direct) or spool-to-dest copy */
    uint64_t open_us;       /* open(2) + ftruncate per shard */
    uint64_t join_us;       /* pthread_join waiting for writer to drain */
    uint64_t close_us;      /* close(fd) per shard */
    uint64_t hash_us;       /* BLAKE3 hasher_update aggregate */
    uint64_t shard_func_us; /* total time inside runtime_write_shard_to_path */
    int      sock_rcvbuf;   /* kernel-actual SO_RCVBUF on accepted fd */
    /* direct-write optimisation: skip spool entirely and write straight to a
     * `.ps5up2-tmp` sibling of the destination, rename on commit. Eliminates
     * the spool-then-apply I/O amplification. */
    int direct_mode;        /* 1 = direct write to tmp + rename on commit */
    char tmp_path[512];     /* fully-qualified `.ps5up2-tmp` path (single-file) */
    /* Parsed manifest held in memory for the lifetime of an active multi-file
     * transaction. Built once at BEGIN_TX so per-shard routing is O(log N)
     * binary search, not O(N) JSON rescan. Both pointers are NULL outside an
     * active multi-file direct tx and always freed in lockstep via
     * runtime_release_tx_resources() on every terminal transition. */
    char  *manifest_blob;       /* heap copy of the manifest JSON, NUL-terminated */
    size_t manifest_blob_len;   /* blob length in bytes (excluding the NUL) */
    void  *manifest_index;      /* opaque manifest_index_entry_t * array */
    uint64_t manifest_index_count; /* number of entries in manifest_index */
    /* Cached last parent dir that ensure_parent_dir() succeeded on. Sibling
     * files in the same directory can skip the whole mkdir walk when their
     * dirname matches. For a 200k-file game dir this saves ~19k × depth × mkdir
     * EEXIST syscalls, a sizeable chunk of commit-time apply work. */
    char last_parent_dir[512];
    /* Cached FILE* for this tx's shard log. Opened lazily on first shard
     * so empty/aborted-at-BEGIN transactions don't leave stray log files.
     * Closed (and NULLed) in runtime_release_tx_resources on terminal
     * transition. `void*` to avoid pulling stdio into this header. */
    void *shard_log_fp;
    /* Persistent direct-write state (single-file direct mode only).
     * First STREAM_SHARD of a direct-mode single-file tx opens the tmp file
     * and (if total_bytes is large enough) spawns a long-lived writer
     * thread. Subsequent shards reuse both without any per-shard
     * open/close/join cost. COMMIT_TX drains the writer, closes the fd,
     * and renames tmp_path → dest_root. Any terminal transition
     * (ABORT / takeover / shutdown / fatal error) goes through
     * runtime_release_tx_resources which tears this state down and unlinks
     * the stale tmp so no zombie `.ps5up2-tmp` survives.
     *
     * `direct_fd_open` disambiguates fd==0 from "not yet opened" without
     * forcing us to memset-to-(-1). All other fields zero-init correctly.
     * `direct_writer` is opaque to this header (holds piped_writer_t +
     * pthread_t + double-buffer handles inside runtime.c). */
    int      direct_fd;             /* valid only when direct_fd_open==1 */
    int      direct_fd_open;        /* 1 = direct_fd refers to an open tmp */
    void    *direct_writer;         /* heap-owned writer handle (see runtime.c) */
    int      direct_slot;           /* next producer slot (0 or 1) across shards */
    /* Packed-shard worker pool state. Lazily created on the first packed
     * STREAM_SHARD, persists across subsequent packed shards in the same tx,
     * torn down through runtime_release_tx_resources on any terminal
     * transition. Timing fields are summed from all workers at teardown and
     * reported in COMMIT_TX_ACK so bench sweep output shows where a
     * many-small-file run is actually spending time. */
    void    *pack_pool;             /* opaque pack_worker_pool_t * (runtime.c) */
    uint64_t pack_records;          /* total packed records processed */
    uint64_t pack_unlink_us;        /* sum of per-worker unlink time */
    uint64_t pack_open_us;          /* sum of per-worker open time */
    uint64_t pack_ftruncate_us;     /* sum of per-worker ftruncate time */
    uint64_t pack_write_us;         /* sum of per-worker write_full time */
    uint64_t pack_close_us;         /* sum of per-worker close time */
    uint64_t pack_open_retries;     /* transient open() retries absorbed */
    uint64_t pack_write_retries;    /* transient write_full() retries absorbed */
} runtime_tx_entry_t;

typedef struct {
    uint64_t instance_id;
    int runtime_port;           /* transfer port (9113) */
    int mgmt_port;              /* management port (9114) */
    int listener_fd;            /* transfer listener */
    int mgmt_listener_fd;       /* management listener */
    pthread_t mgmt_thread;      /* thread running runtime_mgmt_server_loop */
    int mgmt_thread_started;    /* 1 if mgmt_thread is joinable */
    /* Mutex guarding cross-thread state reads/writes. The mgmt listener
     * lives in its own pthread (runtime_mgmt_server_loop) while the
     * transfer listener runs in the main thread; both touch the counters
     * below and tx_entries[]. Keep critical sections short — never hold
     * across socket I/O. */
    pthread_mutex_t state_mtx;
    int shutdown_requested;
    int startup_reason;
    int takeover_requested;
    uint64_t started_at_unix;
    uint64_t command_count;
    uint64_t active_transactions;
    uint64_t last_tx_seq;
    uint64_t recovered_transactions;
    char ownership_path[256];
    char tx_state_path[256];
    char tx_journal_path[256];
    runtime_tx_entry_t tx_entries[PS5UPLOAD2_MAX_TX];
    int last_client_rcvbuf;       /* kernel-actual SO_RCVBUF on most recent accepted fd */
    int listener_rcvbuf_asked;    /* value we passed to setsockopt on listener */
    int listener_rcvbuf_actual;   /* what getsockopt returned on listener */
    int listener_sndbuf_actual;
    int max_rcvbuf_probed;        /* largest SO_RCVBUF the kernel will honor */
} runtime_state_t;

#define PS5UPLOAD2_STARTUP_FRESH 1
#define PS5UPLOAD2_STARTUP_TAKEOVER 2

int runtime_init(runtime_state_t *state);
int runtime_write_ownership(const runtime_state_t *state);
int runtime_clear_ownership(const runtime_state_t *state);
int runtime_ensure_directories(void);
/* Post-startup cleanup: unmount `/mnt/ps5upload/` mounts whose backing
 * dev node is gone (orphans from a previous session). Called once at
 * startup from main.c, after runtime_init completes. Failure-tolerant —
 * any single reconcile error logs + continues so payload still starts. */
void runtime_reconcile_mounts(void);
int runtime_try_takeover(runtime_state_t *state);
int runtime_server_loop(runtime_state_t *state);
/* Management listener: started from main.c via pthread_create *before*
 * runtime_server_loop begins. Binds :9114 and handles all non-transfer
 * frames — HELLO, STATUS, FS_* ops, QUERY_TX, CLEANUP, TAKEOVER_REQUEST.
 * Exits when state->shutdown_requested transitions to 1. */
void *runtime_mgmt_server_loop(void *state_ptr);
void runtime_cleanup_listener(runtime_state_t *state);

#endif
